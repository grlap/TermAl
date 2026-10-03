//! Shared observation accounting and continuation ownership on production paths.
use super::*;

#[test]
fn source_observation_noop_resume_does_not_enqueue_persistence() {
    let claimed = ClaimedRoot::new_scripted("source-observation-noop-resume", vec![]);
    let (persist_tx, persist_rx) = std::sync::mpsc::channel();
    let state = AppState { persist_tx, ..claimed.state.clone() };
    let revision = state.inner.lock().unwrap().revision;
    state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    assert!(matches!(persist_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "a Resume with no source-observation owner must not request a write");
    assert_eq!(state.inner.lock().unwrap().revision, revision);
}

fn opening_fixture(label: &str) -> ClaimedRoot {
    let grant = format!("turn-observation-{label}-grant");
    let claimed = ClaimedRoot::new_scripted(label, vec![
        bind_reply(&format!("turn-observation-{label}-token")),
        grant_reply(&grant), begin_reply(&grant), checkpoint_reply(&grant),
    ]);
    prepare_confirmed_claimed_opening(&claimed, label);
    let worktree = add_claimed_root_worktree(&claimed.root);
    name_claimed_root_source_root(&claimed, label, &worktree);
    claimed
}

fn held_observation(label: &str) -> ClaimedRoot {
    let claimed = opening_fixture(label);
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap()
        .lose_next_observation_reply = true;
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Held { .. }));
    assert!(claimed.runtime_rx.try_recv().is_err());
    claimed.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.source_observation_continuation.is_some());
        assert!(!record.engram.source_opening_disposition.allows_delivery());
    });
    claimed
}

fn original_intent(claimed: &ClaimedRoot) -> EngramSourceObservationIntent {
    let inner = claimed.state.inner.lock().unwrap();
    inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .find(|intent| intent.session_id == claimed.session_id).unwrap().clone()
}

// Canonical history belongs to the producer after the local coupled ACK.
// Validate its exact original wire facts even when the outbox has compacted.
fn finalized_observation_receipt(claimed: &ClaimedRoot, original: &EngramSourceObservationIntent) -> (Value, Value) {
    let roots = claimed.transport.named_roots.lock().unwrap();
    let (request, receipt) = roots.as_ref().unwrap().observations.values().find(|(request, receipt)| {
        let mut captured = original.clone();
        captured.observing_session = receipt["observing_session"].as_str().map(str::to_owned);
        validate_retained_source_observation_receipt(&captured, request, receipt).is_ok()
    }).expect("the producer must retain a validated receipt for the original immutable facts").clone();
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .any(|intent| intent.id == original.id), "fully finalized payload must compact");
    (request, receipt)
}

fn queue_next_observation_turn(claimed: &ClaimedRoot, label: &str, suffix: &str) -> String {
    let grant = format!("turn-observation-{label}-{suffix}-grant");
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(&grant), begin_reply(&grant), checkpoint_reply(&grant),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(
        test_control_work_binding(&format!("turn-observation-{label}"), 1))));
    grant
}

#[test]
fn source_observation_authority_loss_cannot_erase_the_captured_pending_gate() {
    let claimed = held_observation("source-observation-authority-loss");
    let before = original_intent(&claimed);
    let (target, owner) = {
        let inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        (AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true).unwrap().unwrap(),
            EngramQueuedAdmissionOwner::capture(&inner.sessions[index]).unwrap())
    };
    claimed.record(|record| {
        record.engram.work_binding = None;
        record.engram.active_turn_source_binding = None;
    });
    let _ = claimed.state.capture_engram_source_observation(&claimed.session_id, &target, &owner, None);
    claimed.record(|record| {
        assert!(!engram_source_observation_can_deliver(record),
            "absence of current attribution cannot erase a captured accounting barrier");
        assert_eq!(record.engram.source_observation_gate.as_ref().unwrap().observation_id, before.id);
    });
    assert_eq!(original_intent(&claimed), before);
    assert!(claimed.runtime_rx.try_recv().is_err());
}

