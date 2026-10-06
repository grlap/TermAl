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
    claimed.state.select_test_scripted_engram_budget_clock();
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
fn source_observation_unreported_closing_change_does_not_advance_the_accounted_baseline() {
    let label = "source-observation-unreported-close";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();

    // The next turn's grant mediates no local mutation, so the change it
    // makes is measured at its close but its checkpoint carries no change.
    let unreported = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((unreported.clone(), false));
    }
    fs::write(worktree.join("README.md"), "change its checkpoint never reports\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let checkpoint = claimed.transport.requests().into_iter().rev()
        .find(|request| request.request["operation"] == "turn_checkpoint"
            && request.request["idempotency_key"].as_str().is_some_and(|key| key.contains(&unreported)))
        .expect("the unreported turn is checkpointed").request;
    assert!(checkpoint.get("observations").is_none(), "control: the change is not in that checkpoint");

    // Nothing accounted the change, so the next opening must still compare
    // with the last accounted baseline rather than read the change as Equal.
    let next = queue_next_observation_turn(&claimed, label, "after");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let intent = claimed.state.inner.lock().unwrap().engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations).find(|intent| intent.grant_id == next).cloned()
        .expect("an unreported closing change must be accounted at the next opening, not read as Equal");
    assert_eq!(intent.baseline.as_ref(), Some(&baseline));
    assert_ne!(intent.sighting.basis.source_revision, baseline.basis.source_revision);
}

#[test]
fn source_observation_refused_closing_report_does_not_advance_the_accounted_baseline() {
    let label = "source-observation-refused-close";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();

    // The turn reports its own change, but Engram refuses that checkpoint.
    let refused = format!("turn-observation-{label}-refused-grant");
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(&refused), begin_reply(&refused), checkpoint_refusal_reply("evidence_rejected"),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(
        test_control_work_binding(&format!("turn-observation-{label}"), 1))));
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    fs::write(worktree.join("README.md"), "change whose report Engram refuses\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    {
        let inner = claimed.state.inner.lock().unwrap();
        let owner = &inner.engram_source_sightings[0];
        assert_eq!(owner.latest.as_ref(), Some(&baseline), "a refused report accounts nothing");
        // Owed from the measured close, still awaiting the refused grant;
        // the next measured opening reports it from the accounted baseline
        // exactly as it reports a close whose checkpoint carried none.
        let pending = owner.pending_close.as_ref().expect("the refused close stays owed");
        assert_ne!(pending.sighting.basis.source_revision, baseline.basis.source_revision);
        assert_eq!(pending.reported_by.as_deref(), Some(refused.as_str()));
        let plan = engram_source_opening_plan(Some(owner), &pending.sighting);
        assert!(!plan.equal, "an owed close is never read as Equal");
        assert_eq!(plan.baseline.as_ref(), Some(&baseline));
        assert_eq!(&plan.sighting, &pending.sighting);
    }

    // The next turn's admission closes the still-open grant bare and binds
    // again (the fixture answers the status read), then its opening reports
    // the owed change from the accounted baseline.
    let next = format!("turn-observation-{label}-after-grant");
    claimed.transport.responses.lock().unwrap().extend([
        checkpoint_reply(&refused), rebind_reply(&format!("turn-observation-{label}-token")),
        grant_reply(&next), begin_reply(&next), checkpoint_reply(&next),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(
        test_control_work_binding(&format!("turn-observation-{label}"), 1))));
    let outcome = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    let operations = claimed.transport.requests().iter()
        .map(|request| request.request["operation"].as_str().unwrap_or("?").to_owned()).collect::<Vec<_>>();
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Delivered), "{outcome:?}; {operations:?}");
    let _ = received_prompt(&claimed);
    // The refused report is never resent: every later close of that grant
    // went bare.
    let refused_closes = claimed.transport.requests().into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint" && request.request["grant_id"] == refused.as_str())
        .collect::<Vec<_>>();
    assert!(refused_closes.len() >= 2, "{operations:?}");
    assert!(refused_closes.iter().skip(1).all(|close| close.request.get("observations").is_none()));
    let intent = claimed.state.inner.lock().unwrap().engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations).find(|intent| intent.grant_id == next).cloned()
        .unwrap_or_else(|| panic!("a refused closing change must be reported at the next opening: {operations:?}"));
    assert_eq!(intent.baseline.as_ref(), Some(&baseline));
    assert_ne!(intent.sighting.basis.source_revision, baseline.basis.source_revision);
    assert!(claimed.transport.requests().iter().any(|request| request.request["operation"] == "execution_observe"
        && request.request["idempotency_key"] == format!("termal-observe:{}", intent.id).as_str()),
        "the owed change reaches Engram as an inter-turn observation");
}

fn execution_observe_count(claimed: &ClaimedRoot) -> usize {
    claimed.transport.requests().iter()
        .filter(|request| request.request["operation"] == "execution_observe").count()
}

