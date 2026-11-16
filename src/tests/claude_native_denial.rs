// Owns how an auto-approve Claude session meets a tool the Claude CLI refuses
// on its own: the launch keeps the CLI asking TermAl, and a refusal the CLI
// still makes ends as a readable error instead of an approval card nothing
// can answer. Does not own which results count as refusals
// (claude_refusal_signal.rs), the live can_use_tool decisions (claude.rs) or
// the read-only spawn overlay (claude_permission_boundary.rs).
use super::*;

#[test]
fn claude_launch_and_runtime_select_the_same_cli_permission_mode() {
    // An unpinned launch inherits whatever default mode the user, project or
    // local settings name, so a mode such as dontAsk could refuse a tool
    // before TermAl is asked. Ask keeps the user's own interactive choice.
    for mode in [
        ClaudeApprovalMode::AutoApprove,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        ClaudeApprovalMode::Plan,
    ] {
        for resume in [None, Some("existing-session")] {
            let args = claude_cli_persistent_args("opus", mode, ClaudeEffortLevel::Default, resume);
            let launched = args
                .windows(2)
                .filter(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1].as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                launched,
                [mode.session_cli_permission_mode()],
                "{mode:?} launches in the mode a runtime switch selects"
            );
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--permission-prompt-tool", "stdio"]),
                "{mode:?} still routes the CLI's questions to TermAl"
            );
        }
    }
    let auto = claude_cli_persistent_args(
        "opus",
        ClaudeApprovalMode::AutoApprove,
        ClaudeEffortLevel::Default,
        None,
    );
    assert!(
        !auto.iter().any(|arg| arg == "--settings"),
        "auto-approve leaves the CLI's own sandbox and policy settings in force"
    );
}

/// A Claude session in auto-approve, driven through the real recorder.
struct AutoApproveSession {
    state: AppState,
    session_id: String,
    recorder: SessionRecorder,
    turn: ClaudeTurnState,
    external_session_id: Option<String>,
}

impl AutoApproveSession {
    fn new() -> Self {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Claude);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].session.claude_approval_mode =
                Some(ClaudeApprovalMode::AutoApprove);
        }
        let recorder = SessionRecorder::new(state.clone(), session_id.clone());
        Self {
            state,
            session_id,
            recorder,
            turn: ClaudeTurnState::default(),
            external_session_id: None,
        }
    }

    fn event(&mut self, payload: Value) {
        handle_claude_event(
            &payload,
            &mut self.external_session_id,
            &mut self.turn,
            &mut self.recorder,
        )
        .unwrap();
    }

    fn tool_use(&mut self, id: &str, name: &str, input: Value) {
        self.event(json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}
        }));
    }

    fn refused(&mut self, id: &str, detail: &str) {
        self.event(json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": id,
                "is_error": true,
                "content": detail
            }]},
            "tool_use_result": format!("Error: {detail}")
        }));
    }

    fn reply_and_finish(&mut self, text: &str) {
        self.event(json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": text}]}
        }));
        self.event(json!({"type": "result", "is_error": false}));
    }

    fn assert_nothing_waits(&self) {
        let inner = self.state.inner.lock().unwrap();
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == self.session_id)
            .unwrap();
        assert!(
            !record.session.messages.iter().any(|message| matches!(
                message,
                Message::Approval {
                    decision: ApprovalDecision::Pending,
                    ..
                }
            )),
            "a refusal with no live request must not leave an approval card"
        );
        assert!(record.pending_claude_approvals.is_empty());
        assert_ne!(
            record.session.status,
            SessionStatus::Approval,
            "nothing is waiting for a decision"
        );
    }

    fn transcript(&self) -> Vec<Message> {
        let inner = self.state.inner.lock().unwrap();
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == self.session_id)
            .unwrap()
            .session
            .messages
            .iter()
            .cloned()
            .collect()
    }
}

/// The visible text of a transcript message, for the readable-reason checks.
fn readable(message: &Message) -> Option<&str> {
    match message {
        Message::Text { text, .. } => Some(text.as_str()),
        Message::Command { output, .. } => Some(output.as_str()),
        _ => None,
    }
}

#[test]
fn a_refused_bash_command_ends_as_a_readable_error_and_the_reply_is_kept() {
    let mut session = AutoApproveSession::new();
    let detail = "Claude requested permissions to use Bash, but you haven't granted it yet.";
    session.tool_use("bash-refused", "Bash", json!({"command": "cargo test"}));
    session.refused("bash-refused", detail);
    session.reply_and_finish("The sandbox refused cargo test, so I stopped there.");

    session.assert_nothing_waits();
    let transcript = session.transcript();
    let command = transcript
        .iter()
        .find_map(|message| match message {
            Message::Command {
                command, status, ..
            } if command == "cargo test" => Some(*status),
            _ => None,
        })
        .expect("the refused command is in the transcript");
    assert_eq!(
        command,
        CommandStatus::Error,
        "the refused command has ended"
    );
    assert!(
        transcript
            .iter()
            .filter_map(readable)
            .any(|text| text.contains(detail)),
        "the refusal says why on the session"
    );
    assert!(
        transcript
            .iter()
            .filter_map(readable)
            .any(|text| text.contains("so I stopped there")),
        "Claude's own account of the refusal is not hidden"
    );
}

#[test]
fn a_refused_file_write_ends_as_a_readable_error_and_the_reply_is_kept() {
    let mut session = AutoApproveSession::new();
    let detail = "Claude requested permissions to write to /tmp/blocked.txt, but you haven't granted it yet.";
    session.tool_use(
        "write-refused",
        "Write",
        json!({"file_path": "/tmp/blocked.txt", "content": "x"}),
    );
    session.refused("write-refused", detail);
    session.reply_and_finish("Writing /tmp/blocked.txt was refused, so the file is unchanged.");

    session.assert_nothing_waits();
    let transcript = session.transcript();
    assert!(
        transcript
            .iter()
            .filter_map(readable)
            .any(|text| text.contains(detail)),
        "the refusal says why on the session"
    );
    assert!(
        transcript
            .iter()
            .filter_map(readable)
            .any(|text| text.contains("the file is unchanged")),
        "Claude's own account of the refusal is not hidden"
    );
}

#[test]
fn an_oversized_refusal_is_kept_short_on_the_session() {
    let mut session = AutoApproveSession::new();
    let path = format!("/tmp/{}", "deep/".repeat(10_000));
    let detail =
        format!("Claude requested permissions to write to {path}, but you haven't granted it yet.");
    session.tool_use(
        "write-oversized",
        "Write",
        json!({"file_path": path, "content": "x"}),
    );
    session.refused("write-oversized", &detail);
    session.reply_and_finish("Refused.");

    session.assert_nothing_waits();
    let transcript = session.transcript();
    let reason = transcript
        .iter()
        .filter_map(readable)
        .find(|text| text.contains("Claude requested permissions to write to /tmp/deep/"))
        .expect("the refusal says why on the session");
    assert!(
        reason.chars().count() < detail.chars().count() / 4,
        "a {}-character refusal is stored at {} characters",
        detail.chars().count(),
        reason.chars().count()
    );
}