fn pre_intent_conflict_fixture(label: &str, lose_close: bool) -> (ClaimedRoot, String) {
    let claimed = held_observation(label);
    let earlier = original_intent(&claimed);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &earlier.prompt_id).unwrap();
    let adapter = claimed.state.inner.lock().unwrap().engram_host_adapter.clone();
    let receipt = adapter.request(&earlier.connection, &EngramControlRequest::TurnCheckpoint {
        routing_token: earlier.routing_token.clone(), grant_id: earlier.grant_id.clone(),
        next_intent: EngramNextIntent::Exit, report: EngramTurnReport::default(),
        idempotency_key: "fixture-predecessor-close".to_owned(),
    }, Duration::from_millis(earlier.call_timeout_ms)).unwrap();
    assert!(matches!(parse_engram_result::<EngramTurnCheckpointResponse>(receipt.clone()).unwrap(),
        EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == earlier.grant_id));
    let owner = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let mut settled = owner.observations[0].clone();
    settled.grant_settlement = Some(receipt);
    claimed.state.replace_source_history_intent(&owner, settled).unwrap();
    // Control the publication race at the off-lock capture seam. The earlier
    // intent contains actual measurements, request and a real close receipt;
    // its pending accounting becomes visible only after the next real begin.
    let predecessor = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let predecessor = inner.engram_source_sightings.remove(0);
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let record = &mut inner.sessions[index];
        assert_eq!(record.engram.uncertain_grant_id.as_ref(), Some(&earlier.grant_id));
        record.engram.uncertain_grant_id = None;
        record.engram.source_observation_gate = None;
        record.set_auto_dispatch_blocked(false);
        predecessor
    };
    // Cancellation requires a fresh bind before evaluate. Supplying a grant
    // in its place would stop at binding validation before the capture hook.
    claimed.transport.responses.lock().unwrap().push_back(bind_reply(&earlier.routing_token));
    let next = queue_next_observation_turn(&claimed, label, "conflicting");
    let state = claimed.state.clone();
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_checkpoint_reply = lose_close;
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            assert!(inner.engram_source_sightings.is_empty());
            inner.engram_source_sightings.push(predecessor);
        }));
    });
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Held { .. }));
    assert!(TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| hook.borrow().is_none()));
    assert!(claimed.runtime_rx.try_recv().is_err());
    (claimed, next)
}

#[test]
fn source_observation_pre_intent_conflict_retains_the_new_measurement_before_close() {
    let (claimed, next) = pre_intent_conflict_fixture("source-observation-pre-intent-conflict", false);
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    let retained = owner.observations.iter().find(|intent| intent.grant_id == next)
        .expect("the newly measured begun grant needs its own durable historical intent");
    assert!(retained.delivery_retired && retained.continuation_released);
    assert!(matches!(retained.phase, EngramSourceObservationPhase::Captured));
    assert!(owner.latest.is_none(), "pending conflict facts never promote the baseline");
    assert!(retained.grant_settlement.is_some(), "an acknowledged retired owner can close its grant early");
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .any(|intent| intent == retained), "the retained facts and matching settlement must reach SQLite");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count(), 2);
}

#[test]
fn source_observation_ambiguous_initial_close_survives_settings_replacement_and_restart() {
    let (claimed, next) = pre_intent_conflict_fixture("source-observation-ambiguous-close", true);
    let before = claimed.state.inner.lock().unwrap().engram_source_sightings[0].observations.iter()
        .find(|intent| intent.grant_id == next).unwrap().clone();
    assert!(before.delivery_retired && before.grant_settlement.is_none());
    assert!(matches!(before.phase, EngramSourceObservationPhase::Captured));
    let original_close = before.grant_settlement_request.clone().expect("initial close identity must be persisted before send");
    let restored = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(restored.engram_source_sightings.iter().flat_map(|owner| &owner.observations).any(|intent| intent == &before));
    *claimed.state.inner.lock().unwrap() = restored;
    claimed.state.install_test_engram_budget_clock(EngramBudgetClock::scripted());
    install_control_only_transport(&claimed.state, claimed.transport.clone());
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        assert!(inner.sessions[index].engram.source_observation_continuation.is_none(), "restart loses the runtime payload");
        let project_id = inner.sessions[index].session.project_id.clone().unwrap();
        let project = inner.projects.iter_mut().find(|project| project.id == project_id).unwrap();
        project.engram.as_mut().unwrap().home = Some(before.connection.home.join("replacement").to_string_lossy().into_owned());
        claimed.state.commit_locked(&mut inner).unwrap();
    }
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let _ = finalized_observation_receipt(&claimed, &before);
    let requests = claimed.transport.requests();
    let closes: Vec<_> = requests.iter().filter(|request| request.request["operation"] == "turn_checkpoint"
        && request.request["grant_id"] == next).collect();
    assert_eq!(closes.len(), 2, "lost first close reply must retry the same captured operation");
    for close in closes {
        assert_eq!(close.connection, before.connection);
        assert_eq!(close.request, original_close);
    }
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 2);
    assert!(claimed.runtime_rx.try_recv().is_err(), "retired provider payload is never replayed after restart");
    claimed.record(|record| assert!(record.orchestrator_auto_dispatch_blocked));
}