#[test]
fn source_observation_accepted_closing_change_is_the_next_equal_baseline() {
    let label = "source-observation-accepted-close";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    fs::write(worktree.join("README.md"), "change its checkpoint reports\n").unwrap();
    let observation = finish_claimed_turn(&claimed, &runtime);
    assert_eq!(observation["source_changed"], true, "the turn's own observation carries the change");
    let closed = {
        let inner = claimed.state.inner.lock().unwrap();
        let owner = &inner.engram_source_sightings[0];
        assert!(owner.pending_close.is_none(), "a granted report accounts its close");
        owner.latest.clone().unwrap()
    };
    assert_eq!(closed.basis.source_revision, observation["source_basis"]["source_revision"].as_str().unwrap());

    // The next opening at that revision is Equal: nothing is accounted twice.
    let observed = execution_observe_count(&claimed);
    let next = queue_next_observation_turn(&claimed, label, "equal");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    assert_eq!(execution_observe_count(&claimed), observed);
    assert!(!claimed.state.inner.lock().unwrap().engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations).any(|intent| intent.grant_id == next));

    // An unchanged close leaves the baseline where it was, with nothing owed.
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert!(owner.pending_close.is_none());
    assert_eq!(owner.latest.as_ref().map(|latest| &latest.basis), Some(&closed.basis));
}

#[test]
fn source_observation_owed_close_survives_a_persist_and_reload() {
    let label = "source-observation-owed-reload";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    let unreported = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((unreported.clone(), false));
    }
    fs::write(worktree.join("README.md"), "change owed across a restart\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let owed = claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone()
        .expect("the unreported close is owed");

    let restored = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    let owner = restored.engram_source_sightings.iter()
        .find(|owner| owner.scope == claimed.state.inner.lock().unwrap().engram_source_sightings[0].scope)
        .expect("the owner is persisted");
    assert_eq!(owner.latest.as_ref(), Some(&baseline), "the accounted baseline is persisted unchanged");
    assert_eq!(owner.pending_close.as_ref(), Some(&owed), "the owed close is persisted");
    // A restarted opening still reports from them: the owed change from the
    // baseline, and then, once the tree is back, the change from the owed
    // close to the opening.
    let plan = engram_source_opening_plan(Some(owner), &owed.sighting);
    assert!(!plan.equal);
    assert_eq!(plan.baseline.as_ref(), Some(&baseline));
    assert_eq!(&plan.sighting, &owed.sighting);
    assert!(plan.follow_up.is_none());
    let back = EngramSourceSighting { basis: baseline.basis.clone(), observed_at: chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true) };
    let plan = engram_source_opening_plan(Some(owner), &back);
    assert!(!plan.equal);
    assert_eq!(plan.baseline.as_ref(), Some(&baseline));
    assert_eq!(&plan.sighting, &owed.sighting);
    assert_eq!(plan.follow_up.as_ref(), Some(&back));
}

#[test]
fn source_observation_change_accepted_on_one_claim_leaves_the_other_claims_baseline() {
    let label = "source-observation-two-claims";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    // A second live claim names the same root at a later generation and has
    // accounted the same revision.
    let other_scope = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let mut other = inner.engram_source_sightings[0].clone();
        other.scope.work_id.push_str("-other");
        other.scope.run_id.push_str("-other");
        other.scope.claim_id.push_str("-other");
        other.scope.source_root_generation = other.scope.source_root_generation.map(|generation| generation + 1);
        other.version = 1;
        let scope = other.scope.clone();
        inner.engram_source_sightings.push(other);
        scope
    };
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();

    // This claim's turn changes the root and its granted report accounts it.
    let next = queue_next_observation_turn(&claimed, label, "accepted");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    fs::write(worktree.join("README.md"), "change accepted on one claim\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    let own = inner.engram_source_sightings.iter().find(|owner| owner.scope != other_scope).unwrap();
    let advanced = own.latest.clone().unwrap();
    assert_ne!(advanced.basis.source_revision, baseline.basis.source_revision, "control: this claim advanced");
    assert!(own.pending_close.is_none(), "{next} accounted its close");

    let other = inner.engram_source_sightings.iter().find(|owner| owner.scope == other_scope).unwrap();
    assert_eq!(other.latest.as_ref(), Some(&baseline), "another claim's baseline never moves with this one");
    assert!(other.pending_close.is_none());
    let sighting = EngramSourceSighting { basis: advanced.basis.clone(), observed_at: advanced.observed_at.clone() };
    let plan = engram_source_opening_plan(Some(other), &sighting);
    assert!(!plan.equal, "the other claim's next opening accounts the change");
    assert_eq!(plan.baseline.as_ref(), Some(&baseline));
    assert_eq!(&plan.sighting, &sighting);
}

