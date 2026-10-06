//! Kimi's ACP shell-call output and exit recognition: the host's credit path
//! for Kimi sessions. Kimi 2.1.1 sends a shell call's terminal
//! `tool_call_update` with the command's combined output as a bare-string
//! `rawOutput` (and again as the first text content block) and never an exit
//! status field, so the host reads the exit from the update's own signals:
//! `completed` (the tool's exit-0 report), or `failed` with the tool's own
//! closing line "Command failed with exit code: N.".
//!
//! The fixtures are the real `tool_call` / `tool_call_update` sequences
//! captured from a live Kimi 2.1.1 ACP session over stdio, one passing and
//! one failing `cargo` call. Owns the Kimi exit/output mapping and its
//! controls against the other ACP adapters; does not own the generic ACP
//! status normalization (`acp_tool_status.rs`).

use super::*;

const KIMI_ACP_BASH_PASS: &str = include_str!("fixtures/kimi-acp-bash-pass.json");
const KIMI_ACP_BASH_FAIL: &str = include_str!("fixtures/kimi-acp-bash-fail.json");

/// The captured frames of one Kimi shell call, in wire order.
fn kimi_acp_frames(fixture: &str) -> Vec<Value> {
    serde_json::from_str(fixture).expect("the captured fixture parses")
}

/// The captured sequence's terminal update.
fn kimi_acp_terminal(frames: &[Value]) -> &Value {
    frames.last().expect("a terminal frame")
}

#[test]
fn kimi_acp_fixtures_carry_output_as_a_bare_string_and_no_exit_code() {
    for fixture in [KIMI_ACP_BASH_PASS, KIMI_ACP_BASH_FAIL] {
        let frames = kimi_acp_frames(fixture);
        let terminal = kimi_acp_terminal(&frames);
        assert_eq!(terminal["sessionUpdate"], "tool_call_update");
        let raw_output = &terminal["rawOutput"];
        assert!(
            raw_output.is_string(),
            "Kimi's rawOutput is the output text, not an object: {terminal}"
        );
        assert!(
            terminal.pointer("/rawOutput/exitCode").is_none(),
            "no exit status field is ever sent: {terminal}"
        );
        assert!(terminal.pointer("/rawOutput/stdout").is_none());
        assert_eq!(
            terminal.pointer("/content/0/content/text").and_then(Value::as_str),
            raw_output.as_str(),
            "the same text is the first content block"
        );
        assert!(matches!(
            terminal["status"].as_str(),
            Some("completed" | "failed")
        ));
    }
}

#[test]
fn a_kimi_shell_pass_reads_as_exit_0_with_its_output_text() {
    let frames = kimi_acp_frames(KIMI_ACP_BASH_PASS);
    let terminal = kimi_acp_terminal(&frames);
    assert_eq!(terminal["status"], "completed");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, terminal),
        EngramCommandExit::Code(0),
        "Kimi's shell tool reports `completed` only for exit 0"
    );
    assert_eq!(
        summarize_acp_tool_output(terminal, AcpAgent::Kimi),
        "cargo 1.98.1 (797e8a9bc 2026-08-05)\n"
    );
}

#[test]
fn a_kimi_shell_failure_reads_as_its_suffix_exit_code_with_its_output_text() {
    let frames = kimi_acp_frames(KIMI_ACP_BASH_FAIL);
    let terminal = kimi_acp_terminal(&frames);
    assert_eq!(terminal["status"], "failed");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, terminal),
        EngramCommandExit::Code(1),
        "the tool's own closing line carries the exit code"
    );
    let summary = summarize_acp_tool_output(terminal, AcpAgent::Kimi);
    assert!(
        summary.starts_with("error: unexpected argument '--bogus-flag-xyz'"),
        "{summary}"
    );
    assert!(
        summary.ends_with("Command failed with exit code: 1."),
        "{summary}"
    );
}

#[test]
fn the_kimi_exit_and_output_reading_leaves_other_acp_agents_alone() {
    for agent in [AcpAgent::OpenCode, AcpAgent::Cursor] {
        for fixture in [KIMI_ACP_BASH_PASS, KIMI_ACP_BASH_FAIL] {
            let frames = kimi_acp_frames(fixture);
            let terminal = kimi_acp_terminal(&frames);
            assert_eq!(
                engram_acp_command_exit(agent, terminal),
                EngramCommandExit::Unknown,
                "{agent:?} reads no exit from a bare-string rawOutput"
            );
            assert_eq!(
                summarize_acp_tool_output(terminal, agent),
                serde_json::to_string_pretty(terminal.get("rawOutput").expect("rawOutput"))
                    .expect("a string prints"),
                "{agent:?} keeps the pre-existing pretty-print of a non-object rawOutput"
            );
        }
    }
}