#[test]
fn source_observation_missing_proof_after_measured_opening_is_an_invariant_hold() {
    let claimed = opening_fixture("source-observation-missing-proof");
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            assert!(inner.sessions[index].engram.active_turn_start_basis.is_some(),
                "the invariant control requires an actual measured opening");
            for history in &mut inner.engram_work_naming_history { history.proofs.clear(); }
            inner.sessions[index].engram.active_turn_observation_root_basis = None;
        }));
    });
    let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    assert!(TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| hook.borrow().is_none()));
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Held { .. }),
        "an impossible missing proof must not become no-evidence delivery: {outcome:?}");
    assert!(claimed.runtime_rx.try_recv().is_err());
    claimed.record(|record| {
        assert!(!engram_source_observation_can_deliver(record));
        assert!(record.session.preview.contains("invariant"), "the hold must diagnose its cause");
        assert!(record.engram.source_observation_continuation.is_none(), "invariant failures are not blind provider retries");
    });
    let retained = original_intent(&claimed);
    assert!(retained.delivery_retired);
    assert!(matches!(retained.phase, EngramSourceObservationPhase::Invariant { .. }));
    assert!(retained.root_basis.is_null(), "an absent proof must not be fabricated");
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .any(|intent| intent == &retained), "the actual captured grant responsibility must reach SQLite");
}

#[test]
fn source_observation_late_older_accounting_cannot_regress_a_newer_measured_baseline() {
    let label = "source-observation-older-recovery";
    let claimed = held_observation(label);
    let old = original_intent(&claimed);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &old.prompt_id).unwrap();
    let adapter = claimed.state.inner.lock().unwrap().engram_host_adapter.clone();
    let receipt = adapter.request(&old.connection, &EngramControlRequest::TurnCheckpoint {
        routing_token: old.routing_token.clone(), grant_id: old.grant_id.clone(),
        next_intent: EngramNextIntent::Exit, report: EngramTurnReport::default(),
        idempotency_key: "fixture-older-close".to_owned(),
    }, Duration::from_millis(old.call_timeout_ms)).unwrap();
    assert!(matches!(parse_engram_result::<EngramTurnCheckpointResponse>(receipt.clone()).unwrap(),
        EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == old.grant_id));
    let previous = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let mut settled = previous.observations[0].clone();
    settled.grant_settlement = Some(receipt);
    claimed.state.replace_source_history_intent(&previous, settled).unwrap();
    let delayed = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let delayed = inner.engram_source_sightings.remove(0).observations.remove(0);
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let record = &mut inner.sessions[index];
        assert_eq!(record.engram.uncertain_grant_id.as_ref(), Some(&old.grant_id));
        record.engram.uncertain_grant_id = None;
        record.engram.source_observation_gate = None;
        record.set_auto_dispatch_blocked(false);
        delayed
    };
    fs::write(PathBuf::from(&old.sighting.basis.workspace_id).join("README.md"), "newer independently measured content\n").unwrap();
    claimed.transport.responses.lock().unwrap().push_back(bind_reply(&old.routing_token));
    let next = queue_next_observation_turn(&claimed, label, "newer");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let new_intent = original_intent(&claimed);
    assert_eq!(new_intent.grant_id, next);
    let runtime = claimed.runtime_token();
    finish_claimed_turn(&claimed, &runtime);
    let _ = finalized_observation_receipt(&claimed, &new_intent);
    let newer = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        assert!(owner.observations.is_empty());
        let newer = owner.latest.clone().unwrap();
        assert_ne!(newer.basis.source_revision, old.sighting.basis.source_revision);
        assert!(chrono::DateTime::parse_from_rfc3339(&newer.observed_at).unwrap()
            > chrono::DateTime::parse_from_rfc3339(&old.sighting.observed_at).unwrap());
        // Publish genuinely captured older facts after the newer measured and
        // accounted turn; no timestamp or receipt is invented by this seam.
        owner.observations.push(delayed);
        owner.version += 1;
        claimed.state.commit_locked(&mut inner).unwrap();
        newer
    };
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let _ = finalized_observation_receipt(&claimed, &old);
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert!(owner.observations.is_empty());
    assert_eq!(owner.latest.as_ref(), Some(&newer), "receipt arrival order must not replace measurement order");
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_superseded_capture_retains_an_uncertain_original_close() {
    let label = "source-observation-capture-uncertain-successor";
    let claimed = opening_fixture(label);
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_checkpoint_reply = true;
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            let record = &mut inner.sessions[index];
            assert!(record.engram.active_turn_start_basis.is_some());
            record.engram.dispatch_generation += 1;
            record.active_turn_generation += 1;
            record.engram.active_grant_id = Some("successor-grant".to_owned());
            record.engram.uncertain_grant_id = Some("successor-recovery".to_owned());
            record.engram.source_opening_disposition = EngramSourceOpeningDisposition::Equal;
            record.engram.source_observation_delivery_grant = Some("successor-grant".to_owned());
        }));
    });
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Superseded));
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    let retained = saved.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .find(|intent| intent.grant_id == format!("turn-observation-{label}-grant"))
        .expect("supersession must durably retain the original measured grant despite occupied successor slots").clone();
    assert!(retained.delivery_retired);
    assert!(retained.grant_settlement.is_none());
    assert!(retained.grant_settlement_request.is_some());
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let requests = claimed.transport.requests();
    let closes: Vec<_> = requests.iter().filter(|request| request.request["operation"] == "turn_checkpoint").collect();
    assert_eq!(closes.len(), 2);
    assert_eq!(closes[0].request, closes[1].request, "Resume must replay the original close identity");
    let _ = finalized_observation_receipt(&claimed, &retained);
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some("successor-grant"));
        assert_eq!(record.engram.uncertain_grant_id.as_deref(), Some("successor-recovery"));
        assert_eq!(record.engram.source_observation_delivery_grant.as_deref(), Some("successor-grant"));
    });
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_superseded_capture_closes_only_its_original_grant() {
    let label = "source-observation-capture-successor";
    let claimed = opening_fixture(label);
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.dispatch_generation += 1;
            record.active_turn_generation += 1;
            record.engram.active_grant_id = Some("successor-grant".to_owned());
            record.engram.uncertain_grant_id = Some("successor-recovery".to_owned());
            record.engram.source_opening_disposition = EngramSourceOpeningDisposition::Equal;
            record.engram.source_observation_delivery_grant = Some("successor-grant".to_owned());
        }));
    });
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Superseded));
    assert!(TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| hook.borrow().is_none()));
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some("successor-grant"));
        assert_eq!(record.engram.uncertain_grant_id.as_deref(), Some("successor-recovery"));
        assert_eq!(record.engram.source_observation_delivery_grant.as_deref(), Some("successor-grant"));
    });
    let requests = claimed.transport.requests();
    let closes: Vec<_> = requests.iter().filter(|request| request.request["operation"] == "turn_checkpoint").collect();
    assert_eq!(closes.len(), 1, "the superseded begun grant still requires its original close");
    assert_eq!(closes[0].request["grant_id"], format!("turn-observation-{label}-grant"));
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_unmeasured_stop_keeps_the_lifecycle_close_owner() {
    assert_unmeasured_lifecycle_close("source-observation-unmeasured-stop", false);
}