#[test]
fn source_observation_unreported_closing_excursion_is_reported_after_a_revert() {
    let label = "source-observation-unreported-excursion";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    let original = fs::read(worktree.join("README.md")).unwrap();

    // A turn whose grant mediates no mutation changes the tree; its close
    // measures the change, but its checkpoint carries none.
    let unreported = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((unreported.clone(), false));
    }
    fs::write(worktree.join("README.md"), "an excursion its checkpoint never reports\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();

    // The tree returns to the accounted revision before the next opening.
    fs::write(worktree.join("README.md"), &original).unwrap();
    let observed = execution_observe_count(&claimed);
    let next = queue_next_observation_turn(&claimed, label, "after");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    // Two observations, in this order: the owed change itself, from the
    // accounted baseline to the measured close, then the revert from that
    // close to the opening.
    let inner = claimed.state.inner.lock().unwrap();
    let reported = inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .filter(|intent| intent.grant_id == next).cloned().collect::<Vec<_>>();
    assert_eq!(reported.len(), 2, "a measured but unaccounted excursion must be reported, not lost to a revert");
    let (owed, revert) = (&reported[0], &reported[1]);
    assert_eq!(owed.baseline.as_ref(), Some(&baseline));
    assert_ne!(owed.sighting.basis.source_revision, baseline.basis.source_revision, "the owed close itself");
    assert!(matches!(owed.phase, EngramSourceObservationPhase::Recorded { .. }));
    assert_eq!(revert.baseline.as_ref(), Some(&owed.sighting), "the revert is reported from the owed close");
    assert_eq!(revert.sighting.basis.source_revision, baseline.basis.source_revision,
        "the tree is back at the accounted revision");
    assert!(matches!(revert.phase, EngramSourceObservationPhase::Recorded { .. }));
    assert_eq!(execution_observe_count(&claimed), observed + 2);
    let owner = inner.engram_source_sightings.iter().find(|owner| owner.observations.iter().any(|intent| intent.id == revert.id)).unwrap();
    assert!(owner.pending_close.is_none(), "recording the owed change clears it");
    assert_eq!(owner.latest.as_ref().map(|latest| &latest.basis), Some(&revert.sighting.basis));
}

#[test]
fn source_observation_lost_close_accepted_by_recovery_is_the_next_equal_baseline() {
    let label = "source-observation-lost-close";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();

    // The turn reports its own change, but the checkpoint's reply is lost.
    let lost = format!("turn-observation-{label}-lost-grant");
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(&lost), begin_reply(&lost),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport("checkpoint reply lost"))),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(
        test_control_work_binding(&format!("turn-observation-{label}"), 1))));
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    fs::write(worktree.join("README.md"), "change whose checkpoint reply is lost\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    {
        let inner = claimed.state.inner.lock().unwrap();
        let owner = &inner.engram_source_sightings[0];
        assert_eq!(owner.latest.as_ref(), Some(&baseline), "an unanswered checkpoint accounts nothing yet");
        assert_eq!(owner.pending_close.as_ref().and_then(|pending| pending.reported_by.as_deref()), Some(lost.as_str()));
    }

    // The next turn's recovery closes the grant with the report this process
    // kept, Engram accepts it, and the opening at that revision is Equal.
    let next = format!("turn-observation-{label}-after-grant");
    claimed.transport.responses.lock().unwrap().extend([
        checkpoint_reply(&lost), rebind_reply(&format!("turn-observation-{label}-token")),
        grant_reply(&next), begin_reply(&next), checkpoint_reply(&next),
    ]);
    claimed.transport.work_bindings.lock().unwrap().push_back(Ok(Some(
        test_control_work_binding(&format!("turn-observation-{label}"), 1))));
    let observed = execution_observe_count(&claimed);
    // The lost call armed the bind retry's backoff; let it elapse, as the
    // rebind-after-a-lost-close test does, and dispatch the next prompt.
    claimed.record(|record| record.engram.next_bind_retry_at = None);
    queue_test_engram_prompt(&claimed.state, &claimed.session_id,
        "Continue after the lost close.", QueuedPromptSource::User, None);
    let dispatch = claimed.state.start_next_queued_turn_off_lock(&claimed.session_id, false, false)
        .expect("the queue should be inspected")
        .expect("the queued prompt should rebind and dispatch")
        .dispatch;
    let outcome = deliver_turn_dispatch(&claimed.state, dispatch);
    let operations = claimed.transport.requests().iter()
        .map(|request| request.request["operation"].as_str().unwrap_or("?").to_owned()).collect::<Vec<_>>();
    let preview = claimed.record(|record| record.session.preview.clone());
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Delivered), "{outcome:?}; {preview}; {operations:?}");
    let _ = received_prompt(&claimed);
    let recovery = claimed.transport.requests().into_iter().rev()
        .find(|request| request.request["operation"] == "turn_checkpoint" && request.request["grant_id"] == lost.as_str())
        .expect("the lost grant is closed by recovery").request;
    assert!(recovery.get("observations").is_some(), "the recovery carries the kept report: {recovery}");
    assert_eq!(execution_observe_count(&claimed), observed, "an accepted change is not reported twice: {operations:?}");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .any(|intent| intent.grant_id == next));
    let owner = &inner.engram_source_sightings[0];
    assert!(owner.pending_close.is_none());
    assert_ne!(owner.latest.as_ref().unwrap().basis.source_revision, baseline.basis.source_revision);
}

