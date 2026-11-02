// Ordinary execution and known-location windows keep original runtime/turn/key/root/time.
// Artifact custody and explicit native verification have separate owners.
use super::*;

mod credit_corrections {
    use super::*;
    include!("engram_carried_credit_corrections.rs");
}

#[test]
fn ordinary_claim_changes_never_relabel_the_original_carried_launch() {
    for terminal in [false, true] {
    for changed in ["revision", "fence", "claim", "work", "missing"] {
        let (turn, worktree) = ordinary_carried_turn("ordinary-carried-claim-custody");
        ordinary_launch_gate(&turn, &worktree, true);
        let run = terminal.then(|| finish_run(&worktree, "passed", &"f".repeat(64)));
        turn.state.poll_engram_carried_runs();
        let original = turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            (record.engram.work_binding.clone(), carried.work_id.clone(), carried.claim_id.clone(),
                carried.claim_fence, carried.check.host_turn_ref.clone(), carried.root_generation,
                carried.run_directory.clone(), carried.terminal_digest.clone())
        });
        turn.record_mut(|record| {
            match changed {
                "revision" => record.engram.work_binding.as_mut().unwrap().work_revision += 1,
                "fence" => record.engram.work_binding.as_mut().unwrap().claim_fence += 1,
                "claim" => record.engram.work_binding.as_mut().unwrap().claim_id.push_str("-other"),
                "work" => record.engram.work_binding.as_mut().unwrap().work_id.push_str("-other"),
                _ => record.engram.work_binding = None,
            }
            assert!(runtime_observation_check_owner(record).is_none(),
                "a changed current association cannot start checks under the original trace owner");
        });
        turn.state.poll_engram_carried_runs();
        turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            assert_eq!((&carried.work_id, &carried.claim_id, carried.claim_fence,
                &carried.check.host_turn_ref, carried.root_generation,
                &carried.run_directory, &carried.terminal_digest),
                (&original.1, &original.2, original.3, &original.4, original.5, &original.6, &original.7));
            assert!(record.engram.carried_consumed_runs.is_empty());
        });
        turn.record_mut(|record| record.engram.work_binding = original.0);
        turn.record(|record| assert!(runtime_observation_check_owner(record).is_some()));
        if let Some(run) = run {
            assert_carried_artifact_custody(&turn, &run, true);
        } else {
            assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
            turn.record(|record| {
                assert_eq!(record.engram.carried_checks.len(), 1);
                assert!(record.engram.carried_checks[0].run_directory.is_none());
                assert!(record.engram.carried_checks[0].terminal_digest.is_none());
            });
        }
    }
    }
}

#[test]
fn an_expired_or_fenced_launch_does_not_poll_a_new_artifact_or_consume_verification() {
    for expired in [false, true] {
        let (turn, worktree) = ordinary_carried_turn("ordinary-carried-poll-stop");
        ordinary_launch_gate(&turn, &worktree, true);
        if expired {
            turn.record_mut(|record| record.engram.carried_checks[0].check.started_at =
                (chrono::Utc::now() - chrono::Duration::hours(7))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        } else {
            turn.recorder().push_diff("fenced.rs", "actual edit", "+change", ChangeType::Edit).unwrap();
        }
        let start = turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            assert!(!engram_carried_check_polls(carried, chrono::Utc::now()));
            assert_eq!(engram_carried_check_expired(carried, chrono::Utc::now()), expired);
            carried.check.started_at.clone()
        });
        let run = write_run(&review_runs(&worktree), "test-unclaimed-after-poll-stop",
            &fs::canonicalize(&worktree).unwrap(), &start, "passed", &"f".repeat(64));
        assert!(engram_terminal_record_digest(&run).is_some(), "a valid candidate exists");
        turn.state.poll_engram_carried_runs();
        turn.record(|record| {
            assert_eq!(record.engram.carried_checks.len(), 1);
            let carried = &record.engram.carried_checks[0];
            assert!(carried.run_directory.is_none() && carried.terminal_digest.is_none());
            assert!(record.engram.carried_consumed_runs.is_empty());
        });
        assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
        turn.record(|record| {
            assert_eq!(record.engram.carried_checks.len(), 1);
            assert!(record.engram.carried_consumed_runs.is_empty());
        });
    }
}

