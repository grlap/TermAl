// Actual ordinary carried artifacts and their independent execution facts.
// Correlation predicates are tested on clones; they never mint native receipts.
use super::*;

fn refresh_carried_artifacts(turn: &EOCheckedTurn) {
    turn.state
        .refresh_test_runs_with(&|_, _| false, &|inner, event| {
            turn.state.publish_delta_locked(inner, event.clone())
        });
}

fn assert_no_native_run_claim(run: &TestRunSummary) {
    assert_eq!(run.registration.restart_would_void_evidence, None);
    assert!(run.registration.verification_record_ids.is_empty());
    assert!(run.registration.bound_work_ids.is_empty());
    assert!(
        !run.registration
            .restart_evidence_reason
            .contains("awaiting host verification settlement")
    );
}

#[test]
fn an_ordinary_terminal_artifact_keeps_exact_identity_without_automatic_evidence_or_alias() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-carried-artifact-identity");
    ordinary_launch_gate(&turn, &worktree, true);
    let run_dir = finish_run(&worktree, "passed", &"f".repeat(64));
    let request_path = run_dir.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    request["owner"] = json!(turn.session_id);
    request["detached"] = json!(true);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let digest = engram_terminal_record_digest(&run_dir).unwrap();
    let original = {
        let inner = turn.state.inner.lock().unwrap();
        let row = inner
            .test_run_observations
            .values()
            .find(|row| {
                row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())
                    && row.tool_identity == sha256_hex(b"gate")
            })
            .unwrap()
            .clone();
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        let check = &record.engram.carried_checks[0].check;
        assert_eq!(
            row.summary.worktree,
            test_run_display_path(&check.target.root)
        );
        assert_eq!(
            row.command_fingerprint,
            EngramHost::check_fingerprint(&check.command)
        );
        assert_eq!(
            row.summary.started_at.as_deref(),
            Some(check.started_at.as_str())
        );
        assert!(
            row.evidence.is_empty() && row.artifact.is_none(),
            "STARTED supplies no artifact association or native producer receipt"
        );
        row
    };
    assert_carried_artifact_custody(&turn, &run_dir, true);
    refresh_carried_artifacts(&turn);
    let runs = turn.state.test_run_summaries(None);
    assert_eq!(
        runs.len(),
        2,
        "discovery alone cannot invent a proof to merge the original execution"
    );
    let artifact = runs
        .iter()
        .find(|row| row.run_id == "test-carried")
        .unwrap();
    assert_eq!(artifact.state, TestRunState::Passed);
    assert_eq!(artifact.detail_version, Some(format!("sha256:{digest}")));
    assert_no_native_run_claim(artifact);
    assert!(artifact.registration.superseded_run_ids.is_empty());
    assert!(runs.iter().any(|row| row.run_id == original.execution_id));
    {
        let inner = turn.state.inner.lock().unwrap();
        assert!(
            inner.test_run_observations[&original.execution_id]
                .evidence
                .is_empty()
        );
    }

    // Unit control of the retained exact-association predicate. This clone
    // models an independently supplied artifact association; it is never
    // installed in AppState and is not evidence of a native registration.
    let mut associated = original.clone();
    associated.artifact = Some(TestRunObservedArtifact {
        run_id: artifact.run_id.clone(),
        run_directory: artifact.run_dir.clone(),
        request_identity: None,
        terminal_digest: Some(digest.clone()),
    });
    let matches = |run: &TestRunSummary| {
        test_run_observation_matches_run(&*turn.state.inner.lock().unwrap(), run, None, &associated)
    };
    assert!(
        matches(artifact),
        "positive exact owner/root/locator/digest predicate control"
    );
    let original_request = fs::read(&request_path).unwrap();
    for (field, value) in [
        ("owner", json!("another-session")),
        ("root", json!(test_run_display_path(&turn.root))),
    ] {
        let mut request: Value = serde_json::from_slice(&original_request).unwrap();
        request[field] = value;
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        refresh_carried_artifacts(&turn);
        let runs = turn.state.test_run_summaries(None);
        let artifact = runs
            .iter()
            .find(|row| row.run_id == "test-carried")
            .unwrap();
        assert!(
            !matches(artifact),
            "{field} cannot lend a terminal digest to another execution"
        );
        assert_no_native_run_claim(artifact);
        assert!(artifact.registration.superseded_run_ids.is_empty());
        assert!(runs.iter().any(|row| row.run_id == original.execution_id));
        fs::write(&request_path, &original_request).unwrap();
        refresh_carried_artifacts(&turn);
        let restored = turn
            .state
            .test_run_summaries(None)
            .into_iter()
            .find(|row| row.run_id == "test-carried")
            .unwrap();
        assert!(
            matches(&restored),
            "restoring exact bytes restores only the unit predicate"
        );
        assert!(restored.registration.superseded_run_ids.is_empty());
        assert_no_native_run_claim(&restored);
    }
    let results_path = run_dir.join("results.json");
    let mut bytes = fs::read(&results_path).unwrap();
    bytes.push(b' ');
    fs::write(&results_path, bytes).unwrap();
    refresh_carried_artifacts(&turn);
    let runs = turn.state.test_run_summaries(None);
    let artifact = runs
        .iter()
        .find(|row| row.run_id == "test-carried")
        .unwrap();
    assert_eq!(
        artifact.state,
        TestRunState::Passed,
        "a byte-only change leaves the parsed verdict"
    );
    assert!(
        !matches(artifact),
        "parsed PASS cannot replace the first exact terminal digest"
    );
    assert_no_native_run_claim(artifact);
    assert!(artifact.registration.superseded_run_ids.is_empty());
    assert!(runs.iter().any(|row| row.run_id == original.execution_id));
}