#[test]
fn source_observation_unmeasured_reset_keeps_the_lifecycle_close_owner() {
    assert_unmeasured_lifecycle_close("source-observation-unmeasured-reset", true);
}

fn assert_unmeasured_lifecycle_close(label: &str, reset: bool) {
    let claimed = opening_fixture(label);
    let live = claimed.state.inner.lock().unwrap().engram_turn_basis_captures_live.clone();
    let mut releases = Vec::new();
    for _ in 0..ENGRAM_TURN_BASIS_CAPTURE_LIMIT {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let basis: Option<EngramExecutionSourceBasis> = bounded_content_revision_capture(
            &live, ENGRAM_TURN_BASIS_CAPTURE_LIMIT, Duration::from_millis(1), move || {
                let _ = wait.recv();
                None
            });
        assert!(basis.is_none());
    }
    assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), ENGRAM_TURN_BASIS_CAPTURE_LIMIT);
    let stopped = Arc::new(Mutex::new(None));
    let stopped_hook = stopped.clone();
    let state = claimed.state.clone();
    let session_id = claimed.session_id.clone();
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
                assert!(record.engram.active_turn_start_basis.is_none());
                assert!(record.engram.active_grant_id.is_some());
                assert!(inner.engram_source_sightings.is_empty());
            }
            if reset {
                let project_id = {
                    let inner = state.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&session_id).unwrap()]
                        .session.project_id.clone().unwrap()
                };
                state.update_project_engram_settings(&project_id, EngramProjectSettings::default()).unwrap();
            } else {
                state.stop_session(&session_id).unwrap();
            }
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session_id).unwrap()];
            *stopped_hook.lock().unwrap() = Some((record.engram.dispatch_generation,
                record.active_turn_generation, record.engram.active_grant_id.clone(),
                record.engram.uncertain_grant_id.clone(), record.session.status,
                record.queued_prompts.len()));
        }));
    });
    let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    drop(releases);
    let guard = super::super::super::phase_sync::PollGuard::new();
    while live.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        guard.wait(format_args!("bounded capture workers must release after lifecycle teardown"));
    }
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Superseded));
    assert!(TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| hook.borrow().is_none()));
    claimed.record(|record| assert_eq!(Some((record.engram.dispatch_generation,
        record.active_turn_generation, record.engram.active_grant_id.clone(),
        record.engram.uncertain_grant_id.clone(), record.session.status,
        record.queued_prompts.len())), *stopped.lock().unwrap(),
        "late preparation must not change the lifecycle owner's settled state"));
    let requests = claimed.transport.requests();
    let closes: Vec<_> = requests.iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").collect();
    assert_eq!(closes.len(), 1, "only the real lifecycle may close this unmeasured grant");
    assert_eq!(closes[0].request["grant_id"], format!("turn-observation-{label}-grant"));
    assert_eq!(closes[0].request["next_intent"], if reset { "exit" } else { "wait" });
    if reset {
        assert_eq!(closes[0].request["idempotency_key"],
            format!("termal-project-reset-checkpoint:{}:turn-observation-{label}-grant", claimed.session_id));
    } else {
        assert!(closes[0].request["idempotency_key"].as_str().unwrap().starts_with("termal-checkpoint:"));
    }
    let begin = requests.iter().find(|request| request.request["operation"] == "turn_begin").unwrap();
    assert_eq!(closes[0].connection, begin.connection, "the original lifecycle connection owns settlement");
    assert!(!requests.iter().any(|request| request.request["operation"] == "execution_observe"));
    assert!(claimed.state.inner.lock().unwrap().engram_source_sightings.is_empty());
    assert!(!claimed.runtime_rx.try_iter().any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. })));
}