#[test]
fn a_kimi_failure_without_the_exit_suffix_shows_nothing_to_rely_on() {
    for output in [
        // A timeout, an interruption and a spawn failure carry no exit code,
        // and a `null` code is none.
        "error: interrupted\nCommand killed by timeout (60s)",
        "Interrupted by user",
        "Process exited with code null\nCommand failed with exit code: null.",
        "",
    ] {
        let update = json!({"status": "failed", "rawOutput": output});
        assert_eq!(
            engram_acp_command_exit(AcpAgent::Kimi, &update),
            EngramCommandExit::Unknown,
            "{update}"
        );
    }
    // `completed` is the tool's exit-0 report even when nothing was printed.
    assert_eq!(
        engram_acp_command_exit(
            AcpAgent::Kimi,
            &json!({"status": "completed", "rawOutput": "Command executed successfully."})
        ),
        EngramCommandExit::Code(0)
    );
    // No terminal status, no end.
    for status in ["pending", "in_progress"] {
        let update = json!({"status": status, "rawOutput": "partial output"});
        assert_eq!(
            engram_acp_command_exit(AcpAgent::Kimi, &update),
            EngramCommandExit::Unknown,
            "{update}"
        );
    }
}

#[test]
fn a_kimi_background_start_is_the_commands_launch_not_its_end() {
    // The tool answers a `run_in_background` call `completed` with a task
    // metadata block; the command may still be writing.
    let output = "task_id: bash-1\npid: 1234\ndescription: gate\nstatus: running\nautomatic_notification: true\nnext_step: You will be automatically notified when it completes.\nhuman_shell_hint: The task is visible in the background-task panel.";
    assert_eq!(
        engram_acp_command_exit(
            AcpAgent::Kimi,
            &json!({"status": "completed", "rawOutput": output})
        ),
        EngramCommandExit::NotFinished
    );
}

/// The text Kimi's tool-result truncation service renders for an output over
/// its limit (`renderPersistedToolResult`): a header, the head and tail
/// previews, then the result's spill suffix after a blank line.
fn kimi_truncated(head: &str, tail: &str, suffix: &str) -> String {
    let mut lines = vec![
        "Tool output exceeded 50000 characters; the full output was saved to a file.".to_owned(),
        "tool_name: Bash".to_owned(),
        "tool_call_id: tool-1".to_owned(),
        "output_size_chars: 81234".to_owned(),
        "output_size_bytes: 81234".to_owned(),
        "output_path: C:/kimi/tool-results/Bash-tool-1.txt".to_owned(),
        "next_step: Use Read with output_path to page through the saved output, or Grep to search it.".to_owned(),
        String::new(),
        format!("[preview: chars [0, {})]", head.len()),
        head.to_owned(),
        String::new(),
        "[elided: chars [2000, 79234)]".to_owned(),
        String::new(),
        "[preview: chars [79234, 81234)]".to_owned(),
        tail.to_owned(),
    ];
    if !suffix.is_empty() {
        lines.push(String::new());
        lines.push(suffix.to_owned());
    }
    lines.join("\n")
}

/// The spill reference Kimi's shell tool appends to a foreground result over
/// 50,000 characters (`addForegroundOutputReference`).
const KIMI_SPILL_REFERENCE: &str =
    "task_id: bash-7\noutput_size_bytes: 81234\nnext_step: Use TaskOutput(task_id=\"bash-7\") to query the task output.";

#[test]
fn a_kimi_background_block_inside_the_truncation_wrapper_is_still_a_launch() {
    // A foreground call moved to the background still writing, whose result
    // was over the limit: the task block sits in the head preview, not at
    // byte zero.
    let block = "task_id: bash-3\npid: 4321\ndescription: verbose suite\nstatus: running\nautomatic_notification: true\nnext_step: The task now runs in the background. You will be automatically notified when it completes.\nhuman_shell_hint: The task is visible in the background-task panel.\n\nforeground_output:\nrunning 4000 tests";
    let output = kimi_truncated(block, "test big::case_3999 ... ok", "");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "completed", "rawOutput": output})),
        EngramCommandExit::NotFinished
    );
    // The same block unwrapped, detached by the user, with foreground output.
    let detached = "task_id: bash-4\npid: 99\ndescription: suite\nstatus: running\ndetached_by_user: true\nautomatic_notification: true\nnext_step: The user moved this task to the background.\nhuman_shell_hint: The task is visible in the background-task panel.\n\nforeground_output:\nrunning 12 tests";
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "completed", "rawOutput": detached})),
        EngramCommandExit::NotFinished
    );
}

