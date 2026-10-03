//! Durability and public Resume completion after a retired observation.
use super::*;

#[test]
fn source_observation_compaction_keeps_live_gate_and_policy_chain_until_transfer() {
    let claimed = opening_fixture("source-observation-policy-compaction");
    {
        let mut roots = claimed.transport.named_roots.lock().unwrap();
        let roots = roots.as_mut().unwrap();
        roots.refuse_next_observation_policy = true;
        roots.lose_next_observation_reply = true;
    }
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Held { .. }));
    let target = {
        let inner = claimed.state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true).unwrap().unwrap()
    };
    // The production transition follows a typed definitive refusal. The
    // replacement's committed reply is then lost, so both remain unresolved.
    let pending = {
        let mut inner = claimed.state.inner.lock().unwrap();
        let pending = inner.engram_source_sightings[0].observations.clone();
        claimed.state.compact_source_observations_locked(&mut inner).unwrap();
        assert_eq!(inner.engram_source_sightings[0].observations, pending);
        pending
    };
    let refused = pending.iter().find(|intent| matches!(intent.phase, EngramSourceObservationPhase::RefusedPolicy { .. })).unwrap();
    let EngramSourceObservationPhase::RefusedPolicy { request, .. } = &refused.phase else { panic!("retained predecessor"); };
    let wires = claimed.transport.requests();
    let original_request = &wires.iter().find(|request| request.request["operation"] == "execution_observe").unwrap().request;
    assert_eq!(request, original_request);
    claimed.state.advance_engram_source_observation(&claimed.session_id, &target,
        target.dispatch_deadline(target.budget_clock.now())).unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let before = inner.engram_source_sightings[0].observations.clone();
        claimed.state.compact_source_observations_locked(&mut inner).unwrap();
        assert_eq!(inner.engram_source_sightings[0].observations, before,
            "a recorded live-gate intent still owes its delivery transfer");
    }
    claimed.state.release_source_observation_continuation(&claimed.session_id);
    let _ = received_prompt(&claimed);
    finish_claimed_turn(&claimed, &claimed.runtime_token());
    assert!(claimed.state.inner.lock().unwrap().engram_source_sightings[0].observations.is_empty(),
        "completed replacement and linked refused payloads compact together");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count(), 1);
}

#[test]
fn source_observation_normal_close_compacts_completed_payload_and_keeps_pending_bytes() {
    let claimed = opening_fixture("source-observation-normal-finalization");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let completed = original_intent(&claimed);
    let pending = held_observation("source-observation-unrelated-pending");
    let pending_owner = pending.state.inner.lock().unwrap().engram_source_sightings[0].clone();
    let pending_bytes = serde_json::to_vec(&pending_owner).unwrap();
    claimed.state.inner.lock().unwrap().engram_source_sightings.push(pending_owner.clone());
    let runtime = claimed.runtime_token();
    finish_claimed_turn(&claimed, &runtime);
    let inner = claimed.state.inner.lock().unwrap();
    let owner = inner.engram_source_sightings.iter().find(|owner|
        owner.scope.workspace_id == completed.sighting.basis.workspace_id).unwrap();
    assert!(owner.observations.is_empty(), "completed payload must compact without another turn in this scope");
    assert!(owner.latest.is_some());
    assert_eq!(serde_json::to_vec(inner.engram_source_sightings.iter().find(|owner|
        owner.scope == pending_owner.scope).unwrap()).unwrap(), pending_bytes,
        "unresolved original retry bytes cannot be evicted for a size bound");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count(), 1,
        "normal finalization uses the ordinary close, not a retirement close");
}

#[test]
fn source_observation_public_resume_retries_final_ack_after_early_settlement() {
    for restore in [false, true] {
        resume_after_final_ack_loss(false, restore);
    }
}

#[test]
fn source_observation_metadata_commit_cannot_ack_quarantined_session_cleanup() {
    for restore in [false, true] {
        resume_after_final_ack_loss(true, restore);
    }
}