#[test]
fn noncompacted_carried_artifact_loss_and_reload_keep_facts_without_native_risk() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-carried-artifact-loss");
    ordinary_launch_gate(&turn, &worktree, true);
    let run_dir = finish_run(&worktree, "passed", &"f".repeat(64));
    let request_path = run_dir.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    request["owner"] = json!(turn.session_id);
    request["detached"] = json!(true);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert_carried_artifact_custody(&turn, &run_dir, true);
    turn.state.shutdown_persist_blocking();
    refresh_carried_artifacts(&turn);
    let retained = {
        let inner = turn.state.inner.lock().unwrap();
        let retained = inner.test_run_compacted["test-carried"].clone();
        assert!(!retained.compacted);
        turn.state.persist_synchronous_locked(&inner).unwrap();
        retained
    };
    assert_no_native_run_claim(
        &turn
            .state
            .test_run_summaries(None)
            .into_iter()
            .find(|row| row.run_id == "test-carried")
            .unwrap(),
    );
    fs::remove_file(run_dir.join("request.json")).unwrap();
    fs::remove_file(run_dir.join("results.json")).unwrap();
    fs::remove_dir(&run_dir).unwrap();
    refresh_carried_artifacts(&turn);
    let after_loss = turn
        .state
        .test_run_summaries(None)
        .into_iter()
        .find(|row| row.run_id == "test-carried")
        .expect("unresolved artifact identity survives loss");
    assert_no_native_run_claim(&after_loss);
    assert_eq!(after_loss.registration.artifact_unavailable, Some(true));
    assert_eq!(after_loss.registration.history_compacted, Some(false));
    assert_eq!(after_loss.detail_version, retained.summary.detail_version);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let mut cached = inner.test_run_compacted.get_mut("test-carried").unwrap();
        assert!(
            !cached.compacted && cached.registration.is_none() && cached.observations.is_empty()
        );
        // Historical cached projection from the removed automatic-settlement
        // path. This is an old display hint, with no native receipt or proof.
        cached.summary.registration.restart_would_void_evidence = Some(true);
        cached.summary.registration.restart_evidence_reason =
            "Exact carried run is awaiting host verification settlement; restarting would lose its pending credit.".to_owned();
        drop(cached);
        turn.state.persist_synchronous_locked(&inner).unwrap();
    }
    let mut restored = test_app_state();
    restored.persistence_path = turn.state.persistence_path.clone();
    restored.inner = Arc::new(StateMutex::new(
        load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap(),
    ));
    restored.refresh_test_runs_with(&|_, _| false, &|inner, event| {
        restored.publish_delta_locked(inner, event.clone())
    });
    let reloaded = restored
        .test_run_summaries(None)
        .into_iter()
        .find(|row| row.run_id == "test-carried")
        .expect("SQLite reload retains unresolved artifact custody");
    assert_eq!(reloaded.registration.artifact_unavailable, Some(true));
    assert_eq!(reloaded.registration.history_compacted, Some(false));
    assert_eq!(reloaded.detail_version, after_loss.detail_version);
    assert_no_native_run_claim(&reloaded);
    let inner = restored.inner.lock().unwrap();
    let row = &inner.test_run_compacted["test-carried"];
    assert!(!row.compacted && row.registration.is_none() && row.observations.is_empty());
    assert_eq!(row.request_identity, retained.request_identity);
    drop(inner);
    restored.shutdown_persist_blocking();
}