#[test]
fn an_existing_carried_fence_survives_claim_change_without_native_consumption() {
    let (turn, worktree) = ordinary_carried_turn("ordinary-carried-fence-custody");
    ordinary_launch_gate(&turn, &worktree, true);
    turn.recorder().push_diff("first.rs", "first edit", "+original", ChangeType::Edit).unwrap();
    let original = turn.record(|record| record.engram.work_binding.clone());
    let fence = turn.record(|record| record.engram.carried_checks[0].fence.clone().unwrap());
    turn.record_mut(|record| record.engram.work_binding.as_mut().unwrap().claim_id.push_str("-other"));
    turn.recorder().push_diff("second.rs", "later edit", "+later", ChangeType::Edit).unwrap();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks[0].fence.as_deref(), Some(fence.as_str()));
        assert!(record.engram.carried_consumed_runs.is_empty());
    });
    turn.record_mut(|record| record.engram.work_binding = original);
    let cause = assert_carried_fence_custody(&turn, "first.rs");
    assert_eq!(cause, fence);
    assert!(!cause.contains("second.rs"));
}

fn current_command_provenance(turn: &EOCheckedTurn) -> EngramObservationProvenance {
    turn.record(|record| EngramObservationProvenance::Runtime {
        token: record.runtime.runtime_token().unwrap(),
        turn_generation: record.active_turn_generation,
    })
}

fn original_execution(turn: &EOCheckedTurn, key: &str) -> TestRunCommandObservation {
    let inner = turn.state.inner.lock().unwrap();
    let index = inner.find_session_index(&turn.session_id).unwrap();
    let origin = TestRunCommandOrigin::capture(
        &inner, index, &current_command_provenance_locked(&inner.sessions[index]), key);
    inner.test_run_observations.get(&origin.execution_id())
        .expect("the exact original execution exists").clone()
}

fn current_command_provenance_locked(record: &SessionRecord) -> EngramObservationProvenance {
    EngramObservationProvenance::Runtime {
        token: record.runtime.runtime_token().unwrap(),
        turn_generation: record.active_turn_generation,
    }
}

fn execution_start(row: &TestRunCommandObservation) -> &str {
    row.summary.started_at.as_deref().expect("original provider start")
}

fn assert_original_execution_identity(turn: &EOCheckedTurn, original: &TestRunCommandObservation) {
    let inner = turn.state.inner.lock().unwrap();
    let now = inner.test_run_observations.get(&original.execution_id).unwrap();
    assert_eq!((&now.execution_id, &now.runtime_identity, &now.tool_identity,
        now.turn_generation, &now.grant_identity, &now.command_fingerprint,
        &now.work_binding, &now.source_root, &now.summary.started_at, &now.summary.worktree),
        (&original.execution_id, &original.runtime_identity, &original.tool_identity,
        original.turn_generation, &original.grant_identity, &original.command_fingerprint,
        &original.work_binding, &original.source_root, &original.summary.started_at, &original.summary.worktree));
    assert!(now.artifact.is_none() && now.evidence.is_empty() && now.final_unbound_proof.is_none());
}

fn observe_start(turn: &EOCheckedTurn, key: &str, command: &str) {
    let cwd = fs::canonicalize(&turn.root).unwrap().to_string_lossy().into_owned();
    turn.state.note_engram_command_started(&turn.session_id,
        &current_command_provenance(turn), key, Some(command), Some(&cwd));
}

