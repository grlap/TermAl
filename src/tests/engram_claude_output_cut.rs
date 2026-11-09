// Owns Claude persisted-output safety through the real parser and ordinary EO.
// Keeps diagnostic result facts and raw transcript output without opening spill
// paths or granting native verification. Uses the shared ordinary-turn fixture.
use super::*;

const SPILL_PATH: &str = "C:\\not-read\\tool-results\\private-output.txt";

fn prefix() -> String {
    "running tests\n".to_owned() + &"x".repeat(30_000 - "running tests\n".len())
}

fn persisted(stdout: &str) -> Value {
    json!({"stdout": stdout, "stderr": "", "interrupted": false,
        "isImage": false, "noOutputExpected": false,
        "persistedOutputPath": SPILL_PATH, "persistedOutputSize": 173853})
}

fn start(turn: &EOCheckedTurn, background: bool) -> (ClaudeTurnState, SessionRecorder) {
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
    )
    .unwrap();
    turn.wait_for_snapshots();
    (parser, recorder)
}

fn complete(
    turn: &EOCheckedTurn,
    parser: &mut ClaudeTurnState,
    recorder: &mut SessionRecorder,
    result: Value,
    is_error: bool,
) {
    let detail = if is_error {
        "Exit code 1\ncheck failed".to_owned()
    } else {
        format!(
            "<persisted-output>\nOutput too large (169.8KB). Full output saved to: {SPILL_PATH}\n\nPreview (first 2KB):\nrunning tests\n...\n</persisted-output>"
        )
    };
    handle_claude_event(
        &json!({"type": "user", "message": {"content": [{
            "type": "tool_result", "tool_use_id": "large-check", "content": detail,
            "is_error": is_error
        }]}, "tool_use_result": result}),
        &mut None,
        parser,
        recorder,
    )
    .unwrap();
    turn.wait_for_snapshots();
}

fn diagnostic(turn: &EOCheckedTurn) -> EngramTurnCheck {
    turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == "large-check")
            .unwrap()
            .clone()
    })
}

fn finish_observed(turn: &EOCheckedTurn, expected: &str) {
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], expected);
    assert!(
        !request.to_string().contains(SPILL_PATH),
        "spill paths are not observation evidence"
    );
    assert_ordinary_observation_has_no_native_credit(turn, &request);
}

#[test]
fn claude_persisted_output_without_summary_keeps_the_cut_and_raw_output() {
    let turn = EOCheckedTurn::start("claude-cut", true);
    let (mut parser, mut recorder) = start(&turn, false);
    let stdout = prefix();
    complete(&turn, &mut parser, &mut recorder, persisted(&stdout), false);
    let check = diagnostic(&turn);
    assert!(check.runtime_output_cut);
    assert!(!check.end.as_ref().unwrap().showed_passing_tests);
    assert!(
        !check
            .end
            .as_ref()
            .unwrap()
            .result_lines
            .join("\n")
            .contains(SPILL_PATH)
    );
    finish_observed(&turn, "indeterminate");
    turn.record(|record| {
        assert!(
            record.session.messages.iter().any(|message| matches!(
                message, Message::Command { output, .. } if output == &stdout
            )),
            "raw command output stays unchanged"
        );
    });
}

#[test]
fn claude_large_inline_output_keeps_its_supplied_passing_tail_without_native_credit() {
    for has_metadata in [false, true] {
        let turn = EOCheckedTurn::start(&format!("claude-inline-{has_metadata}"), true);
        let (mut parser, mut recorder) = start(&turn, false);
        let stdout = prefix() + "\ntest result: ok. 1 passed; 0 failed\n";
        let result = if has_metadata {
            persisted(&stdout)
        } else {
            json!({"stdout": stdout, "stderr": "", "interrupted": false})
        };
        complete(&turn, &mut parser, &mut recorder, result, false);
        let check = diagnostic(&turn);
        assert_eq!(check.runtime_output_cut, has_metadata);
        assert!(
            check.end.as_ref().unwrap().showed_passing_tests,
            "actual supplied tail remains diagnostic evidence even with spill metadata"
        );
        assert!(
            check
                .end
                .as_ref()
                .unwrap()
                .result_lines
                .iter()
                .any(|line| line.contains("test result: ok. 1 passed"))
        );
        // A cut ordinary observation stays conservative; it never becomes a
        // native verification or opens the spill file to manufacture a verdict.
        finish_observed(
            &turn,
            if has_metadata {
                "indeterminate"
            } else {
                "passed"
            },
        );
    }
}

#[test]
fn claude_malformed_persisted_metadata_does_not_claim_an_output_cut() {
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
    ]
    .into_iter()
    .enumerate()
    {
        let turn = EOCheckedTurn::start(&format!("claude-cut-malformed-{index}"), true);
        let (mut parser, mut recorder) = start(&turn, false);
        let mut result = json!({"stdout": stdout, "stderr": "", "interrupted": false});
        result
            .as_object_mut()
            .unwrap()
            .extend(metadata.as_object().unwrap().clone());
        complete(&turn, &mut parser, &mut recorder, result, false);
        let check = diagnostic(&turn);
        assert!(!check.runtime_output_cut);
        assert!(!check.end.as_ref().unwrap().showed_passing_tests);
        // This is the observed successful shell result, not proof that a test passed.
        finish_observed(&turn, "passed");
    }
}

#[test]
fn claude_persisted_metadata_never_promotes_failure_interruption_or_background() {
    for cause in ["failed", "interrupted", "background", "auto-background"] {
        let turn = EOCheckedTurn::start(&format!("claude-cut-{cause}"), true);
        let (mut parser, mut recorder) = start(&turn, cause == "background");
        let mut result = persisted("test result: FAILED. 0 passed; 1 failed");
        if cause == "interrupted" {
            result["interrupted"] = json!(true);
        }
        if cause == "auto-background" {
            result["backgroundTaskId"] = json!("task-running");
        }
        complete(&turn, &mut parser, &mut recorder, result, cause == "failed");
        let check = diagnostic(&turn);
        let end = check.end.as_ref().unwrap();
        assert!(!end.showed_passing_tests);
        match cause {
            "failed" => assert_eq!(end.exit, EngramCommandExit::Code(1)),
            "interrupted" => assert_eq!(end.exit, EngramCommandExit::Unknown),
            _ => assert_eq!(end.exit, EngramCommandExit::NotFinished),
        }
        finish_observed(&turn, "indeterminate");
    }
}

#[test]
fn claude_persisted_output_keeps_overlap_and_cut_diagnostics_without_native_credit() {
    let turn = EOCheckedTurn::start("claude-cut-overlap", true);
    let (mut parser, mut recorder) = start(&turn, false);
    run_beside(&turn);
    complete(
        &turn,
        &mut parser,
        &mut recorder,
        persisted(&prefix()),
        false,
    );
    let check = diagnostic(&turn);
    assert!(check.overlapped && check.runtime_output_cut);
    assert!(!check.end.as_ref().unwrap().showed_passing_tests);
    finish_observed(&turn, "indeterminate");
}
