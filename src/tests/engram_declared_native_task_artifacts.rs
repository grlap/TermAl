// Native task/omitted-output controls for the ordinary task-END regressions.
// Uses real native reservation/registration fixtures, with no consumer run.
use super::*;

#[test]
fn native_task_notices_keep_the_artifact_verdict_without_an_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (bytes, expected) in [
        (Some(PASS), EngramVerificationRecordRunOutcome::Passed),
        (Some(FAIL), EngramVerificationRecordRunOutcome::Failed),
        (None, EngramVerificationRecordRunOutcome::Indeterminate),
    ] {
        let fixture = ordinary("committed");
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        start_route(&fixture, &mut recorder, "background", true);
        let reservation = fixture.wait();
        if let Some(bytes) = bytes {
            artifact(&fixture, bytes);
        }
        result(&fixture, &mut recorder, "background");
        assert!(observation(&fixture).summary.ended_at.is_none());
        fixture.later_prompt();
        task_end(&fixture, "completed", &fixture.token);
        let (registration, request, results) = fixture.original_results(&reservation);
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(body.run.outcome, expected);
        assert_eq!(results["exitCode"], Value::Null);
        if bytes.is_none() {
            assert!(
                registration
                    .declared_terminal
                    .as_ref()
                    .unwrap()
                    .check
                    .summary
                    .as_deref()
                    .unwrap()
                    .contains("the artifact is missing at the end")
            );
        }
        assert_eq!(
            observation(&fixture).summary.state,
            match expected {
                EngramVerificationRecordRunOutcome::Passed => TestRunState::Passed,
                EngramVerificationRecordRunOutcome::Failed => TestRunState::Failed,
                _ => TestRunState::Unknown,
            }
        );
    }
}

#[test]
fn native_persisted_output_without_a_success_report_still_uses_its_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for structured_metadata in [false, true] {
        let fixture = ordinary("committed");
        fixture.held_revision(4);
        fixture.prepare();
        let mut recorder = fixture.recorder();
        start_route(&fixture, &mut recorder, "foreground", true);
        let reservation = fixture.wait();
        artifact(&fixture, PASS);
        let mut frame = json!({"message":{"content":[{
            "type":"tool_result", "tool_use_id":"native-tool", "is_error":true,
            "content":"<persisted-output>Output too large. Full output saved to: .tmp/provider-console.txt</persisted-output>"
        }]}});
        if structured_metadata {
            frame["tool_use_result"] = json!({"stdout":"bounded preview",
                "persistedOutputPath":".tmp/provider-console.txt", "persistedOutputSize":743_500});
        }
        handle_claude_tool_result(
            &frame,
            &mut fixture.claude_state.lock().unwrap(),
            &mut recorder,
        )
        .unwrap();
        let (registration, request, results) = fixture.original_results(&reservation);
        let body = engram_verification_record_body(&registration, &request, &results).unwrap();
        assert_eq!(body.run.outcome, EngramVerificationRecordRunOutcome::Passed);
        assert_eq!(observation(&fixture).summary.state, TestRunState::Passed);
        assert!(observation(&fixture).summary.error.is_none());
        assert_eq!(results["exitCode"], Value::Null);
    }
}