fn observe_finish(turn: &EOCheckedTurn, key: &str, command: &str, exit: EngramCommandExit) {
    turn.state.note_engram_command_finished(&turn.session_id,
        &current_command_provenance(turn), key, command, "STARTED", Some(exit));
}

fn assert_unmeasured_without_native_credit(turn: &EOCheckedTurn) {
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty());
    });
    assert_ordinary_turn_has_no_native_requests(turn);
}

fn assert_observed_windows(turn: &EOCheckedTurn, rows: &[TestRunCommandObservation]) {
    let expected = rows.iter().map(|row| {
        (engram_path_key(&turn.root), execution_start(row).to_owned())
    }).collect::<Vec<_>>();
    turn.record(|record| assert_eq!(record.engram.carried_unmatched_launches, expected));
}

// An actual unclaimed closing has no report. Never use finish(), which waits
// for a claimed report, as evidence of quiet unclaimed completion.
fn end_unclaimed_turn(turn: &EOCheckedTurn) {
    turn.end();
    let guard = phase_sync::PollGuard::new();
    while turn.record(|record| record.engram.unadmitted_worker_running
        || runtime_observation_has_removal_duty(record))
    {
        guard.wait("unclaimed execution observation settles quietly");
    }
    turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    assert!(turn.transport.requests().iter().all(|request|
        request.request["operation"] != "execution_observe"));
    assert_ordinary_turn_has_no_native_requests(turn);
}

fn bind_observed_root(turn: &EOCheckedTurn, label: &str) {
    let binding = test_control_work_binding(&format!("eo-check-{label}"), 1);
    turn.transport.set_current_work_binding(Some(binding.clone()));
    turn.transport.replace_held_claims([Ok(EngramHeldClaims {
        items: vec![EngramHeldClaim {
            work_id: binding.work_id.clone(), short_ref: binding.work_id.clone(),
            claim_id: binding.claim_id.clone(), claim_fence: binding.claim_fence,
            focused: true, control_binding: Some(binding.clone()),
        }], omitted: 0,
    })]);
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work: binding.work_id.clone(),
        path: Some(Some(turn.root.to_string_lossy().into_owned())),
    }).expect("ordinary work service binds the newly held claim and names this root");
    turn.record(|record| assert_eq!(record.engram.work_binding.as_ref(), Some(&binding)));
}