#[test]
fn source_observation_unmeasured_opening_delivers_without_advancing_the_baseline() {
    let label = "source-observation-unmeasured";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    fs::write(worktree.join("README.md"), "change before an unmeasured opening\n").unwrap();
    queue_next_observation_turn(&claimed, label, "unmeasured");

    // Fill the actual bounded capture workers, rather than assigning a fake
    // missing measurement. Their completion channels release on every exit.
    let live = claimed.state.inner.lock().unwrap().engram_turn_basis_captures_live.clone();
    let mut releases = Vec::new();
    for _ in 0..ENGRAM_TURN_BASIS_CAPTURE_LIMIT {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let basis: Option<EngramExecutionSourceBasis> = bounded_content_revision_capture(
            &live, ENGRAM_TURN_BASIS_CAPTURE_LIMIT, Duration::from_millis(1), move || {
                let _ = wait.recv();
                None
            });
        assert!(basis.is_none());
    }
    assert_eq!(live.load(std::sync::atomic::Ordering::SeqCst), ENGRAM_TURN_BASIS_CAPTURE_LIMIT);
    let previous_observations = claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "execution_observe").count();
    let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    drop(releases);
    let guard = super::super::super::phase_sync::PollGuard::new();
    while live.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        guard.wait(format_args!("bounded capture workers must release"));
    }
    claimed.record(|record| {
        assert!(record.engram.active_turn_start_basis.is_none());
        assert!(matches!(record.engram.active_turn_root_capture,
            Some(EngramRootCapture::Recorded { state: EngramSourceRootState::Named, .. })));
    });
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Delivered),
        "the base no-measurement path must deliver without fabricating accounting: {outcome:?}");
    let _ = received_prompt(&claimed);
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "execution_observe").count(), previous_observations);
    fs::write(worktree.join("README.md"), "change during the unmeasured turn\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    assert_eq!(claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.as_ref(), Some(&baseline),
        "a raw closing measurement cannot hide the interval after an unmeasured opening");
    let next = queue_next_observation_turn(&claimed, label, "measured");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let intent = claimed.state.inner.lock().unwrap().engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations).find(|intent| intent.grant_id == next).unwrap().clone();
    assert_eq!(intent.baseline.as_ref(), Some(&baseline));
    assert_ne!(intent.sighting.basis.source_revision, baseline.basis.source_revision);
}

#[test]
fn source_observation_pending_sighting_does_not_promote_the_accounted_baseline() {
    let label = "source-observation-pending-baseline";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    fs::write(worktree.join("README.md"), "unaccounted between-turn content\n").unwrap();
    let next = queue_next_observation_turn(&claimed, label, "pending");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Held { .. }));
    assert!(claimed.runtime_rx.try_recv().is_err());
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    let intent = owner.observations.iter().find(|intent| intent.grant_id == next).unwrap();
    assert_eq!(intent.baseline.as_ref(), Some(&baseline));
    assert!(matches!(intent.phase, EngramSourceObservationPhase::Prepared { .. }));
    assert_ne!(intent.sighting.basis.source_revision, baseline.basis.source_revision);
    assert_eq!(owner.latest.as_ref(), Some(&baseline),
        "retained unacknowledged facts are not an accounted baseline");
}

