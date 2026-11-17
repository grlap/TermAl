// Owns how TermAl tells a Claude tool call the CLI refused from one that ran
// and failed. The CLI marks a refused call in the user frame's
// `tool_result_meta` (`non_execution_kind`); builds that send no such mark are
// recognised by the CLI's own refusal wording. Does not own what the
// transcript shows for a refusal it already recognises, or the launch mode
// (claude_native_denial.rs).
use super::*;

/// Runs one Bash call and its error result through the Claude event handler,
/// with `meta` as the user frame's `tool_result_meta` when given.
fn bash_result(id: &str, command: &str, detail: &str, meta: Option<Value>) -> TestRecorder {
    let mut recorder = TestRecorder::default();
    let mut turn = ClaudeTurnState::default();
    let mut session_id = None;
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}
            }]}
        }),
        &mut session_id,
        &mut turn,
        &mut recorder,
    )
    .unwrap();
    let mut frame = json!({
        "type": "user",
        "message": {"content": [{
            "type": "tool_result", "tool_use_id": id, "is_error": true, "content": detail
        }]},
        "tool_use_result": format!("Error: {detail}")
    });
    if let Some(meta) = meta {
        frame["tool_result_meta"] = meta;
    }
    handle_claude_event(&frame, &mut session_id, &mut turn, &mut recorder).unwrap();
    recorder
}

fn refused(recorder: &TestRecorder, id: &str) -> bool {
    recorder.abandoned_commands.iter().any(|key| key == id)
        && !recorder.command_exits.iter().any(|(key, _)| key == id)
        && recorder.approvals.is_empty()
}

/// What the recorder was told about each command, for failure messages.
fn trace(recorder: &TestRecorder) -> String {
    format!(
        "abandoned {:?}, exits {:?}, approvals {}",
        recorder.abandoned_commands,
        recorder.command_exits,
        recorder.approvals.len()
    )
}

fn ran(recorder: &TestRecorder, id: &str) -> bool {
    recorder.command_exits.iter().any(|(key, _)| key == id)
        && !recorder.abandoned_commands.iter().any(|key| key == id)
}

#[test]
fn a_deny_rule_refusal_marked_by_the_cli_is_not_a_run() {
    let detail = "Permission to use Bash with command cargo test has been denied.";
    let recorder = bash_result(
        "deny-rule",
        "cargo test",
        detail,
        Some(json!([{"id": "deny-rule", "non_execution_kind": "permission-rule"}])),
    );
    assert!(refused(&recorder, "deny-rule"), "{}", trace(&recorder));
}

#[test]
fn a_refusal_the_cli_marks_counts_whatever_its_wording() {
    // A hook can deny with its own text; the CLI's mark still says the call
    // never ran.
    for kind in ["permission-rule", "user-rejected", "automode-blocked"] {
        let recorder = bash_result(
            "hooked",
            "cargo test",
            "Blocked: this repository does not allow test runs here.",
            Some(json!([{"id": "hooked", "non_execution_kind": kind}])),
        );
        assert!(refused(&recorder, "hooked"), "{kind}: {}", trace(&recorder));
    }
}

#[test]
fn a_generic_denial_without_a_mark_is_recognised_by_its_wording() {
    for detail in [
        "Permission to use Bash with command cargo test has been denied.",
        "Permission to use Bash has been denied.",
        "Permission to use Bash with cargo test:* has been denied.",
        "Permission to use Bash with cargo test:* has been denied \u{2014} a listed rule denies it.",
        "Permission to use REPL has been denied: it runs code as Bash does, and a rule denies Bash.",
        "Permission to edit /repo/src/main.rs has been denied.",
        "Permission to read /repo/.env has been denied.",
        "Permission to use Bash denied by plugin guard: test runs are not allowed here",
        "Permission for this tool use was denied: it requires interactive approval",
        "Permission for this command was denied by a built-in Claude Code safety check",
        "Permission for this action was denied by the Claude Code auto mode classifier. \
         Reason: the command deletes files outside the task",
        "Permission for this action has been denied. Reason: blocked by managed policy",
        "<tool_use_error>Permission to use Bash has been denied.</tool_use_error>",
    ] {
        let recorder = bash_result("unmarked", "cargo test", detail, None);
        assert!(
            refused(&recorder, "unmarked"),
            "{detail}: {}",
            trace(&recorder)
        );
    }
}

#[test]
fn a_mark_with_only_a_permission_decision_falls_back_to_the_wording() {
    // A subagent's entry may carry only permission_decision; whether the call
    // ran is then not stated, so the refusal wording decides.
    let meta =
        json!([{"id": "sub", "permission_decision": {"decision": "reject", "source": "rule"}}]);
    let denied = bash_result(
        "sub",
        "cargo test",
        "Permission to use Bash with command cargo test has been denied.",
        Some(meta.clone()),
    );
    assert!(refused(&denied, "sub"), "{}", trace(&denied));
    let failed = bash_result(
        "sub",
        "cargo test",
        "Exit code 101\nerror: test failed",
        Some(meta),
    );
    assert!(ran(&failed, "sub"), "{}", trace(&failed));
}