#[test]
fn a_verbose_kimi_foreground_result_keeps_its_exit_behind_the_spill_reference() {
    // A noisy failure: the exit sentence closes the command's text and the
    // spill reference follows it, unwrapped and inside the wrapper.
    let unwrapped = format!("test a ... FAILED\nCommand failed with exit code: 101.\n\n{KIMI_SPILL_REFERENCE}");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": unwrapped})),
        EngramCommandExit::Code(101)
    );
    let wrapped = kimi_truncated("running 4000 tests", "test result: FAILED. 3999 passed; 1 failed\nCommand failed with exit code: 101.", KIMI_SPILL_REFERENCE);
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": wrapped})),
        EngramCommandExit::Code(101)
    );
    // A noisy success carries the same spill reference, which is no
    // background block: `completed` is still its exit-0 report.
    let passed = kimi_truncated("running 4000 tests", "test result: ok. 4000 passed; 0 failed", KIMI_SPILL_REFERENCE);
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "completed", "rawOutput": passed})),
        EngramCommandExit::Code(0)
    );
}

#[test]
fn a_wrapped_kimi_background_block_is_a_launch_even_when_its_marker_was_elided() {
    // A model-written description longer than the head preview pushes
    // `automatic_notification` into the elided middle; the block still opens
    // the original text, right after the head preview's marker.
    let long_description = "d".repeat(5000);
    let head = format!("task_id: bash-5\npid: 77\ndescription: {}", &long_description[..4000]);
    let output = kimi_truncated(&head, "test case_12000 ... ok", "");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "completed", "rawOutput": output})),
        EngramCommandExit::NotFinished
    );
}

#[test]
fn a_kimi_failure_keeps_its_exit_behind_the_per_line_truncation_pointer() {
    // Shortening long lines brought the text under the limit: the pointer is
    // appended right after the exit sentence, the spill reference after it.
    let pointer = "[Per-line truncation occurred; only the first 60000 characters (of 61000) were saved to a file.\noutput_path: C:/kimi/tool-results/Bash-tool-2.txt\nnext_step: Use Read with output_path to page through the saved output, or Grep to search it.]";
    let with_spill = format!("very long line …\nCommand failed with exit code: 101.\n{pointer}\n\n{KIMI_SPILL_REFERENCE}");
    let without_spill = format!("very long line …\nCommand failed with exit code: 2.\n{pointer}");
    for (output, code) in [(with_spill, 101), (without_spill, 2)] {
        assert_eq!(
            engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": output})),
            EngramCommandExit::Code(code)
        );
    }
    // Only the producer's exact trailer is removed, once: any other text
    // between the sentence and it, or a lone pointer line, is not the tool's
    // report.
    let extra = format!("Command failed with exit code: 2.\noutput_path: C:/x\n{pointer}");
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": extra})),
        EngramCommandExit::Unknown
    );
    let stray_path = "Command failed with exit code: 2.\noutput_path: C:/x";
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": stray_path})),
        EngramCommandExit::Unknown
    );
}

#[test]
fn a_failed_kimi_call_printing_a_background_block_still_reads_its_exit() {
    // Only a `completed` answer can be a background start; a failing test that
    // prints such a block is still that test's failure.
    let output = "task_id: bash-1\npid: 1234\nautomatic_notification: true\nassertion failed\nCommand failed with exit code: 1.";
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": output})),
        EngramCommandExit::Code(1)
    );
}

#[test]
fn only_the_closing_kimi_exit_sentence_counts_and_failed_never_reads_as_zero() {
    // The command's own output printing the sentence, followed by anything
    // other than the producer's spill lines, is not the tool's exit report.
    let embedded = "Command failed with exit code: 3.\nCommand killed by timeout (60s)";
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": embedded})),
        EngramCommandExit::Unknown
    );
    // A `failed` call is never a success, whatever code its text names.
    let zero = "Command failed with exit code: 0.";
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &json!({"status": "failed", "rawOutput": zero})),
        EngramCommandExit::Unknown
    );
}