#[test]
fn source_observation_early_settlement_still_requires_accounting_without_another_close() {
    let claimed = held_observation("source-observation-early-settlement");
    let before = original_intent(&claimed);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &before.prompt_id).unwrap();
    let (owner, mut intent) = {
        let inner = claimed.state.inner.lock().unwrap();
        let owner = inner.engram_source_sightings.iter().find(|owner|
            owner.observations.iter().any(|intent| intent.id == before.id)).unwrap().clone();
        let intent = owner.observations.iter().find(|intent| intent.id == before.id).unwrap().clone();
        (owner, intent)
    };
    let adapter = claimed.state.inner.lock().unwrap().engram_host_adapter.clone();
    let receipt = adapter.request(&intent.connection, &EngramControlRequest::TurnCheckpoint {
        routing_token: intent.routing_token.clone(), grant_id: intent.grant_id.clone(),
        next_intent: EngramNextIntent::Exit, report: EngramTurnReport::default(),
        idempotency_key: "fixture-early-close".to_owned(),
    }, Duration::from_millis(intent.call_timeout_ms)).unwrap();
    assert!(matches!(parse_engram_result::<EngramTurnCheckpointResponse>(receipt.clone()).unwrap(),
        EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == intent.grant_id));
    intent.grant_settlement = Some(receipt);
    claimed.state.replace_source_history_intent(&owner, intent.clone()).unwrap();
    let saved_owner = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let clock = claimed.state.engram_budget_clock();
    claimed.state.confirm_source_history_durable(&saved_owner, &clock,
        clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS)).unwrap();
    let closes = claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count();
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let _ = finalized_observation_receipt(&claimed, &before);
    assert_eq!(claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.as_ref(),
        Some(&before.sighting), "the durably accounted sighting becomes the baseline after early settlement too");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count(), closes);
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_begin").count(), 1);
    assert!(claimed.runtime_rx.try_recv().is_err(), "retired provider payload is never replayed");
}

#[test]
fn source_observation_settled_abort_does_not_readmit_behind_pending_accounting() {
    let label = "source-observation-settled-abort";
    let claimed = held_observation(label);
    let before = original_intent(&claimed);
    let target = {
        let inner = claimed.state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true).unwrap().unwrap()
    };
    // Actually close the begun fixture grant, retain the exact receipt, and
    // exercise the existing production settlement/ACK and automatic tick.
    let receipt = target.adapter.request(&before.connection, &EngramControlRequest::TurnCheckpoint {
        routing_token: before.routing_token.clone(), grant_id: before.grant_id.clone(),
        next_intent: EngramNextIntent::Exit, report: EngramTurnReport::default(),
        idempotency_key: "fixture-settled-abort".to_owned(),
    }, target.settings.call_timeout()).unwrap();
    assert!(matches!(parse_engram_result::<EngramTurnCheckpointResponse>(receipt.clone()).unwrap(),
        EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == before.grant_id));
    let owner = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let mut intent = before.clone();
    intent.grant_settlement = Some(receipt);
    claimed.state.replace_source_history_intent(&owner, intent).unwrap();
    let saved = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let clock = claimed.state.engram_budget_clock();
    claimed.state.confirm_source_history_durable(&saved, &clock,
        clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS)).unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        assert!(claimed.state.settle_engram_abort_before_handoff(&mut inner, index,
            EngramAbortReason::AdmissionFence, Some(&before.grant_id), &engram_abort_authority(&target), None));
    }
    let next = format!("turn-observation-{label}-second-grant");
    claimed.transport.responses.lock().unwrap().extend([
        bind_reply(&before.routing_token), grant_reply(&next), begin_reply(&next), checkpoint_reply(&next),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(before.binding.clone())));
    claimed.state.engram_abort_retry_tick(chrono::Utc::now());
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
    let requests = claimed.transport.requests();
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1,
        "a durable settled close must not readmit while accounting still needs explicit Resume");
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_evaluate").count(), 1);
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_checkpoint").count(), 1);
    assert!(matches!(original_intent(&claimed).phase, EngramSourceObservationPhase::Prepared { .. }));
    assert!(claimed.runtime_rx.try_recv().is_err());
    claimed.record(|record| assert!(record.orchestrator_auto_dispatch_blocked));
    // Resume finishes the real accounted receipt and local history ACK. The
    // already settled grant is not closed again, and the retained head can
    // then use its original acknowledged abort retry to admit exactly once.
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let _ = finalized_observation_receipt(&claimed, &before);
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(2));
    let _ = received_prompt(&claimed);
    let requests = claimed.transport.requests();
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 2);
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_checkpoint").count(), 1);
}

