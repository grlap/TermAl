// Test-only child beside test_run_registration.rs, using its run fixtures.
// Owns real ACP repeated-start and unaliased launcher-result regressions.
// Does not execute tests through a provider or alter verification authority.
use super::*;

#[test]
fn acp_repeated_starts_keep_the_first_provider_boundary_and_ownership() {
    let fixture = RunFixture::new("registration-acp-repeated-start");
    let session = owner(&fixture);
    let (runtime, _runtime_rx) = test_acp_runtime_handle(AcpAgent::Cursor, "registry-acp-runtime");
    let input_tx = runtime.input_tx.clone();
    let original = EngramControlWorkBinding {
        root_execution_id: "original-root".to_owned(),
        work_id: "original-work".to_owned(),
        run_id: "original-run".to_owned(),
        work_revision: 1,
        claim_id: "original-claim".to_owned(),
        claim_fence: 1,
    };
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.agent = Agent::Cursor;
        record.runtime = SessionRuntime::Acp(runtime);
        record.active_turn_generation = 5;
        record.engram.work_binding = Some(original.clone());
        record.engram.active_grant_id = Some("original-grant".to_owned());
    }
    let mut turn_state = AcpTurnState::default();
    let mut recorder = SessionRecorder::new(fixture.state.clone(), session.clone());
    test_acp_session_update(
        &json!({
            "sessionUpdate": "tool_call", "toolCallId": "repeated-tool",
            "status": "pending", "title": "Run check",
            "rawInput": { "cwd": test_run_display_path(&fixture.root) }
        }),
        &fixture.state,
        &session,
        &input_tx,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();
    let original_start = {
        let inner = fixture.state.inner.lock().unwrap();
        assert!(inner.test_run_observations.is_empty());
        assert_eq!(inner.test_runs.unclassified_commands.len(), 1);
        inner
            .test_runs
            .unclassified_commands
            .values()
            .next()
            .unwrap()
            .started_at
            .clone()
    };
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.active_turn_generation = 6;
        record.engram.work_binding.as_mut().unwrap().work_id = "successor-work".to_owned();
        record.engram.active_grant_id = None;
    }
    for status in ["pending", "in_progress", "pending"] {
        test_acp_session_update(
            &json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "repeated-tool",
                "status": status, "rawInput": { "command": "cargo test" }
            }),
            &fixture.state,
            &session,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner.test_run_observations.len(),
            1,
            "repeated provider starts are one execution"
        );
        let row = inner.test_run_observations.values().next().unwrap();
        assert_eq!(
            row.summary.started_at.as_deref(),
            Some(original_start.as_str())
        );
        assert_eq!(row.work_binding.as_ref(), Some(&original));
        assert_eq!(row.grant_identity, Some(sha256_hex(b"original-grant")));
        assert_eq!(row.turn_generation, 5);
        assert!(row.evidence.is_empty());
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .active_turn_checks
                .is_empty(),
            "late command text cannot create a retroactive check under the successor"
        );
    }
    test_acp_session_update(
        &json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "repeated-tool",
            "status": "completed", "rawInput": { "command": "cargo test" },
            "rawOutput": { "exitCode": 0 }
        }),
        &fixture.state,
        &session,
        &input_tx,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();
    let runs = fixture.summaries();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].started_at.as_deref(), Some(original_start.as_str()));
    assert_eq!(runs[0].registration.bound_work_ids, [original.work_id]);
    assert!(runs[0].registration.verification_record_ids.is_empty());
}

#[test]
fn a_successful_unaliased_detached_launch_never_reports_a_terminal_test_pass() {
    for cut in [false, true] {
        let fixture = RunFixture::new("registration-launch-only");
        let session = running_owner(&fixture, "registry-detached-runtime");
        let command = "node scripts/test-launcher.mjs full --detach --notify coordinator";
        fixture.state.note_engram_command_started(
            &session,
            &EngramObservationProvenance::Ambient,
            "launch-tool",
            Some(command),
            Some(&test_run_display_path(&fixture.root)),
        );
        fixture.state.note_engram_command_finished_with_capture(
            &session,
            &EngramObservationProvenance::Ambient,
            "launch-tool",
            command,
            "STARTED",
            Some(EngramCommandExit::Code(0)),
            cut,
        );
        let runs = fixture.summaries();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, TestRunState::Unknown);
        assert!(runs[0].ended_at.is_none());
        assert_eq!(
            runs[0].exit_code, None,
            "launch success is not the run exit code"
        );
        assert_eq!(runs[0].registration.restart_would_void_evidence, None);
        assert!(runs[0].registration.verification_record_ids.is_empty());
        let inner = fixture.state.inner.lock().unwrap();
        let row = inner.test_run_observations.values().next().unwrap();
        assert!(row.launch_only);
        assert!(row.artifact.is_none());
        assert!(row.evidence.is_empty());
    }
}
