// Owns Claude persisted-output credit diagnostics through the real parser,
// session recorder and checkpoint. Does not read spill files or change check
// verdicts. New module using the fixture from engram_turn_checks.rs.
use super::*;

const SPILL_PATH: &str = "C:\\not-read\\tool-results\\private-output.txt";
const CUT_REASON: &str = "the runtime cut the output before any passing summary was available";

fn prefix() -> String {
    "running tests\n".to_owned() + &"x".repeat(30_000 - "running tests\n".len())
}

fn persisted(stdout: &str) -> Value {
    json!({"stdout": stdout, "stderr": "", "interrupted": false,
        "isImage": false, "noOutputExpected": false,
        "persistedOutputPath": SPILL_PATH, "persistedOutputSize": 173853})
}

fn start(turn: &CheckedTurn, background: bool) -> (ClaudeTurnState, SessionRecorder) {
    let mut parser = ClaudeTurnState::default();
    let mut recorder = turn.recorder();
    let mut external = None;
    handle_claude_event(
        &json!({"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": "large-check", "name": "Bash",
            "input": {"command": "cargo test --bin termal", "run_in_background": background}
        }]}}),
        &mut external,
        &mut parser,
        &mut recorder,
    ).unwrap();
    turn.wait_for_snapshots();
    (parser, recorder)
}

fn complete(
    turn: &CheckedTurn,
    parser: &mut ClaudeTurnState,
    recorder: &mut SessionRecorder,
    result: Value,
    is_error: bool,
) {
    let detail = if is_error { "Exit code 1\ncheck failed".to_owned() } else {
        format!("<persisted-output>\nOutput too large (169.8KB). Full output saved to: {SPILL_PATH}\n\nPreview (first 2KB):\nrunning tests\n...\n</persisted-output>")
    };
    handle_claude_event(
        &json!({"type": "user", "message": {"content": [{
            "type": "tool_result", "tool_use_id": "large-check", "content": detail,
            "is_error": is_error
        }]}, "tool_use_result": result}),
        &mut None,
        parser,
        recorder,
    ).unwrap();
    turn.wait_for_snapshots();
}

fn pending(turn: &CheckedTurn) -> String {
    turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap_or_default()
}

#[test]
fn claude_persisted_output_without_summary_names_the_cut_and_quiet_remedy() {
    let turn = CheckedTurn::start("claude-cut", true);
    let (mut parser, mut recorder) = start(&turn, false);
    let stdout = prefix();
    complete(&turn, &mut parser, &mut recorder, persisted(&stdout), false);
    let checkpoint = turn.finish();
    assert_withheld_and_told(&turn, &checkpoint, CUT_REASON);
    let line = pending(&turn);
    assert!(line.contains("cargo test -q"), "{line}");
    assert!(!line.contains(SPILL_PATH) && !line.contains(&stdout), "{line}");
    assert!(!checkpoint.to_string().contains(SPILL_PATH));
    turn.record(|record| {
        assert!(record.session.messages.iter().any(|message| matches!(
            message, Message::Command { output, .. } if output == &stdout
        )), "raw command output stays unchanged");
    });
}

#[test]
fn claude_large_inline_output_still_credits_the_supplied_passing_tail() {
    // Metadata with a supplied passing summary keeps the existing verdict too:
    // this fix explains missing evidence, it never manufactures or removes it.
    for has_metadata in [false, true] {
        let turn = CheckedTurn::start(&format!("claude-inline-{has_metadata}"), true);
        let (mut parser, mut recorder) = start(&turn, false);
        let stdout = prefix() + "\ntest result: ok. 1 passed; 0 failed\n";
        let result = if has_metadata { persisted(&stdout) } else {
            json!({"stdout": stdout, "stderr": "", "interrupted": false})
        };
        complete(&turn, &mut parser, &mut recorder, result, false);
        let checkpoint = turn.finish();
        assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
        assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded");
        assert!(!pending(&turn).contains(CUT_REASON));
    }
}

#[test]
fn claude_malformed_persisted_metadata_keeps_the_generic_no_summary_refusal() {
    let stdout = prefix();
    for (index, metadata) in [
        json!({}),
        json!({"persistedOutputPath": SPILL_PATH}),
        json!({"persistedOutputSize": 173853}),
        json!({"persistedOutputPath": null, "persistedOutputSize": 173853}),
        json!({"persistedOutputPath": true, "persistedOutputSize": 173853}),
        json!({"persistedOutputPath": "", "persistedOutputSize": 173853}),
        json!({"persistedOutputPath": SPILL_PATH, "persistedOutputSize": "173853"}),
        json!({"persistedOutputPath": SPILL_PATH, "persistedOutputSize": -1}),
        json!({"persistedOutputPath": SPILL_PATH, "persistedOutputSize": 1.5}),
        json!({"persistedOutputPath": SPILL_PATH, "persistedOutputSize": 0}),
        json!({"persistedOutputPath": SPILL_PATH, "persistedOutputSize": 30000}),
    ].into_iter().enumerate() {
        let turn = CheckedTurn::start(&format!("claude-cut-malformed-{index}"), true);
        let (mut parser, mut recorder) = start(&turn, false);
        let mut result = json!({"stdout": stdout, "stderr": "", "interrupted": false});
        result.as_object_mut().unwrap().extend(metadata.as_object().unwrap().clone());
        complete(&turn, &mut parser, &mut recorder, result, false);
        let checkpoint = turn.finish();
        assert_withheld_and_told(&turn, &checkpoint, "its output shows no passing test");
        assert!(!pending(&turn).contains("cargo test -q"));
    }
}

#[test]
fn claude_persisted_metadata_never_promotes_failure_interruption_or_background() {
    for cause in ["failed", "interrupted", "background", "auto-background"] {
        let turn = CheckedTurn::start(&format!("claude-cut-{cause}"), true);
        let (mut parser, mut recorder) = start(&turn, cause == "background");
        let mut result = persisted("test result: FAILED. 0 passed; 1 failed");
        if cause == "interrupted" { result["interrupted"] = json!(true); }
        if cause == "auto-background" { result["backgroundTaskId"] = json!("task-running"); }
        complete(&turn, &mut parser, &mut recorder, result, cause == "failed");
        let checkpoint = turn.finish();
        match cause {
            "failed" | "interrupted" => {
                assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
                assert_eq!(observations(&checkpoint)[0]["outcome"], if cause == "failed" { "failed" } else { "unknown" });
            }
            _ => assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}"),
        }
        assert!(!pending(&turn).contains(CUT_REASON));
    }
}

#[test]
fn claude_persisted_output_keeps_overlap_refusal_precedence() {
    let turn = CheckedTurn::start("claude-cut-overlap", true);
    let (mut parser, mut recorder) = start(&turn, false);
    run_beside(&turn);
    complete(&turn, &mut parser, &mut recorder, persisted(&prefix()), false);
    let checkpoint = turn.finish();
    assert_withheld_and_told(&turn, &checkpoint, "reached its worktree while it ran");
    assert!(!pending(&turn).contains(CUT_REASON));
}
