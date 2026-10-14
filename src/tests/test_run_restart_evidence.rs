// Test-only extension mounted by src/tests/engram_carried_checks.rs.
// Owns run-index evidence-risk assertions at the existing carried-run boundary;
// it does not mint registration, verification receipts or completion seals.
use super::*;

#[test]
fn terminal_pass_remains_at_risk_while_its_exact_carried_check_is_unsettled() {
    let (turn, worktree) = named_turn("run-risk-unsettled");
    launch_gate(&turn, &worktree, true);
    let run_dir = finish_run(&worktree, "passed", &"f".repeat(64));
    let request_path = run_dir.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    request["owner"] = json!(turn.session_id);
    request["detached"] = json!(true);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    turn.state.poll_engram_carried_runs();
    let raw_digest = turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.run_directory.as_deref(), Some(run_dir.as_path()));
        carried
            .terminal_digest
            .clone()
            .expect("first terminal digest")
    });
    let observed_id = {
        let inner = turn.state.inner.lock().unwrap();
        let row = inner
            .test_run_observations
            .values()
            .find(|row| {
                row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str())
                    && row.tool_identity == sha256_hex(b"gate")
            })
            .expect("the original launcher command is observed");
        let record = &inner.sessions[inner.find_session_index(&turn.session_id).unwrap()];
        let check = &record.engram.carried_checks[0].check;
        assert_eq!(
            row.summary.worktree,
            test_run_display_path(&check.target.root),
            "the observation retains the same validated named worktree as credit"
        );
        assert_eq!(
            row.command_fingerprint,
            EngramHost::check_fingerprint(&check.command)
        );
        assert_eq!(
            row.summary.started_at.as_deref(),
            Some(check.started_at.as_str())
        );
        row.execution_id.clone()
    };
    turn.state
        .refresh_test_runs_with(&|_, _| false, &|inner, event| {
            turn.state.publish_delta_locked(inner, event.clone())
        });
    let summary = turn
        .state
        .test_run_summaries(None)
        .into_iter()
        .find(|run| run.run_id == "test-carried")
        .expect("the matched carried artifact should be listed globally");
    assert_eq!(summary.state, TestRunState::Passed);
    assert_eq!(summary.detail_version, Some(format!("sha256:{raw_digest}")));
    let wire = serde_json::to_value(summary).unwrap();
    assert_eq!(
        wire.get("restartWouldVoidEvidence"),
        Some(&json!(true)),
        "terminal PASS cannot clear its still-carried bound check: {wire}"
    );
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);

    // The real checkpoint transfers the same raw digest to its original
    // observation. Both the scanner delta and the fresh API must merge it
    // with the prefixed disk identity, rather than publishing a duplicate.
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run_dir, "succeeded");
    {
        let inner = turn.state.inner.lock().unwrap();
        let row = &inner.test_run_observations[&observed_id];
        assert_eq!(
            row.evidence.len(),
            1,
            "the exact reported check is associated"
        );
        let association = &row.evidence[0];
        let producer_id =
            checkpoint["verification_evidence"][0]["producer_observation"]["observation_id"]
                .as_str()
                .unwrap();
        assert_eq!(association.producer_observation_id, producer_id);
        let producer = checkpoint["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|producer| producer["observation_id"] == producer_id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&association.source_basis).unwrap(),
            producer["source_basis"]
        );
        assert_eq!(
            test_run_path_key(&association.source_basis.workspace_id),
            test_run_path_key(&row.summary.worktree),
            "the source association cannot switch away from the validated root"
        );
        assert_eq!(association.source_basis.source_root_generation, Some(1));
        assert_eq!(row.work_binding.as_ref(), Some(&association.binding));
    }
    turn.state.refresh_test_runs();
    let runs = turn.state.test_run_summaries(None);
    assert_eq!(
        runs.len(),
        1,
        "exact terminal proof merges the original execution"
    );
    assert_eq!(
        runs[0].registration.superseded_run_ids,
        [observed_id.clone()]
    );
    assert_eq!(
        runs[0].registration.restart_would_void_evidence,
        Some(true),
        "a checkpoint without an exact verification receipt cannot clear evidence risk"
    );

    // Request-only mutations leave the terminal result bytes untouched.
    // Neither a new owner nor another worktree may inherit the original
    // execution's evidence merely by retaining that result digest.
    let original_request = fs::read(&request_path).unwrap();
    for (field, value) in [
        ("owner", json!("another-session")),
        ("root", json!(test_run_display_path(&turn.root))),
    ] {
        let mut request: Value = serde_json::from_slice(&original_request).unwrap();
        request[field] = value;
        fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
        turn.state.refresh_test_runs();
        let runs = turn.state.test_run_summaries(None);
        let artifact = runs
            .iter()
            .find(|run| run.run_id == "test-carried")
            .unwrap();
        assert!(
            artifact.registration.superseded_run_ids.is_empty(),
            "{field} mutation cannot alias"
        );
        assert!(
            artifact.registration.bound_work_ids.is_empty(),
            "{field} mutation cannot borrow work"
        );
        assert_eq!(
            artifact.registration.restart_would_void_evidence, None,
            "{field}"
        );
        assert!(runs.iter().any(|run| run.run_id == observed_id));
        fs::write(&request_path, &original_request).unwrap();
        turn.state.refresh_test_runs();
        let runs = turn.state.test_run_summaries(None);
        assert_eq!(
            runs.len(),
            1,
            "restoring the exact request restores the alias"
        );
        assert_eq!(
            runs[0].registration.superseded_run_ids,
            [observed_id.clone()]
        );
    }

    // A byte change cannot reuse the first terminal identity, even if its
    // parsed verdict still says PASS. Retain the original execution instead.
    let results_path = run_dir.join("results.json");
    let mut bytes = fs::read(&results_path).unwrap();
    bytes.push(b' ');
    fs::write(&results_path, bytes).unwrap();
    turn.state.refresh_test_runs();
    let runs = turn.state.test_run_summaries(None);
    let artifact = runs
        .iter()
        .find(|run| run.run_id == "test-carried")
        .unwrap();
    assert!(artifact.registration.superseded_run_ids.is_empty());
    assert_eq!(artifact.registration.restart_would_void_evidence, None);
    assert!(runs.iter().any(|run| run.run_id == observed_id));
}