#[test]
fn source_observation_owed_close_at_the_accounted_revision_owes_nothing() {
    let label = "source-observation-clock-step";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let mut owner = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let latest = owner.latest.clone().unwrap();
    // A close at the accounted revision stamped later than the observation
    // that recorded it, as after the clock stepped back between them.
    owner.pending_close = Some(EngramPendingClose {
        sighting: EngramSourceSighting { basis: latest.basis.clone(), observed_at: "2999-01-01T00:00:00.000Z".to_owned() },
        reported_by: None,
    });
    let plan = engram_source_opening_plan(Some(&owner), &latest);
    assert!(plan.equal, "a close at the accounted revision owes nothing");
    assert_eq!(&plan.sighting, &latest);
    assert!(plan.follow_up.is_none());
    assert!(settle_engram_pending_close_by_observation(&mut owner, &latest, true), "its recorded revision accounts it");
    assert!(owner.pending_close.is_none());
}

#[test]
fn source_observation_granted_close_settles_even_after_the_session_moved_on() {
    let label = "source-observation-moved-grant";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let (closed, card) = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let card = inner.sessions[index].session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } if matches!(card.stage, EngramControlStage::Checkpoint) => Some(card.clone()),
            _ => None,
        }).expect("the first turn's checkpoint card");
        let latest = inner.engram_source_sightings[0].latest.clone().unwrap();
        let closed = EngramSourceSighting {
            basis: EngramExecutionSourceBasis { source_revision: "content-v1:closed".to_owned(), ..latest.basis.clone() },
            observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        };
        inner.engram_source_sightings[0].pending_close = Some(EngramPendingClose {
            sighting: closed.clone(), reported_by: Some("moved-grant".to_owned()),
        });
        // A reset or revocation moved the session on before the answer came.
        inner.sessions[index].engram.active_grant_id = Some("successor-grant".to_owned());
        (closed, card)
    };
    claimed.state.finish_engram_checkpoint_record(
        &claimed.session_id, "moved-grant", EngramControlCardDecision::Grant, card, None, true);
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert!(owner.pending_close.is_none(), "what Engram accepted for the grant settles its close");
    assert_eq!(owner.latest.as_ref(), Some(&closed));
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    assert_eq!(inner.sessions[index].engram.active_grant_id.as_deref(), Some("successor-grant"));
}

#[test]
fn source_observation_retired_follow_up_stays_owed_after_recovery() {
    let label = "source-observation-retired-follow-up";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    let original = fs::read(worktree.join("README.md")).unwrap();
    let unreported = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((unreported.clone(), false));
    }
    fs::write(worktree.join("README.md"), "an excursion recovery must not lose\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    fs::write(worktree.join("README.md"), &original).unwrap();

    // The next opening reports the owed close first, but that reply is lost,
    // and the prompt is cancelled before the follow-up can run.
    let next = queue_next_observation_turn(&claimed, label, "held");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Held { .. }));
    let first = claimed.state.inner.lock().unwrap().engram_source_sightings.iter()
        .flat_map(|owner| &owner.observations).find(|intent| intent.grant_id == next).cloned().unwrap();
    assert_eq!(first.baseline.as_ref(), Some(&baseline));
    let follow_up = first.follow_up.clone().expect("the revert to the accounted revision is the follow-up");
    assert_eq!(follow_up.basis.source_revision, baseline.basis.source_revision);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &first.prompt_id).unwrap();
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    assert!(claimed.runtime_rx.try_recv().is_err(), "the cancelled prompt is never delivered");

    // Recovery recorded the owed close; the change from it back to the
    // opening was measured but never reported, so it is owed in turn.
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert_eq!(owner.latest.as_ref(), Some(&first.sighting), "the recorded owed close is the baseline");
    assert_eq!(owner.pending_close.as_ref().map(|pending| &pending.sighting), Some(&follow_up),
        "the unreported follow-up stays owed after recovery");
    let plan = engram_source_opening_plan(Some(owner), &follow_up);
    assert!(!plan.equal);
    assert_eq!(plan.baseline.as_ref(), Some(&first.sighting));
    assert_eq!(&plan.sighting, &follow_up);
}

