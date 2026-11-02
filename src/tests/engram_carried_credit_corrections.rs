// Ordinary location/identity correction witnesses. Existing EO rows and
// bounded windows are the owners; no removed diagnostic object is modeled.

fn start_at(turn: &EOCheckedTurn, key: &str, command: &str, cwd: Option<&str>) {
    turn.state.note_engram_command_started(&turn.session_id,
        &current_command_provenance(turn), key, Some(command), cwd);
}

fn linked_launch_marker_and_relaunch(binding_missing: bool, one_call: bool) {
    let (turn, worktree) = ordinary_carried_turn("correction-linked-launch");
    let directory = engram_exact_path_key(&fs::canonicalize(&worktree).unwrap());
    let (binding, trace) = turn.record(|record| (
        record.engram.work_binding.clone(), record.engram.unadmitted_active_turn.clone()));
    // Local absence-selector inputs retained from this test. Actual ordinary
    // service naming/binding is separately exercised by the parent A/B tests.
    turn.record_mut(|record| {
        if binding_missing { record.engram.work_binding = None; }
        else { record.engram.unadmitted_active_turn = None; }
    });
    let command = if one_call { format!("pushd \"{directory}\" && {DETACHED_GATE}") }
        else { DETACHED_GATE.to_owned() };
    let before = chrono::Utc::now();
    start_at(&turn, "uncredited", &command, (!one_call).then_some(directory.as_str()));
    let after = chrono::Utc::now();
    let original = original_execution(&turn, "uncredited");
    let at = execution_start(&original).to_owned();
    let observed = engram_parse_time(&at).unwrap();
    assert!(observed >= before - chrono::Duration::milliseconds(1) && observed <= after);
    observe_finish(&turn, "uncredited", &command, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert_eq!(record.engram.carried_unmatched_launches,
            vec![(engram_path_key(&worktree), at.clone())]);
        assert!(record.engram.carried_checks.is_empty() && record.engram.active_turn_checks.is_empty());
    });
    assert_original_execution_identity(&turn, &original);
    let cwd = fs::canonicalize(&worktree).unwrap();
    let runs = review_runs(&worktree);
    let run_id = format!("test-{}", Uuid::new_v4());
    let earlier = runs.join(&run_id);
    fs::create_dir_all(&earlier).unwrap();
    fs::write(earlier.join("request.json"), serde_json::to_vec(&json!({
        "runId": run_id, "root": cwd, "owner": turn.session_id,
        "full": true, "started": at, "expectedFingerprint": "f".repeat(64),
        "stages": [{"name": "rust-tests", "kind": "test"}],
    })).unwrap()).unwrap();
    fs::write(earlier.join("results.json"), serde_json::to_vec(&json!({
        "runId": run_id, "state": "running", "started": at,
        "stages": [{"name": "rust-tests", "state": "unrun"}],
    })).unwrap()).unwrap();
    assert!(engram_terminal_record_digest(&earlier).is_none());
    turn.state.observe_test_command_artifact(&turn.session_id,
        &current_command_provenance(&turn), "uncredited", &command,
        &format!("RUN {}", earlier.display()), false);
    assert_original_execution_identity(&turn, &original);
    assert!(original.summary.worktree.is_empty(),
        "truthful B location alone cannot turn this explicit locator into credit eligibility");
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.unadmitted_active_turn = trace;
    });
    let clock = LaunchCustodyClock::at(&at);
    ordinary_launch_gate(&turn, &worktree, true);
    assert_eq!(engram_find_carried_run(&turn.session_id, &cwd, &at, &[],
        &std::collections::BTreeSet::new()).0, Some(earlier.clone()),
        "the earlier same-owner Running artifact is eligible without the window");
    turn.record(|record| {
        assert!(record.engram.carried_checks[0].fence.is_none(), "no unrelated fence");
        assert!(record.engram.carried_checks[0].ambiguous);
    });
    assert_carried_ambiguity(&turn);
    turn.record(|record| assert!(record.engram.carried_checks[0].run_directory.is_none()));
    assert_original_execution_identity(&turn, &original);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    assert_carried_ambiguity(&turn);
    drop(clock);
}

#[test]
fn unbound_linked_reported_cwd_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(true, false);
}

#[test]
fn unowned_linked_reported_cwd_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(false, false);
}

#[test]
fn unbound_linked_one_call_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(true, true);
}

#[test]
fn unowned_linked_one_call_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(false, true);
}

// This changes only the existing selector's runtime-token input, not an
// execution row, trace, root or grant. A later real dispatch creates its trace.
fn replace_runtime_input(turn: &EOCheckedTurn) {
    turn.record_mut(|record| {
        let SessionRuntime::Codex(current) = &mut record.runtime else { panic!("Codex fixture"); };
        current.runtime_id.push_str("-fresh");
    });
}

