// Test-only child beside src/tests/engram_declared_checks.rs, mounted there.
// Owns while-running declared-command visibility through the real recorder
// and HTTP list, using its parent's real dotnet/TRX fixtures. Does not launch
// dotnet, fabricate launcher registration, or infer verification from an exit.
use super::*;

#[tokio::test]
async fn declared_dotnet_is_globally_visible_before_trx_or_completion_even_with_idle_owner() {
    for claimed in [false, true] {
        let turn = CheckedTurn::start(&format!("registry-dotnet-{claimed}"), claimed);
        declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
        assert!(!turn.root.join(DECLARED_ARTIFACT).exists());
        let mut recorder = turn.recorder();
        recorder
            .command_started("dotnet-tool", DECLARED_TEST)
            .unwrap();
        turn.wait_for_snapshots();
        // Provider work can outlive the agent's active presentation. Listing
        // it must not depend on the owner's status or a selected session.
        turn.record_mut(|record| record.session.status = SessionStatus::Idle);
        let response = app_router(turn.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/test-runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        let runs = list["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1, "{list}");
        let run = &runs[0];
        assert_eq!(run["registrationState"], "hostObserved");
        assert_eq!(run["trackingDurability"], "durable");
        assert_eq!(run["state"], "running");
        assert_eq!(run["ownerSessionId"], turn.session_id);
        assert_eq!(run["command"], json!(["dotnet"]));
        assert_eq!(run["runnerPid"], Value::Null, "provider supplied no PID");
        assert_eq!(run["runDir"], "", "no fictional launcher directory");
        assert!(
            run["artifactLocations"][0]
                .as_str()
                .unwrap()
                .ends_with(".termal-results/results.trx")
        );
        assert!(
            !turn.root.join(DECLARED_ARTIFACT).exists(),
            "visibility must precede the result artifact"
        );
        assert_eq!(run["verificationRecordIds"], json!([]));
        assert_eq!(run["restartWouldVoidEvidence"], Value::Null);

        write_artifact(&turn, PASSING_TRX);
        recorder
            .command_completed_with_exit(
                "dotnet-tool",
                DECLARED_TEST,
                "dotnet completed",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        turn.wait_for_snapshots();
        let current = turn.state.test_run_summaries(None);
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].state, TestRunState::Passed);
        assert!(current[0].registration.verification_record_ids.is_empty());
        // This witness proves visibility through real start/end telemetry.
        // The existing declared tests independently judge the real TRX; no
        // verification or launcher ACK is fabricated by this UI projection.
    }
}

#[test]
fn a_dotnet_line_without_an_exact_enabled_declaration_creates_no_observed_test() {
    let turn = CheckedTurn::start("registry-dotnet-unlisted", false);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.recorder()
        .command_started("unlisted-dotnet-tool", "dotnet test")
        .unwrap();
    assert!(turn.state.test_run_summaries(None).is_empty());
}

#[test]
fn real_declared_checkpoint_preserves_the_producers_verification_object_id() {
    let verification = "7".repeat(64);
    let checkpoint_id = "8".repeat(64);
    let receipt = ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision":"checkpointed", "receipt": {
            "grant_id": CHECK_GRANT, "cursor": 4, "confirmed_cursor": 4,
            "checkpoint": checkpoint_id, "verification_evidence": [verification]
        }
    })));
    let turn = CheckedTurn::start_with("registration-dotnet-receipt", true, None, vec![receipt]);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run(
        "receipt-dotnet-tool",
        DECLARED_TEST,
        EngramCommandExit::Code(0),
        || write_artifact(&turn, PASSING_TRX),
    );
    assert!(
        turn.state.test_run_summaries(None)[0]
            .registration
            .verification_record_ids
            .is_empty()
    );
    let request = turn.finish();
    assert_eq!(
        request["verification_evidence"].as_array().unwrap().len(),
        1
    );
    assert_eq!(producer_outcome(&request), "succeeded");
    let runs = turn.state.test_run_summaries(None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].registration.verification_record_ids, [verification]);
    assert_eq!(
        runs[0].registration.restart_would_void_evidence,
        Some(true),
        "a producer-recorded pass remains at risk until exact work completion"
    );
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let row = loaded.test_run_observations.values().next().unwrap();
    assert_eq!(
        row.evidence[0].checkpoint_id.as_deref(),
        Some(checkpoint_id.as_str())
    );
    assert_eq!(row.evidence[0].confirmed_cursor, Some(4));
}

#[test]
fn a_real_declared_failure_overrides_native_exit_zero_without_losing_the_native_code() {
    let receipt = ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision":"checkpointed", "receipt": {
            "grant_id": CHECK_GRANT, "cursor": 4, "confirmed_cursor": 4,
            "checkpoint": "8".repeat(64), "verification_evidence": ["7".repeat(64)]
        }
    })));
    let turn = CheckedTurn::start_with(
        "registration-dotnet-failed-receipt",
        true,
        None,
        vec![receipt],
    );
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run(
        "failed-dotnet-tool",
        DECLARED_TEST,
        EngramCommandExit::Code(0),
        || write_artifact(&turn, FAILING_TRX),
    );
    let request = turn.finish();
    assert_eq!(producer_outcome(&request), "failed");
    let run = turn.state.test_run_summaries(None).pop().unwrap();
    assert_eq!(
        run.state,
        TestRunState::Failed,
        "real artifact verdict is authoritative for the declared test"
    );
    assert_eq!(
        run.exit_code,
        Some(0),
        "native process status remains separately available"
    );
    assert_eq!(run.registration.restart_would_void_evidence, Some(false));
}