#[test]
fn source_observation_owed_interval_always_runs_forward_in_time() {
    let label = "source-observation-forward-interval";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let mut owner = claimed.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let latest = owner.latest.clone().unwrap();
    let at = |basis: &EngramExecutionSourceBasis, observed_at: &str| EngramSourceSighting {
        basis: basis.clone(), observed_at: observed_at.to_owned(),
    };
    let changed = EngramExecutionSourceBasis { source_revision: "content-v1:owed".to_owned(), ..latest.basis.clone() };
    // Another session's later close refreshed the baseline past the owed one.
    owner.latest = Some(at(&latest.basis, "2026-10-05T12:00:00.000Z"));
    owner.pending_close = Some(EngramPendingClose { sighting: at(&changed, "2026-10-05T11:00:00.000Z"), reported_by: None });
    let opening = at(&latest.basis, "2026-10-05T13:00:00.000Z");
    let plan = engram_source_opening_plan(Some(&owner), &opening);
    assert!(!plan.equal);
    assert!(plan.baseline.is_none(), "a baseline measured after the owed close cannot start it");
    assert_eq!(plan.sighting.basis, changed);
    assert_eq!(plan.follow_up.as_ref(), Some(&opening));
    let owed = owner.pending_close.clone().unwrap().sighting;
    assert_eq!(engram_follow_up_baseline(&owed, &opening).as_ref(), Some(&owed),
        "a follow-up is reported from the close it follows");
    // An opening is always measured after the close it follows was kept, so
    // one the clock stamps earlier is still reported, never dropped, and goes
    // without a baseline rather than as a backward interval.
    let early = at(&latest.basis, "2026-10-05T10:00:00.000Z");
    assert_eq!(engram_source_opening_plan(Some(&owner), &early).follow_up.as_ref(), Some(&early));
    assert!(engram_follow_up_baseline(&owed, &early).is_none());
}

/// Runs a turn whose grant mediates no mutation and which writes `content`,
/// so its close is measured but its checkpoint carries no observation.
fn unreported_turn(claimed: &ClaimedRoot, worktree: &std::path::Path, label: &str, step: &str, content: &str) -> String {
    let grant = queue_next_observation_turn(claimed, label, step);
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((grant.clone(), false));
    }
    fs::write(worktree.join("README.md"), content).unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    grant
}

/// The opening observations reported for `grant`, in order.
fn reported_for(claimed: &ClaimedRoot, grant: &str) -> Vec<EngramSourceObservationIntent> {
    claimed.state.inner.lock().unwrap().engram_source_sightings.iter().flat_map(|owner| &owner.observations)
        .filter(|intent| intent.grant_id == grant).cloned().collect()
}

#[test]
fn source_observation_close_behind_a_later_stamped_baseline_stays_owed() {
    let label = "source-observation-clock-behind";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let original = fs::read(worktree.join("README.md")).unwrap();
    let grant = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    // During the turn the accounted baseline comes to carry a stamp later
    // than the close that follows, as after the clock was stepped back.
    let baseline = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((grant, false));
        let owner = &mut inner.engram_source_sightings[0];
        owner.latest.as_mut().unwrap().observed_at = (chrono::Utc::now() + chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        owner.latest.clone().unwrap()
    };
    fs::write(worktree.join("README.md"), "an excursion closed behind the clock\n").unwrap();
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let owed = claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone()
        .expect("a differing close is owed even behind a later-stamped baseline");
    assert_ne!(owed.sighting.basis.source_revision, baseline.basis.source_revision);
    assert_eq!(claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.as_ref(), Some(&baseline));

    // The tree reverts before the next opening; both changes still reach Engram.
    fs::write(worktree.join("README.md"), &original).unwrap();
    let next = queue_next_observation_turn(&claimed, label, "after");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let reported = reported_for(&claimed, &next);
    assert_eq!(reported.len(), 2, "the excursion and its revert are reported, not lost to the clock");
    assert!(reported[0].baseline.is_none(), "a later-stamped baseline cannot start the owed interval");
    assert_eq!(reported[0].sighting, owed.sighting);
    assert_eq!(reported[1].baseline.as_ref(), Some(&owed.sighting));
    assert_eq!(reported[1].sighting.basis.source_revision, baseline.basis.source_revision);
}

#[test]
fn source_observation_opening_never_displaces_a_baseline_accounted_after_its_measurement() {
    let label = "source-observation-competing-capture";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    fs::write(worktree.join("README.md"), "the revision this opening measures\n").unwrap();
    // Between this opening's measurement and its capture, another session on
    // the scope accounts a newer revision.
    let state = claimed.state.clone();
    let competing = Arc::new(Mutex::new(None::<EngramSourceSighting>));
    let captured = competing.clone();
    TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        std::thread::sleep(Duration::from_millis(5));
        let mut inner = state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        let latest = owner.latest.as_mut().unwrap();
        latest.basis.source_revision = "content-v1:accounted-by-another-session".to_owned();
        latest.observed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        *captured.lock().unwrap() = Some(latest.clone());
        owner.version += 1;
    })));
    let _ = queue_next_observation_turn(&claimed, label, "after");
    // The opening's interval would end before its baseline's stamp, which
    // the protocol refuses, so it is held, as before this change. Whatever
    // its outcome, it must not displace the newer baseline.
    let _ = deliver_turn_dispatch(&claimed.state, claimed.dispatch());
    let newer = competing.lock().unwrap().clone().expect("the competing accounting ran");
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert_eq!(owner.latest.as_ref(), Some(&newer), "a stale opening never displaces a newer accounted baseline");
}

/// A sighting of the claimed root at `revision`, stamped `observed_at`.
fn sighting_at(claimed: &ClaimedRoot, revision: &str, observed_at: &str) -> EngramSourceSighting {
    let latest = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    EngramSourceSighting {
        basis: EngramExecutionSourceBasis { source_revision: revision.to_owned(), ..latest.basis },
        observed_at: observed_at.to_owned(),
    }
}