#[test]
fn source_observation_tick_retries_the_same_accounted_request_and_original_grant() {
    let claimed = held_observation("source-observation-tick");
    let before = original_intent(&claimed);
    let EngramSourceObservationPhase::Prepared { request } = &before.phase else {
        panic!("a lost receipt must retain the original prepared request");
    };
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .any(|intent| intent == &before));
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(2));
    let _ = received_prompt(&claimed);
    let requests = claimed.transport.requests();
    let observations: Vec<_> = requests.iter().filter(|request|
        request.request["operation"] == "execution_observe").collect();
    assert_eq!(observations.len(), 2);
    assert_eq!(&observations[0].request, request);
    assert_eq!(observations[0].request, observations[1].request);
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1);
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_evaluate").count(), 1);
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_ref(), Some(&before.grant_id));
        assert!(record.engram.source_observation_gate.is_none());
        assert!(record.queued_prompts.is_empty());
    });
    assert!(matches!(original_intent(&claimed).phase, EngramSourceObservationPhase::Recorded { .. }));
    assert!(claimed.runtime_rx.try_recv().is_err(), "exactly one provider command");
}

#[test]
fn source_observation_cancel_retains_captured_recovery_and_never_replays_the_prompt() {
    let claimed = held_observation("source-observation-cancel");
    let before = original_intent(&claimed);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &before.prompt_id).unwrap();
    let retired = original_intent(&claimed);
    assert!(retired.delivery_retired);
    assert_eq!(retired.phase, before.phase);
    assert_eq!(retired.connection, before.connection);
    assert_eq!(retired.routing_token, before.routing_token);
    assert!(retired.grant_settlement.is_none());
    claimed.record(|record| {
        assert!(record.queued_prompts.is_empty(), "cancelled head stays cancelled");
        assert_eq!(record.engram.uncertain_grant_id.as_ref(), Some(&before.grant_id));
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.source_observation_continuation.is_none());
    });
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
    assert!(claimed.runtime_rx.try_recv().is_err());
    // Explicit reconciliation accounts the identical original request, then
    // closes that captured grant. It does not re-admit the cancelled payload.
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let _ = finalized_observation_receipt(&claimed, &before);
    assert!(claimed.runtime_rx.try_recv().is_err());
    let requests = claimed.transport.requests();
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_begin").count(), 1);
    let close = requests.iter().find(|request| request.request["operation"] == "turn_checkpoint").unwrap();
    assert_eq!(close.request["routing_token"], before.routing_token);
    assert_eq!(close.request["grant_id"], before.grant_id);
    assert_eq!(close.request["next_intent"], "exit");
}

