// Test-only child beside src/tests/engram_declared_checks.rs, mounted there.
// Owns while-running declared-command visibility through the real recorder
// and HTTP list, using its parent's real dotnet/TRX fixtures. Does not launch
// dotnet, fabricate launcher registration, or infer verification from an exit.
use super::*;

#[tokio::test]
async fn declared_dotnet_is_globally_visible_before_trx_or_completion_even_with_idle_owner() {
    for claimed in [false, true] {
        let turn = EOCheckedTurn::start(&format!("registry-dotnet-{claimed}"), claimed);
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
    let turn = EOCheckedTurn::start("registry-dotnet-unlisted", false);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.recorder()
        .command_started("unlisted-dotnet-tool", "dotnet test")
        .unwrap();
    assert!(turn.state.test_run_summaries(None).is_empty());
}

#[test]
fn a_declared_completion_persists_its_observation_without_minting_verification() {
    let turn = EOCheckedTurn::start("registration-dotnet-observed", true);
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
    let request = finish_observation(&turn);
    assert_eq!(observed_result(&request), "passed");
    let runs = turn.state.test_run_summaries(None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, TestRunState::Passed);
    assert!(runs[0].registration.verification_record_ids.is_empty());
    assert_eq!(
        runs[0].registration.restart_would_void_evidence,
        None,
        "an ordinary fact is not a correlated native-verification receipt"
    );
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let row = loaded.test_run_observations.values().next().unwrap();
    assert_eq!(row.execution_id, runs[0].run_id);
    assert_eq!(row.summary.state, TestRunState::Passed);
    assert!(row.evidence.is_empty(), "reload cannot turn ordinary telemetry into verification");
}

#[test]
fn a_real_declared_failure_overrides_native_exit_zero_without_losing_the_native_code() {
    let turn = EOCheckedTurn::start("registration-dotnet-failed-observation", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run(
        "failed-dotnet-tool",
        DECLARED_TEST,
        EngramCommandExit::Code(0),
        || write_artifact(&turn, FAILING_TRX),
    );
    let request = finish_observation(&turn);
    assert_eq!(observed_result(&request), "failed");
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
    assert!(run.registration.verification_record_ids.is_empty());
    assert_eq!(run.registration.restart_would_void_evidence, None);
}

/// The declared verdict applies only to the run its own exit ended. A
/// repeated start of the same tool key in the same turn names the same run,
/// which an earlier exit already ended: the later failing artifact is not
/// applied to that older ended run.
#[test]
fn a_declared_verdict_never_reaches_an_older_ended_run_of_a_reused_key() {
    let turn = EOCheckedTurn::start("registration-reused-key", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("reused-dotnet-tool", DECLARED_TEST, EngramCommandExit::Code(0),
        || write_artifact(&turn, PASSING_TRX));
    let first = turn.state.test_run_summaries(None);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].state, TestRunState::Passed);
    let (first_id, first_ended) = (first[0].run_id.clone(), first[0].ended_at.clone());
    turn.run("reused-dotnet-tool", DECLARED_TEST, EngramCommandExit::Code(0),
        || write_artifact(&turn, FAILING_TRX));
    let runs = turn.state.test_run_summaries(None);
    let older = runs.iter().find(|run| run.run_id == first_id).expect("the first run is kept");
    assert_eq!(older.state, TestRunState::Passed, "an older ended run is never reselected");
    assert_eq!(older.ended_at, first_ended);
    assert_eq!(older.exit_code, Some(0));
}

/// A background launch is not ended by its tool result, so no declared
/// verdict applies to it even when a failing artifact is already present.
#[test]
fn a_declared_verdict_never_ends_a_background_launch() {
    let turn = EOCheckedTurn::start("registration-background-launch", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    turn.run("background-dotnet-tool", DECLARED_TEST, EngramCommandExit::NotFinished,
        || write_artifact(&turn, FAILING_TRX));
    let run = turn.state.test_run_summaries(None).pop().expect("the launch is visible");
    assert_ne!(run.state, TestRunState::Failed, "no verdict for a run that has not ended");
    assert!(run.ended_at.is_none(), "a background launch is not ended by its tool result");
}

/// A duplicate end of an already ended run ends nothing, so a later artifact
/// cannot change the verdict its own end applied.
#[test]
fn a_duplicate_declared_end_changes_nothing() {
    let turn = EOCheckedTurn::start("registration-duplicate-end", true);
    declare(&turn, DECLARED_TEST, DECLARED_ARTIFACT);
    let mut recorder = turn.recorder();
    recorder.command_started("duplicate-dotnet-tool", DECLARED_TEST).unwrap();
    turn.wait_for_snapshots();
    write_artifact(&turn, FAILING_TRX);
    recorder.command_completed_with_exit("duplicate-dotnet-tool", DECLARED_TEST,
        "dotnet completed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    let ended = turn.state.test_run_summaries(None).pop().unwrap();
    assert_eq!(ended.state, TestRunState::Failed, "its own end applied the artifact verdict");
    write_artifact(&turn, PASSING_TRX);
    recorder.command_completed_with_exit("duplicate-dotnet-tool", DECLARED_TEST,
        "dotnet completed", CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    let after = turn.state.test_run_summaries(None).pop().unwrap();
    assert_eq!(after.run_id, ended.run_id);
    assert_eq!(after.state, TestRunState::Failed, "a duplicate end applies no verdict");
    assert_eq!(after.ended_at, ended.ended_at);
}