/// Retains `close` as the delivered turn's closing measurement.
fn retain_close(claimed: &ClaimedRoot, close: &EngramSourceSighting, reported_by: Option<&str>) {
    let mut inner = claimed.state.inner.lock().unwrap();
    let index = inner.find_session_index(&claimed.session_id).unwrap();
    let _ = retain_engram_closing_sighting_locked(&mut inner, index, &close.basis, &close.observed_at, reported_by);
}

fn owed_close(claimed: &ClaimedRoot) -> Option<EngramPendingClose> {
    claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone()
}

#[test]
fn source_observation_owed_slot_keeps_the_newer_measurement_as_the_baseline_would() {
    let label = "source-observation-newer-close";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let _ = queue_next_observation_turn(&claimed, label, "closing");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let c100 = sighting_at(&claimed, "content-v1:close-c", "2026-10-05T10:00:00.100Z");
    let d200 = sighting_at(&claimed, "content-v1:close-d", "2026-10-05T10:00:00.200Z");
    retain_close(&claimed, &c100, Some("grant-c"));
    // A newer close takes the slot with its own grant, as the baseline keeps
    // the newer close: the next opening reports it, then any change from it.
    retain_close(&claimed, &d200, None);
    let owed = owed_close(&claimed).unwrap();
    assert_eq!(owed.sighting, d200);
    assert!(owed.reported_by.is_none(), "the slot carries the newer close's own grant");
    // A delayed older close never takes it back.
    retain_close(&claimed, &sighting_at(&claimed, "content-v1:close-c", "2026-10-05T10:00:00.150Z"), None);
    assert_eq!(owed_close(&claimed).unwrap().sighting, d200);
    // A newer measurement at the same revision advances the stamp, so a
    // delayed close stamped in between cannot win either.
    let d300 = sighting_at(&claimed, "content-v1:close-d", "2026-10-05T10:00:00.300Z");
    retain_close(&claimed, &d300, Some("grant-d"));
    assert_eq!(owed_close(&claimed).unwrap(), EngramPendingClose { sighting: d300.clone(), reported_by: Some("grant-d".to_owned()) });
    retain_close(&claimed, &sighting_at(&claimed, "content-v1:close-c", "2026-10-05T10:00:00.250Z"), None);
    assert_eq!(owed_close(&claimed).unwrap().sighting, d300);
    // An exact duplicate changes nothing.
    let version = claimed.state.inner.lock().unwrap().engram_source_sightings[0].version;
    retain_close(&claimed, &d300, None);
    assert_eq!(claimed.state.inner.lock().unwrap().engram_source_sightings[0].version, version);
    assert_eq!(owed_close(&claimed).unwrap().reported_by.as_deref(), Some("grant-d"));
}

#[test]
fn source_observation_older_grant_never_settles_a_newer_owed_close() {
    let label = "source-observation-older-grant";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let _ = queue_next_observation_turn(&claimed, label, "closing");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    retain_close(&claimed, &sighting_at(&claimed, "content-v1:close-c", "2026-10-05T10:00:00.100Z"), Some("grant-c"));
    let d200 = sighting_at(&claimed, "content-v1:close-d", "2026-10-05T10:00:00.200Z");
    retain_close(&claimed, &d200, Some("grant-d"));
    // The older close's Grant arrives late: Engram has its own accounting of
    // that close, and the newer duty stays owed.
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        assert!(!settle_engram_pending_close_by_checkpoint_locked(&mut inner, "grant-c", true));
    }
    assert_eq!(owed_close(&claimed).unwrap().sighting, d200);
    // Only the newer close's own accepted report releases it, even though a
    // later measurement keeps it from becoming the baseline.
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        assert!(settle_engram_pending_close_by_checkpoint_locked(&mut inner, "grant-d", true));
    }
    assert!(owed_close(&claimed).is_none(), "an accepted report settles its duty");
}

/// Runs the retired-follow-up recovery with a close owed meanwhile, stamped
/// by `stamp` from the recorded owed close and the follow-up's stamps, and
/// returns what is owed afterwards with the recovered intent.
fn retired_follow_up_meets_an_owed_close(
    label: &str, stamp: fn(&str, &str) -> String,
) -> (ClaimedRoot, EngramSourceObservationIntent, EngramPendingClose) {
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let original = fs::read(worktree.join("README.md")).unwrap();
    unreported_turn(&claimed, &worktree, label, "unreported", "an excursion recovery meets a newer close on\n");
    fs::write(worktree.join("README.md"), &original).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let next = queue_next_observation_turn(&claimed, label, "held");
    claimed.transport.named_roots.lock().unwrap().as_mut().unwrap().lose_next_observation_reply = true;
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Held { .. }));
    let first = reported_for(&claimed, &next).into_iter().next().unwrap();
    let follow_up = first.follow_up.clone().expect("the revert is the follow-up");
    claimed.state.cancel_queued_prompt(&claimed.session_id, &first.prompt_id).unwrap();
    // Another close is owed on the scope before recovery runs.
    let meanwhile = sighting_at(&claimed, "content-v1:owed-meanwhile",
        &stamp(&first.sighting.observed_at, &follow_up.observed_at));
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        owner.pending_close = Some(EngramPendingClose { sighting: meanwhile, reported_by: None });
        owner.version += 1;
    }
    claimed.state.reconcile_retired_source_observations(&claimed.session_id).unwrap();
    let owed = owed_close(&claimed).expect("something stays owed");
    (claimed, first, owed)
}