#[test]
fn source_observation_lost_owner_does_not_mutate_the_successor_grant_or_delivery_ack() {
    let claimed = held_observation("source-observation-successor");
    let before = original_intent(&claimed);
    claimed.record(|record| {
        record.engram.dispatch_generation += 1;
        record.active_turn_generation += 1;
        record.engram.active_grant_id = Some("successor-grant".to_owned());
        record.engram.uncertain_grant_id = Some("another-recovery-grant".to_owned());
        record.engram.source_opening_disposition = EngramSourceOpeningDisposition::Equal;
        record.engram.source_observation_delivery_grant = Some("successor-grant".to_owned());
    });
    let previous_requests = claimed.transport.requests().len();
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
    assert_eq!(claimed.transport.requests().len(), previous_requests);
    assert!(original_intent(&claimed).delivery_retired);
    claimed.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some("successor-grant"));
        assert_eq!(record.engram.uncertain_grant_id.as_deref(), Some("another-recovery-grant"));
        assert!(record.engram.source_opening_disposition.allows_delivery());
        assert_eq!(record.engram.source_observation_delivery_grant.as_deref(), Some("successor-grant"));
        assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, before.prompt_id);
    });
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_retry_exhaustion_retires_delivery_without_losing_the_obligation() {
    let claimed = held_observation("source-observation-exhausted");
    let before = original_intent(&claimed);
    claimed.record(|record| record.engram.source_observation_gate.as_mut().unwrap().attempts = 8);
    let previous_requests = claimed.transport.requests().len();
    claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
    assert_eq!(claimed.transport.requests().len(), previous_requests);
    let retired = original_intent(&claimed);
    assert!(retired.delivery_retired && retired.grant_settlement.is_none());
    assert_eq!(retired.phase, before.phase);
    claimed.record(|record| {
        assert!(record.engram.source_observation_gate.as_ref().unwrap().retired);
        assert!(record.engram.source_observation_continuation.is_none());
        assert_eq!(record.engram.uncertain_grant_id.as_ref(), Some(&before.grant_id));
        assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, before.prompt_id);
        assert!(!engram_source_observation_can_deliver(record));
    });
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_receipt_and_actual_sqlite_commit_require_the_local_ack_before_handoff() {
    for root_move in [false, true] {
    for fail_ack in [false, true] {
        let label = match (root_move, fail_ack) {
            (false, false) => "source-observation-early-ack",
            (false, true) => "source-observation-ack-retry",
            (true, false) => "source-observation-audit-early-ack",
            (true, true) => "source-observation-audit-ack-retry",
        };
        let mut claimed = opening_fixture(label);
        let dispatch = claimed.dispatch();
        let (tx, rx) = std::sync::mpsc::channel();
        claimed.state.persist_tx = tx;
        let state = claimed.state.clone();
        let session_id = claimed.session_id.clone();
        let task = std::thread::spawn(move || {
            if root_move {
                let move_state = state.clone();
                TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        move_state.name_engram_source_root(&session_id, EngramSourceRootRequest {
                            work: format!("w-{label}"), path: None,
                        }).unwrap();
                    }));
                });
            }
            deliver_turn_dispatch(&state, dispatch)
        });
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        let mut saw_receipt_fence = false;
        while !task.is_finished() {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(PersistRequest::Fence(fence)) => {
                    let recorded = matches!(&fence.target,
                        PersistFenceTarget::EngramSourceObservation { owner, .. }
                            if owner.observations.iter().any(|intent|
                                matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. })));
                    if recorded && !saw_receipt_fence {
                        saw_receipt_fence = true;
                        claimed.record(|record| {
                            assert!(!record.engram.source_opening_disposition.allows_delivery());
                            assert!(record.engram.source_observation_continuation.is_none(),
                                "the early receipt precedes continuation registration");
                        });
                        assert!(claimed.runtime_rx.try_recv().is_err());
                        let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                        persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
                        claimed.record(|record| assert!(!record.engram.source_opening_disposition.allows_delivery()));
                        assert!(claimed.runtime_rx.try_recv().is_err(), "commit alone is not the local ACK");
                        if fail_ack {
                            fence.finish(Err(PersistFenceError::WriteFailed("receipt commit reply was lost".to_owned())));
                            continue;
                        }
                    }
                    batch.accept(PersistRequest::Fence(fence));
                    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("writer fixture disconnected: {error}"),
            }
        }
        assert!(saw_receipt_fence, "must reach an actual receipt durability boundary");
        let outcome = task.join().unwrap();
        if fail_ack {
            assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Held { .. }));
            assert!(claimed.runtime_rx.try_recv().is_err());
            claimed.record(|record| assert!(record.engram.source_observation_continuation.is_some()));
            claimed.state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(2));
            let guard = super::super::super::phase_sync::PollGuard::new();
            loop {
                match claimed.runtime_rx.try_recv() {
                    Ok(CodexRuntimeCommand::Prompt { .. }) => break,
                    Ok(_) => panic!("unexpected provider command"),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(error) => panic!("runtime fixture disconnected: {error}"),
                }
                if let Ok(request) = rx.recv_timeout(Duration::from_millis(20)) {
                    batch.accept(request);
                    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
                    persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
                }
                guard.wait(format_args!("same-grant ACK retry must release the parked continuation"));
            }
        } else {
            assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Delivered));
            let _ = received_prompt(&claimed);
        }
        assert_eq!(claimed.transport.requests().iter().filter(|request|
            request.request["operation"] == "execution_observe").count(), 1,
            "local ACK recovery does not create or resend the accounted observation");
        assert!(claimed.runtime_rx.try_recv().is_err());
        let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
        assert!(saved.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
            .any(|intent| matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. })));
        if root_move {
            assert!(saved.engram_source_sightings.iter().all(|owner| owner.latest.is_none()));
            let intent = original_intent(&claimed);
            let EngramSourceObservationPhase::Recorded { receipt, .. } = intent.phase else { unreachable!() };
            assert_eq!(receipt["accounting"], json!({ "kind": "audit_only", "reason": "root_basis_moved" }));
            claimed.record(|record| assert!(matches!(record.engram.source_opening_disposition,
                EngramSourceOpeningDisposition::HistoricalRootMove { .. })));
        }
        batch.fail(PersistFenceError::Shutdown);
    }
    }
}

#[path = "engram_source_observation_audit.rs"]
mod audit;

#[path = "engram_source_observation_finalization.rs"]
mod finalization;

#[path = "engram_source_observation_delegation.rs"]
mod delegation;

#[path = "engram_source_observation_removal.rs"]
mod removal;

#[path = "engram_source_observation_delete_races.rs"]
mod delete_races;