#[test]
fn kimi_output_falls_back_to_the_content_text_when_raw_output_is_absent() {
    let update = json!({
        "status": "failed",
        "content": [{"type": "content", "content": {"type": "text", "text": "boom\nCommand failed with exit code: 42."}}]
    });
    assert_eq!(
        engram_acp_command_exit(AcpAgent::Kimi, &update),
        EngramCommandExit::Code(42)
    );
    assert_eq!(
        summarize_acp_tool_output(&update, AcpAgent::Kimi),
        "boom\nCommand failed with exit code: 42."
    );
}

#[test]
fn a_kimi_acp_shell_check_ends_with_the_exit_its_completion_states() {
    // The full captured shape: a pending create and argument-streaming
    // updates precede the frame that carries the call's input, so the check
    // starts at the dispatch frame, not at the placeholder.
    let turn = CheckedTurn::start("kimi-acp-exit", true);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    for update in [
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "kimi-check",
            "title": "Bash", "kind": "execute", "status": "pending",
            "content": [{"type": "content", "content": {"type": "text", "text": ""}}]
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "status": "in_progress",
            "content": [{"type": "content", "content": {"type": "text", "text": "{\"command\":\"cargo"}}]
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "title": format!("Running: {SIZE_TEST}"), "kind": "execute",
            "status": "in_progress", "rawInput": {"command": SIZE_TEST}
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text",
                "text": "running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4040 filtered out\n"}}],
            "rawOutput": "running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4040 filtered out\n"
        }),
    ] {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Kimi,
        )
        .expect("the update should apply");
    }
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.end.as_ref().map(|end| end.exit), check.overlapped))
                .collect::<Vec<_>>()
        }),
        [(Some(EngramCommandExit::Code(0)), false)],
        "the dispatch frame names the test before it runs: no overlap"
    );
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();
    assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded");
}

#[test]
fn a_kimi_acp_shell_check_failure_ends_with_the_suffix_exit_code() {
    let turn = CheckedTurn::start("kimi-acp-exit-fail", true);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    for update in [
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "kimi-check",
            "title": "Bash", "kind": "execute", "status": "pending",
            "content": [{"type": "content", "content": {"type": "text", "text": ""}}]
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "status": "in_progress",
            "content": [{"type": "content", "content": {"type": "text", "text": "{\"command\":\"cargo"}}]
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "title": format!("Running: {SIZE_TEST}"), "kind": "execute",
            "status": "in_progress", "rawInput": {"command": SIZE_TEST}
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "kimi-check",
            "status": "failed",
            "rawOutput": "running 1 test\ntest result: FAILED. 0 passed; 1 failed\n\nCommand failed with exit code: 101."
        }),
    ] {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Kimi,
        )
        .expect("the update should apply");
    }
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.end.as_ref().map(|end| end.exit), check.overlapped))
                .collect::<Vec<_>>()
        }),
        [(Some(EngramCommandExit::Code(101)), false)]
    );
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();
    assert_eq!(observations(&checkpoint)[0]["outcome"], "failed");
}

#[test]
fn a_check_named_late_stays_overlapped_for_other_acp_agents() {
    // The streaming prelude is Kimi's alone: another adapter whose first
    // frame names the call only by its title still marks the check the later
    // frame names (`named_late` in the turn's checks).
    let turn = CheckedTurn::start("acp-exit-late", true);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    for update in [
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "late-check",
            "title": "Bash", "kind": "execute", "status": "pending"
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "late-check",
            "title": format!("Running: {SIZE_TEST}"), "kind": "execute",
            "status": "in_progress", "rawInput": {"command": SIZE_TEST}
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "late-check",
            "status": "completed",
            "rawOutput": {"exitCode": 0, "stdout": "running 1 test\ntest result: ok. 1 passed"}
        }),
    ] {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
    }
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.overlapped, check.overlap_cause.clone()))
                .collect::<Vec<_>>()
        }),
        [(
            true,
            Some("its command was named a test only by a later start".to_owned())
        )]
    );
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();
    // A success the report could only call unknown is withheld, so an earlier
    // pass at the same revision stays the newest record.
    assert!(
        checkpoint["verification_evidence"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{checkpoint:#}"
    );
}