fn shift(stamp: &str, by: chrono::Duration) -> String {
    (chrono::DateTime::parse_from_rfc3339(stamp).unwrap() + by).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[test]
fn source_observation_retired_follow_up_takes_the_slot_from_an_older_owed_close() {
    // Owed after the recorded close but measured before the follow-up.
    let (_claimed, first, owed) = retired_follow_up_meets_an_owed_close(
        "source-observation-follow-up-newer", |close, _| shift(close, chrono::Duration::milliseconds(1)));
    assert_eq!(Some(&owed.sighting), first.follow_up.as_ref(), "the newer follow-up is owed, not the older close");
    assert!(owed.reported_by.is_none());
}

#[test]
fn source_observation_retired_finalization_never_clears_a_newer_owed_close() {
    // Owed and measured after the follow-up.
    let (_claimed, first, owed) = retired_follow_up_meets_an_owed_close(
        "source-observation-owed-newer", |_, follow_up| shift(follow_up, chrono::Duration::hours(1)));
    assert_eq!(owed.sighting.basis.source_revision, "content-v1:owed-meanwhile",
        "neither the recovered close nor its older follow-up clears the newer duty");
    assert_ne!(Some(&owed.sighting), first.follow_up.as_ref());
}

#[test]
fn source_observation_owed_close_drains_and_a_later_excursion_is_still_reported() {
    let label = "source-observation-drain";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let original = fs::read(worktree.join("README.md")).unwrap();
    // A close at the tree's revision is owed behind a newer baseline at
    // another revision.
    let now = chrono::Utc::now();
    let tree = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        owner.latest = Some(EngramSourceSighting {
            basis: EngramExecutionSourceBasis { source_revision: "content-v1:newer-elsewhere".to_owned(), ..tree.basis.clone() },
            observed_at: (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
        owner.pending_close = Some(EngramPendingClose {
            sighting: EngramSourceSighting { basis: tree.basis.clone(),
                observed_at: (now - chrono::Duration::seconds(2)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true) },
            reported_by: None,
        });
        owner.version += 1;
    }
    // The next opening reports the owed close once, and its accounting
    // settles it.
    let first = queue_next_observation_turn(&claimed, label, "first");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    assert_eq!(reported_for(&claimed, &first).len(), 1);
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    // It drains: the following unchanged openings report nothing more.
    for step in ["second", "third"] {
        let observed = execution_observe_count(&claimed);
        let grant = queue_next_observation_turn(&claimed, label, step);
        assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
        let _ = received_prompt(&claimed);
        assert_eq!(execution_observe_count(&claimed), observed, "the old duty does not re-report at {step}");
        assert!(reported_for(&claimed, &grant).is_empty());
        claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    }
    assert!(owed_close(&claimed).is_none());
    // A later unreported excursion, reverted before the next opening, is
    // still reported.
    unreported_turn(&claimed, &worktree, label, "excursion", "a later excursion the drained slot must keep\n");
    fs::write(worktree.join("README.md"), &original).unwrap();
    let after = queue_next_observation_turn(&claimed, label, "after");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let reported = reported_for(&claimed, &after);
    assert_eq!(reported.len(), 2, "the excursion and its revert are reported");
    assert_eq!(reported[1].sighting.basis, tree.basis);
}

#[test]
fn source_observation_close_left_owed_at_the_baseline_revision_never_blocks_a_new_one() {
    let label = "source-observation-stale-owed";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        let latest = owner.latest.clone().unwrap();
        // As a retired observation's finalization can leave it.
        owner.pending_close = Some(EngramPendingClose {
            sighting: EngramSourceSighting { basis: latest.basis, observed_at: chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true) },
            reported_by: None,
        });
    }
    unreported_turn(&claimed, &worktree, label, "unreported", "a change a stale owed close must not hold back\n");
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    let owed = owner.pending_close.as_ref().expect("the new close is owed");
    assert_ne!(Some(&owed.sighting.basis), owner.latest.as_ref().map(|latest| &latest.basis),
        "the close measured now replaced the one at the baseline's revision");
}

#[test]
fn source_observation_repeated_excursion_to_the_same_revision_stays_owed() {
    let label = "source-observation-repeated-excursion";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let original = fs::read(worktree.join("README.md")).unwrap();
    let excursion = "the same excursion, twice\n";
    unreported_turn(&claimed, &worktree, label, "first", excursion);
    let first = claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone().unwrap();
    fs::write(worktree.join("README.md"), &original).unwrap();

    // The next turn's opening reports that excursion and its revert; the turn
    // then writes the very same content again, unreported, and its granted
    // checkpoint finalizes the delivered observations.
    let observed = execution_observe_count(&claimed);
    let second_grant = unreported_turn(&claimed, &worktree, label, "second", excursion);
    assert_eq!(execution_observe_count(&claimed), observed + 2,
        "the second turn's opening reported the excursion and its revert");
    let reported = reported_for(&claimed, &second_grant);
    assert!(reported.iter().all(|intent| intent.finalization_complete),
        "the second turn's observations were finalized by its checkpoint");
    let owed = claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone()
        .expect("finalizing the earlier report must not clear the newer close at its revision");
    assert_eq!(owed.sighting.basis, first.sighting.basis, "the same content gives the same revision");
    assert_ne!(owed.sighting, first.sighting, "the newer close, not the reported one");

    // It reverts again before the next opening, which still reports it.
    fs::write(worktree.join("README.md"), &original).unwrap();
    let next = queue_next_observation_turn(&claimed, label, "after");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let reported = reported_for(&claimed, &next);
    assert_eq!(reported.len(), 2, "the repeated excursion is reported again, not lost");
    assert_eq!(reported[0].sighting, owed.sighting);
}

#[test]
fn source_observation_granted_close_leaves_an_owner_a_live_gate_fenced() {
    let label = "source-observation-live-gate";
    let (claimed, _worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let card = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        let card = inner.sessions[index].session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } if matches!(card.stage, EngramControlStage::Checkpoint) => Some(card.clone()),
            _ => None,
        }).unwrap();
        let latest = inner.engram_source_sightings[0].latest.clone().unwrap();
        inner.engram_source_sightings[0].pending_close = Some(EngramPendingClose {
            sighting: EngramSourceSighting {
                basis: EngramExecutionSourceBasis { source_revision: "content-v1:closed".to_owned(), ..latest.basis.clone() },
                observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            },
            reported_by: Some("fenced-grant".to_owned()),
        });
        let scope = inner.engram_source_sightings[0].scope.clone();
        let owner_version = inner.engram_source_sightings[0].version;
        // A successor's opening gate is live on this owner's scope.
        inner.sessions[index].engram.source_observation_gate = Some(EngramSourceObservationGate {
            scope, observation_id: "successor-observation".to_owned(), owner_version,
            prompt_id: "successor-prompt".to_owned(), dispatch_generation: 0, active_turn_generation: 0,
            grant_id: "successor-grant".to_owned(), attempts: 0, policy_refreshed: false,
            retry_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            reason: "successor opening".to_owned(), retired: false,
        });
        inner.sessions[index].engram.active_grant_id = Some("successor-grant".to_owned());
        card
    };
    let version = claimed.state.inner.lock().unwrap().engram_source_sightings[0].version;
    claimed.state.finish_engram_checkpoint_record(
        &claimed.session_id, "fenced-grant", EngramControlCardDecision::Grant, card, None, true);
    let inner = claimed.state.inner.lock().unwrap();
    let owner = &inner.engram_source_sightings[0];
    assert_eq!(owner.version, version, "a live gate's owner version is not moved under it");
    assert!(owner.pending_close.is_some(), "the gate's own recorded observation will account the close");
}