fn unmeasured_observation_launch(
    label: &str, claimed: bool, has_owner: bool, detached: bool, gain_binding: bool,
) {
    let turn = EOCheckedTurn::start(label, claimed);
    let saved_binding = turn.record(|record| record.engram.work_binding.clone());
    // Explicit owner-absence inputs for this local selector test. We do not
    // claim these mutations are provider dispatch or work-service recovery.
    turn.record_mut(|record| {
        if !claimed || gain_binding { record.engram.work_binding = None; }
        if !has_owner { record.engram.unadmitted_active_turn = None; }
    });
    let command = if detached { DETACHED_GATE } else { GATE };
    let before = chrono::Utc::now();
    observe_start(&turn, "unmeasured", command);
    let after = chrono::Utc::now();
    let original = original_execution(&turn, "unmeasured");
    let started = engram_parse_time(execution_start(&original)).unwrap();
    assert!(started >= before - chrono::Duration::milliseconds(1) && started <= after);
    if gain_binding { turn.record_mut(|record| record.engram.work_binding = saved_binding); }
    let exit = if detached { EngramCommandExit::Code(0) } else { EngramCommandExit::NotFinished };
    observe_finish(&turn, "unmeasured", command, exit);
    let ended = original_execution(&turn, "unmeasured");
    assert_eq!((&ended.execution_id, &ended.work_binding, &ended.source_root,
        &ended.grant_identity, &ended.runtime_identity, ended.turn_generation),
        (&original.execution_id, &original.work_binding, &original.source_root,
        &original.grant_identity, &original.runtime_identity, original.turn_generation));
    assert_observed_windows(&turn, &[original]);
    observe_finish(&turn, "unmeasured", command, exit);
    assert_eq!(original_execution(&turn, "unmeasured"), ended,
        "a duplicate completion cannot replace the original execution or outcome");
    turn.record(|record| assert!(record.engram.pending_source_root_line.is_none(),
        "old reason-specific deficit notices are retired"));
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn unbound_detached_launch_keeps_facts_without_carry() {
    unmeasured_observation_launch("outcome-unbound-detached", false, true, true, false);
}

#[test]
fn unbound_background_launch_keeps_facts_without_carry() {
    unmeasured_observation_launch("outcome-unbound-background", false, true, false, false);
}

#[test]
fn bound_launch_without_owner_keeps_unmeasured_facts() {
    unmeasured_observation_launch("outcome-no-observation", true, false, true, false);
}

#[test]
fn binding_acquired_after_start_does_not_hide_unbound_launch() {
    unmeasured_observation_launch("outcome-bound-after-start", true, true, false, true);
}

#[test]
fn duplicate_start_and_delayed_finish_keep_original_time_and_authority() {
    let turn = EOCheckedTurn::start("outcome-delayed", true);
    let binding = turn.record(|record| record.engram.work_binding.clone());
    turn.record_mut(|record| record.engram.work_binding = None);
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let clock = LaunchCustodyClock::at(&at);
    observe_start(&turn, "delayed", DETACHED_GATE);
    let original = original_execution(&turn, "delayed");
    turn.record_mut(|record| record.engram.work_binding = binding);
    let later = (engram_parse_time(&at).unwrap() + chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    clock.advance_to(&later);
    observe_start(&turn, "delayed", DETACHED_GATE);
    assert_eq!(original_execution(&turn, "delayed"), original);
    assert_observed_windows(&turn, &[original.clone()]);
    observe_finish(&turn, "delayed", DETACHED_GATE, EngramCommandExit::Code(0));
    let ended = original_execution(&turn, "delayed");
    assert_eq!(execution_start(&ended), execution_start(&original));
    assert!(ended.work_binding.is_none() && ended.source_root.is_none());
    assert_observed_windows(&turn, &[original]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn failed_and_compound_launches_keep_actual_results_without_claiming_credit() {
    for (label, command, exit) in [
        ("outcome-failed", DETACHED_GATE.to_owned(), EngramCommandExit::Code(1)),
        ("outcome-compound", format!("{DETACHED_GATE}; echo done"), EngramCommandExit::Code(0)),
    ] {
        let turn = EOCheckedTurn::start(label, false);
        observe_start(&turn, "ended", &command);
        let original = original_execution(&turn, "ended");
        observe_finish(&turn, "ended", &command, exit);
        let ended = original_execution(&turn, "ended");
        assert_eq!(ended.execution_id, original.execution_id);
        assert_eq!(ended.summary.state, TestRunState::Unknown);
        assert!(ended.summary.exit_code.is_none() && ended.summary.ended_at.is_none(),
            "a detached or compound launch acknowledgement is not a terminal test outcome");
        assert!(ended.artifact.is_none() && ended.evidence.is_empty());
        // The current factual owner conservatively retains a known start;
        // a launcher's failed acknowledgement does not erase that fact.
        assert_observed_windows(&turn, &[original]);
        assert_unmeasured_without_native_credit(&turn);
    }
}

#[test]
fn foreground_and_missing_start_cannot_invent_carry_or_execution() {
    let turn = EOCheckedTurn::start("outcome-ordinary", false);
    observe_start(&turn, "foreground", GATE);
    observe_finish(&turn, "foreground", GATE, EngramCommandExit::Code(0));
    let original = original_execution(&turn, "foreground");
    let count = turn.state.inner.lock().unwrap().test_run_observations.len();
    observe_finish(&turn, "missing-start", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_eq!(turn.state.inner.lock().unwrap().test_run_observations.len(), count);
    assert_observed_windows(&turn, &[original.clone()]);
    assert_eq!(original_execution(&turn, "foreground"), original);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn missing_location_keeps_execution_without_inventing_root() {
    let turn = EOCheckedTurn::start("outcome-no-location", false);
    turn.record_mut(|record| record.engram.shell_directory = Some(EngramShellDirectory {
        runtime: record.runtime.runtime_token(), directory: None, pending: None,
        lost_among: Vec::new(), lost_unbounded: true,
    }));
    turn.state.note_engram_command_started(&turn.session_id,
        &current_command_provenance(&turn), "unknown", Some(DETACHED_GATE), None);
    let original = original_execution(&turn, "unknown");
    assert!(original.summary.worktree.is_empty());
    observe_finish(&turn, "unknown", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(record.engram.carried_unmatched_launches.is_empty());
        assert!(record.engram.pending_source_root_line.is_none());
    });
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn disablement_preserves_only_already_observed_windows() {
    for at_finish in [false, true] {
        for project in [false, true] {
            let turn = EOCheckedTurn::start("outcome-disabled", false);
            let disable = || {
                let mut inner = turn.state.inner.lock().unwrap();
                let index = inner.find_session_index(&turn.session_id).unwrap();
                let id = inner.sessions[index].session.project_id.clone().unwrap();
                let settings = &mut inner.projects.iter_mut().find(|p| p.id == id).unwrap().engram;
                if project { *settings = None; }
                else { settings.as_mut().unwrap().execution_observability = false; }
            };
            if !at_finish { disable(); }
            observe_start(&turn, "disabled", DETACHED_GATE);
            let original = original_execution(&turn, "disabled");
            if at_finish { disable(); }
            observe_finish(&turn, "disabled", DETACHED_GATE, EngramCommandExit::Code(0));
            turn.record(|record| {
                assert!(record.engram.pending_source_root_line.is_none());
                let expected = if at_finish {
                    vec![(engram_path_key(&turn.root), execution_start(&original).to_owned())]
                } else { Vec::new() };
                assert_eq!(record.engram.carried_unmatched_launches, expected,
                    "disablement stops new observations but does not erase an existing location fact");
            });
            assert_unmeasured_without_native_credit(&turn);
        }
    }
}

#[test]
fn foreign_replaced_and_unowned_results_cannot_take_original_execution() {
    let turn = EOCheckedTurn::start("outcome-foreign", false);
    observe_start(&turn, "owned", DETACHED_GATE);
    let original = original_execution(&turn, "owned");
    let (token, generation) = turn.record(|record| (
        record.runtime.runtime_token().unwrap(), record.active_turn_generation));
    for provenance in [
        EngramObservationProvenance::Runtime { token, turn_generation: generation + 1 },
        EngramObservationProvenance::Runtime {
            token: RuntimeToken::Codex("outcome-replaced".to_owned()), turn_generation: generation,
        },
    ] {
        turn.state.note_engram_command_finished(&turn.session_id, &provenance,
            "owned", DETACHED_GATE, "STARTED", Some(EngramCommandExit::Code(0)));
        assert_eq!(original_execution(&turn, "owned"), original);
        assert_observed_windows(&turn, &[original.clone()]);
    }
    // Explicit local selector input: the current turn has lost its owner.
    turn.record_mut(|record| record.unmediated_claude_turn = Some(UnmediatedClaudeTurn {
        adopted_generation: None, notice_message_id: "unowned-result".to_owned(),
    }));
    turn.state.note_engram_command_finished(&turn.session_id, &EngramObservationProvenance::Ambient,
        "owned", DETACHED_GATE, "STARTED", Some(EngramCommandExit::Code(0)));
    assert_original_execution_identity(&turn, &original);
    turn.record_mut(|record| record.unmediated_claude_turn = None);
    observe_finish(&turn, "owned", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_eq!(original_execution(&turn, "owned").summary.state, TestRunState::Unknown);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn excluded_starts_create_no_window_and_a_new_turn_cannot_borrow_execution() {
    let turn = EOCheckedTurn::start("outcome-excluded-start", false);
    let cwd = turn.root.to_string_lossy().into_owned();
    for provenance in [
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: turn.record(|record| record.runtime.runtime_token().unwrap()),
            origin: ClaudeWorkOrigin::Unattributed,
        }),
        EngramObservationProvenance::Runtime {
            token: RuntimeToken::Codex("outcome-stale-start".to_owned()),
            turn_generation: turn.record(|record| record.active_turn_generation),
        },
    ] {
        turn.state.note_engram_command_started(&turn.session_id, &provenance,
            "excluded", Some(DETACHED_GATE), Some(&cwd));
    }
    turn.record(|record| assert!(record.engram.carried_unmatched_launches.is_empty()));
    observe_start(&turn, "reused", DETACHED_GATE);
    let original = original_execution(&turn, "reused");
    let provenance = current_command_provenance(&turn);
    // A predecessor's explicitly attributed boundary may refine only its
    // own execution; it cannot manufacture a start in the newer turn.
    let count = turn.state.inner.lock().unwrap().test_run_observations.len();
    turn.record_mut(|record| record.active_turn_generation += 1);
    turn.state.note_engram_command_finished(&turn.session_id, &provenance,
        "reused", DETACHED_GATE, "STARTED", Some(EngramCommandExit::Code(0)));
    assert_original_execution_identity(&turn, &original);
    assert_eq!(turn.state.inner.lock().unwrap().test_run_observations.len(), count);
    assert_observed_windows(&turn, &[original]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn changed_command_with_the_same_key_cannot_borrow_a_launch_start() {
    let turn = EOCheckedTurn::start("outcome-changed-command", false);
    observe_start(&turn, "changed", DETACHED_GATE);
    let original = original_execution(&turn, "changed");
    observe_finish(&turn, "changed", GATE, EngramCommandExit::NotFinished);
    let after = original_execution(&turn, "changed");
    assert_eq!((&after.execution_id, &after.command_fingerprint, &after.work_binding, &after.source_root),
        (&original.execution_id, &original.command_fingerprint, &original.work_binding, &original.source_root));
    assert!(after.artifact.is_none() && after.evidence.is_empty());
    assert_observed_windows(&turn, &[original]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn abandonment_respects_original_runtime_turn_and_execution() {
    let turn = EOCheckedTurn::start("outcome-abandoned", false);
    observe_start(&turn, "abandoned", DETACHED_GATE);
    let original = original_execution(&turn, "abandoned");
    let provenance = current_command_provenance(&turn);
    let generation = turn.record(|record| record.active_turn_generation);
    turn.record_mut(|record| record.active_turn_generation += 1);
    turn.state.note_engram_command_abandoned(&turn.session_id, &provenance, "abandoned");
    assert_original_execution_identity(&turn, &original);
    turn.record_mut(|record| record.active_turn_generation = generation);
    turn.state.note_engram_command_abandoned(&turn.session_id, &provenance, "abandoned");
    let after = original_execution(&turn, "abandoned");
    assert_eq!((&after.execution_id, &after.command_fingerprint, &after.work_binding),
        (&original.execution_id, &original.command_fingerprint, &original.work_binding));
    assert_eq!(after.summary.state, TestRunState::Unknown);
    assert!(after.artifact.is_none());
    assert_observed_windows(&turn, &[original]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn a_replacement_runtime_cannot_take_an_original_execution_with_the_same_key() {
    let turn = EOCheckedTurn::start("outcome-replaced-runtime", false);
    observe_start(&turn, "reused", DETACHED_GATE);
    let original = original_execution(&turn, "reused");
    let provenance = current_command_provenance(&turn);
    turn.record_mut(|record| {
        let SessionRuntime::Codex(runtime) = &mut record.runtime else { panic!("Codex fixture"); };
        runtime.runtime_id.push_str("-replacement");
    });
    turn.state.note_engram_command_finished(&turn.session_id, &provenance,
        "reused", DETACHED_GATE, "STARTED", Some(EngramCommandExit::Code(0)));
    assert_original_execution_identity(&turn, &original);
    assert_observed_windows(&turn, &[original]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn bounded_windows_keep_newest_starts_without_rewriting_old_executions() {
    let turn = EOCheckedTurn::start("outcome-cap", false);
    let first = chrono::Utc::now();
    let at = |i| (first + chrono::Duration::milliseconds(i))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let clock = LaunchCustodyClock::at(&at(0));
    let mut originals = Vec::new();
    for index in 0..=ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT {
        clock.advance_to(&at(index as i64));
        let key = format!("cap-{index}");
        observe_start(&turn, &key, DETACHED_GATE);
        originals.push(original_execution(&turn, &key));
    }
    assert_observed_windows(&turn, &originals[1..]);
    let inner = turn.state.inner.lock().unwrap();
    for original in &originals {
        assert_eq!(inner.test_run_observations.get(&original.execution_id), Some(original),
            "bounded correlation does not discard or rewrite original durable executions");
    }
    drop(inner);
    observe_finish(&turn, "cap-0", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_observed_windows(&turn, &originals[1..]);
    observe_finish(&turn, &format!("cap-{}", ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT),
        DETACHED_GATE, EngramCommandExit::Code(0));
    assert_observed_windows(&turn, &originals[1..]);
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn bound_unmeasured_launch_keeps_its_existing_refusal() {
    let turn = EOCheckedTurn::start("outcome-unmeasured", true);
    observe_start(&turn, "unmeasured", DETACHED_GATE);
    turn.wait_for_snapshots();
    observe_finish(&turn, "unmeasured", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(record.engram.pending_source_root_line.as_ref().unwrap()
            .contains("not measured in a named source root"));
        assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
    });
    assert_unmeasured_without_native_credit(&turn);
}

#[test]
fn an_exact_explicit_locator_attaches_only_to_its_original_execution_without_native_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("explicit-original-execution");
    ordinary_launch_gate(&turn, &worktree, true);
    let original = original_execution(&turn, "gate");
    let run_id = format!("test-{}", Uuid::new_v4());
    let run = write_named_run(&worktree, &run_id, "passed");
    let request_path = run.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    request["owner"] = json!(turn.session_id);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let locator = format!("RUN {}", run.display());
    let provenance = current_command_provenance(&turn);
    turn.state.observe_test_command_artifact(&turn.session_id, &provenance,
        "gate", DETACHED_GATE, &locator, true);
    assert!(original_execution(&turn, "gate").artifact.is_none(), "cut output proves no locator");
    turn.state.observe_test_command_artifact(&turn.session_id, &provenance,
        "another-key", DETACHED_GATE, &locator, false);
    assert!(original_execution(&turn, "gate").artifact.is_none(), "another invocation cannot lend its key");
    turn.state.observe_test_command_artifact(&turn.session_id, &provenance,
        "gate", DETACHED_GATE, &locator, false);
    let attached = original_execution(&turn, "gate");
    let artifact = attached.artifact.as_ref().expect("the exact same-root/owner/request locator attaches");
    assert_eq!(artifact.run_id, run_id);
    assert!(artifact.request_identity.is_some());
    assert_eq!((&attached.execution_id, &attached.runtime_identity, &attached.tool_identity,
        attached.turn_generation, &attached.grant_identity, &attached.work_binding, &attached.source_root),
        (&original.execution_id, &original.runtime_identity, &original.tool_identity,
        original.turn_generation, &original.grant_identity, &original.work_binding, &original.source_root));
    assert!(attached.evidence.is_empty() && attached.final_unbound_proof.is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}