#[test]
fn a_command_that_ran_and_hit_an_os_permission_error_is_still_a_run() {
    let recorder = bash_result(
        "os-denied",
        "ls /root",
        "Exit code 2\nls: cannot open directory '/root': Permission denied",
        None,
    );
    assert!(ran(&recorder, "os-denied"), "{}", trace(&recorder));
}

#[test]
fn a_command_the_cli_sandbox_blocked_is_still_a_run() {
    // The sandbox does not refuse the call: the command runs, the OS refuses
    // what it tried, and the CLI appends a `<sandbox_violations>` block to
    // its output.
    for (command, detail) in [
        (
            "touch /etc/termal",
            "Exit code 1\ntouch: cannot touch '/etc/termal': Read-only file system\n\
             <sandbox_violations>\ndeny file-write-create /etc/termal\n</sandbox_violations>",
        ),
        (
            "curl https://example.com",
            "Exit code 7\ncurl: (7) Failed to connect: Operation not permitted\n\
             <sandbox_violations>\ndeny network-outbound example.com:443 (not allowed)\n\
             </sandbox_violations>",
        ),
    ] {
        let recorder = bash_result("sandboxed", command, detail, None);
        assert!(
            ran(&recorder, "sandboxed"),
            "{command}: {}",
            trace(&recorder)
        );
    }
}

#[test]
fn interrupted_and_cancelled_calls_keep_their_own_path() {
    // A kind the CLI states decides on its own, even beside refusal wording.
    for detail in [
        "[Request interrupted by user for tool use]",
        "Permission to use Bash with command cargo test has been denied.",
    ] {
        for kind in [
            "interrupted",
            "cancelled",
            "a-kind-this-build-does-not-know",
        ] {
            let recorder = bash_result(
                "stopped",
                "cargo test",
                detail,
                Some(json!([{"id": "stopped", "non_execution_kind": kind}])),
            );
            assert!(
                !recorder
                    .abandoned_commands
                    .iter()
                    .any(|key| key == "stopped"),
                "{kind} / {detail}: {}",
                trace(&recorder)
            );
        }
    }
}

/// Refuses one `tool` call carrying a refusal that names a 50k-character
/// path, and returns the refusal's length and the reason the session keeps.
fn refused_tool_reason(tool: &str, input: Value, refusal_start: &str) -> (usize, String) {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn = ClaudeTurnState::default();
    let mut external = None;
    let path = format!("/tmp/{}", "deep/".repeat(10_000));
    let detail = format!("{refusal_start}{path} has been denied.");
    let mut input = input;
    input["file_path"] = json!(path);
    for frame in [
        json!({"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": "refused", "name": tool, "input": input
        }]}}),
        json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result", "tool_use_id": "refused", "is_error": true,
                "content": detail
            }]},
            "tool_result_meta": [{"id": "refused", "non_execution_kind": "permission-rule"}]
        }),
    ] {
        handle_claude_event(&frame, &mut external, &mut turn, &mut recorder).unwrap();
    }
    let inner = state.inner.lock().unwrap();
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .unwrap();
    let reason = record
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Text { text, .. } if text.contains(refusal_start) => Some(text.clone()),
            Message::SubagentResult { summary, .. } if summary.contains(refusal_start) => {
                Some(summary.clone())
            }
            _ => None,
        })
        .expect("the refusal says why on the session");
    (detail.chars().count(), reason)
}

#[test]
fn a_refused_task_keeps_a_bounded_reason_on_the_session() {
    let (refusal, reason) = refused_tool_reason(
        "Task",
        json!({"description": "Survey the repository", "subagent_type": "general-purpose"}),
        "Permission to use Task with ",
    );
    assert!(
        reason.chars().count() < refusal / 4,
        "a {refusal}-character refusal is stored at {} characters",
        reason.chars().count()
    );
}

#[test]
fn a_refused_question_keeps_a_bounded_reason_on_the_session() {
    let (refusal, reason) = refused_tool_reason(
        "AskUserQuestion",
        json!({"questions": []}),
        "Permission to use AskUserQuestion with ",
    );
    assert!(
        reason.chars().count() < refusal / 4,
        "a {refusal}-character refusal is stored at {} characters",
        reason.chars().count()
    );
}

#[test]
fn a_refused_edit_keeps_a_bounded_reason_on_the_session() {
    let (refusal, reason) = refused_tool_reason(
        "Edit",
        json!({"old_string": "a", "new_string": "b"}),
        "Permission to edit ",
    );
    assert!(
        reason.chars().count() < refusal / 4,
        "a {refusal}-character refusal is stored at {} characters",
        reason.chars().count()
    );
}

#[test]
fn a_refused_read_keeps_a_bounded_reason_on_the_session() {
    let (refusal, reason) = refused_tool_reason("Read", json!({}), "Permission to read ");
    assert!(
        reason.chars().count() < refusal / 4,
        "a {refusal}-character refusal is stored at {} characters",
        reason.chars().count()
    );
}