#[test]
fn source_observation_owed_close_is_durable_before_its_checkpoint_is_sent() {
    let label = "source-observation-owed-durable";
    let (claimed, worktree, runtime) = named_root_turn(label, true);
    finish_claimed_turn(&claimed, &runtime);
    let baseline = claimed.state.inner.lock().unwrap().engram_source_sightings[0].latest.clone().unwrap();
    let unreported = queue_next_observation_turn(&claimed, label, "unreported");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&claimed.session_id).unwrap();
        inner.sessions[index].engram.active_turn_grant_mutates = Some((unreported.clone(), false));
    }
    fs::write(worktree.join("README.md"), "change a crash during the checkpoint must not lose\n").unwrap();

    // What is on disk the moment the checkpoint request leaves is all a
    // crash during that call would leave behind.
    let persistence_path = claimed.state.persistence_path.clone();
    let on_disk = Arc::new(Mutex::new(None::<Option<EngramPendingClose>>));
    let captured = on_disk.clone();
    TEST_ENGRAM_BEFORE_TURN_CHECKPOINT_REQUEST.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        let restored = load_state(persistence_path.as_path()).unwrap().unwrap();
        *captured.lock().unwrap() = Some(restored.engram_source_sightings.first().and_then(|owner| owner.pending_close.clone()));
    })));
    claimed.state.finish_turn_ok_if_runtime_matches(&claimed.session_id, &claimed.runtime_token()).unwrap();
    let owed = claimed.state.inner.lock().unwrap().engram_source_sightings[0].pending_close.clone()
        .expect("the unreported close is owed");
    assert_ne!(owed.sighting.basis.source_revision, baseline.basis.source_revision);
    let durable = on_disk.lock().unwrap().clone().expect("the checkpoint request was sent");
    assert_eq!(durable.as_ref(), Some(&owed), "the owed close is durable before its checkpoint is sent");
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