fn resume_after_final_ack_loss(quarantine_cleanup: bool, restore: bool) {
    let mut claimed = held_observation("source-observation-final-ack");
    let original = original_intent(&claimed);
    claimed.state.cancel_queued_prompt(&claimed.session_id, &original.prompt_id).unwrap();
    let (scope, target) = {
        let inner = claimed.state.inner.lock().unwrap();
        (inner.engram_source_sightings[0].scope.clone(),
            AppState::engram_binding_target_for_session_shape_locked(&inner, &claimed.session_id, true)
                .unwrap().unwrap())
    };
    let clock = claimed.state.engram_budget_clock();
    claimed.state.close_retired_source_history(&scope, &original.id, &target,
        clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS)).unwrap();
    claimed.state.close_retired_source_history(&scope, &original.id, &target,
        clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS)).unwrap();
    let close_count = claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count();
    assert_eq!(close_count, 1);
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let mut cache = SqlitePersistConnectionCache::new();
    // Carry the production writer's watermark across successful commits.
    // Recollecting from zero would hide an unstamped session cleanup.
    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    let mut watermark = delta.watermark;
    persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
    let mut lost_ack = false;
    let mut missing_cleanup_row = false;
    for attempt in 0..2 {
        let state = claimed.state.clone();
        let session_id = claimed.session_id.clone();
        let task = std::thread::spawn(move || state.resume_local_session_queue(&session_id, None));
        let mut batch = PersistFenceBatch::default();
        while !task.is_finished() {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(PersistRequest::Fence(fence)) => {
                    let mut delta = collect_persist_delta_from_shared_state(&claimed.state.inner, watermark);
                    if matches!(&fence.target, PersistFenceTarget::EngramSourceFinalization { .. })
                        && !delta.changed_sessions.iter().any(|record| record.session.id == claimed.session_id) {
                        missing_cleanup_row = true;
                        fence.finish(Err(PersistFenceError::WriteFailed(
                            "finalization omitted its session after the writer watermark advanced".to_owned())));
                        continue;
                    }
                    let recorded = if quarantine_cleanup {
                        matches!(&fence.target, PersistFenceTarget::EngramSourceFinalization { .. })
                    } else { matches!(&fence.target,
                        PersistFenceTarget::EngramSourceSightingHistory(owner)
                            if owner.observations.iter().any(|intent| intent.id == original.id
                                && matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. }))) };
                    if attempt == 0 && recorded && !lost_ack {
                        if quarantine_cleanup {
                            delta.changed_sessions.retain(|record| record.session.id != claimed.session_id);
                            delta.deferred_session_ids.push(claimed.session_id.clone());
                        }
                        persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
                        watermark = delta.watermark;
                        if quarantine_cleanup {
                            assert!(!fence.target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap(),
                                "committed metadata cannot acknowledge quarantined session cleanup");
                        } else {
                            assert!(fence.target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap(),
                                "the exact receipt did commit before its acknowledgement was lost");
                        }
                        lost_ack = true;
                        fence.finish(Err(PersistFenceError::WriteFailed("final history commit reply was lost".to_owned())));
                    } else {
                        batch.accept(PersistRequest::Fence(fence));
                        persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
                        watermark = delta.watermark;
                    }
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("writer fixture disconnected: {error}"),
            }
        }
        let result = task.join().unwrap();
        assert!(!missing_cleanup_row,
            "every finalization attempt must select its session after prior history commits advanced the writer watermark");
        if attempt == 0 {
            assert!(lost_ack, "exercise an actual committed Recorded+settled history fence");
            assert!(result.is_err());
            {
                let inner = claimed.state.inner.lock().unwrap();
                let index = inner.find_session_index(&claimed.session_id).unwrap();
                assert!(engram_source_observation_holds_admission(&inner, index));
                let intent = inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations)
                    .find(|intent| intent.id == original.id).unwrap();
                assert!(!intent.finalization_complete, "a local cleanup or metadata commit is not completion proof");
            }
            if restore {
                *claimed.state.inner.lock().unwrap() = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
                claimed.state.install_test_engram_budget_clock(EngramBudgetClock::scripted());
                install_control_only_transport(&claimed.state, claimed.transport.clone());
                watermark = 0; // A restored process starts a new persistence worker.
            }
        } else {
            result.unwrap();
            let inner = claimed.state.inner.lock().unwrap();
            let owner = inner.engram_source_sightings.iter().find(|owner| owner.scope == scope).unwrap();
            assert_eq!(owner.latest.as_ref(), Some(&original.sighting),
                "public Resume must finalize the accounted measurement after a lost local ACK");
            assert!(owner.observations.is_empty());
            let record = &inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()];
            assert!(record.engram.source_observation_gate.is_none());
            assert!(record.engram.active_grant_id.is_none());
            assert!(record.engram.uncertain_grant_id.is_none());
            assert!(!record.orchestrator_auto_dispatch_blocked);
        }
        batch.fail(PersistFenceError::Shutdown);
    }
    let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, watermark);
    persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
    let saved = load_state(claimed.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(saved.engram_source_sightings.iter().find(|owner| owner.scope == scope).unwrap().latest.as_ref(),
        Some(&original.sighting));
    assert!(saved.engram_source_sightings.iter().find(|owner| owner.scope == scope).unwrap().observations.is_empty());
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_checkpoint").count(), close_count,
        "a matching settled close must never be repeated");
    assert_eq!(claimed.transport.requests().iter().filter(|request|
        request.request["operation"] == "turn_begin").count(), 1);
    assert!(claimed.runtime_rx.try_recv().is_err(), "retired payload cannot be replayed");
}