fn fresh_same_key_start(runtime: bool, bound: bool) {
    let label = if runtime { "fresh-runtime-execution" } else { "fresh-turn-execution" };
    let turn = EOCheckedTurn::start(label, false);
    let first = chrono::Utc::now();
    let at = |i| (first + chrono::Duration::seconds(i))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let clock = LaunchCustodyClock::at(&at(0));
    observe_start(&turn, "reused", DETACHED_GATE);
    let original = original_execution(&turn, "reused");
    observe_finish(&turn, "reused", DETACHED_GATE, EngramCommandExit::Code(0));
    end_unclaimed_turn(&turn);
    if runtime { replace_runtime_input(&turn); }
    if bound { bind_observed_root(&turn, label); }
    turn.begin("A fresh actual ordinary turn may reuse the provider tool key.");
    clock.advance_to(&at(1));
    observe_start(&turn, "reused", DETACHED_GATE);
    let fresh = original_execution(&turn, "reused");
    assert_ne!(fresh.execution_id, original.execution_id);
    assert!(fresh.runtime_identity != original.runtime_identity
        || fresh.turn_generation != original.turn_generation);
    assert_original_execution_identity(&turn, &original);
    turn.wait_for_snapshots();
    observe_finish(&turn, "reused", DETACHED_GATE, EngramCommandExit::Code(0));
    if bound {
        turn.record(|record| {
            assert_eq!(record.engram.carried_checks.len(), 1);
            let carried = &record.engram.carried_checks[0];
            assert_eq!(carried.check.key, "reused");
            assert!(carried.ambiguous, "the earlier location still excludes borrowing");
        });
        assert_observed_windows(&turn, &[original.clone()]);
        assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    } else {
        assert_observed_windows(&turn, &[original.clone(), fresh]);
        assert_unmeasured_without_native_credit(&turn);
        end_unclaimed_turn(&turn);
    }
    assert_original_execution_identity(&turn, &original);
}

#[test]
fn fresh_turn_same_key_can_start_and_complete_its_bound_check() {
    fresh_same_key_start(false, true);
}

#[test]
fn fresh_runtime_same_key_can_start_and_complete_its_bound_check() {
    fresh_same_key_start(true, true);
}

#[test]
fn fresh_turn_same_key_keeps_a_distinct_unbound_execution() {
    fresh_same_key_start(false, false);
}

#[test]
fn fresh_runtime_same_key_keeps_a_distinct_unbound_execution() {
    fresh_same_key_start(true, false);
}

#[test]
fn unrelated_current_finish_keeps_stale_execution_and_original_window() {
    let turn = EOCheckedTurn::start("correction-stale-finish", false);
    observe_start(&turn, "stale", DETACHED_GATE);
    let original = original_execution(&turn, "stale");
    let original_provenance = current_command_provenance(&turn);
    turn.record_mut(|record| record.active_turn_generation += 1);
    observe_finish(&turn, "not-the-stale-key", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_eq!(turn.state.inner.lock().unwrap().test_run_observations.get(&original.execution_id),
        Some(&original), "an unrelated result invents no terminal boundary");
    turn.state.note_engram_command_finished(&turn.session_id, &original_provenance,
        "stale", DETACHED_GATE, "STARTED", Some(EngramCommandExit::Code(0)));
    assert_original_execution_identity(&turn, &original);
    assert_observed_windows(&turn, &[original]);
    turn.record(|record| assert!(record.engram.pending_source_root_line.is_none()));
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn changed_completion_keeps_original_identity_and_bounded_redacted_summary() {
    let turn = EOCheckedTurn::start("correction-mismatch", false);
    let command = format!("{DETACHED_GATE} --notify private-value-not-for-summary");
    observe_start(&turn, "mismatch", &command);
    let original = original_execution(&turn, "mismatch");
    observe_finish(&turn, "mismatch", GATE, EngramCommandExit::NotFinished);
    assert_original_execution_identity(&turn, &original);
    let ended = original_execution(&turn, "mismatch");
    assert_eq!(ended.summary.command, Some(vec!["node".to_owned()]));
    assert!(!serde_json::to_string(&ended.summary).unwrap().contains("private-value-not-for-summary"));
    observe_finish(&turn, "mismatch", GATE, EngramCommandExit::NotFinished);
    assert_eq!(original_execution(&turn, "mismatch"), ended);
    assert_observed_windows(&turn, &[original]);
    turn.record(|record| assert!(record.engram.pending_source_root_line.is_none()));
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn unknown_or_disagreeing_candidate_directories_do_not_invent_a_marker_root() {
    let (turn, worktree) = ordinary_carried_turn("correction-root-negative");
    turn.record_mut(|record| {
        record.engram.work_binding = None;
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: record.runtime.runtime_token(),
            directory: Some(worktree.to_string_lossy().into_owned()), pending: None,
            lost_among: Vec::new(), lost_unbounded: false,
        });
    });
    start_at(&turn, "disagreement", DETACHED_GATE, None);
    let original = original_execution(&turn, "disagreement");
    assert!(original.summary.worktree.is_empty());
    observe_finish(&turn, "disagreement", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| assert!(record.engram.carried_unmatched_launches.is_empty()));
    assert_original_execution_identity(&turn, &original);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn disablement_does_not_become_a_command_mismatch_notice() {
    let turn = EOCheckedTurn::start("correction-disabled-mismatch", false);
    observe_start(&turn, "disabled", DETACHED_GATE);
    let original = original_execution(&turn, "disabled");
    turn.set_observability(false);
    observe_finish(&turn, "disabled", GATE, EngramCommandExit::NotFinished);
    assert_original_execution_identity(&turn, &original);
    assert_observed_windows(&turn, &[original]);
    turn.record(|record| assert!(record.engram.pending_source_root_line.is_none()));
    assert_unmeasured_without_native_credit(&turn);
}
