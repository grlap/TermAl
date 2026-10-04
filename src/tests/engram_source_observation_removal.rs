//! Refused removal retains the original session for settlement-only recovery.
use super::*;

#[test]
fn source_observation_delete_waits_for_delivered_local_finalization() {
    for quarantine in [false, true] {
        delivered_delete_with_unacknowledged_local_completion(quarantine);
    }
}

fn delivered_delete_with_unacknowledged_local_completion(quarantine: bool) {
    let mut claimed = opening_fixture("source-observation-delete-delivered");
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    let _ = received_prompt(&claimed);
    let before = original_intent(&claimed);
    assert!(matches!(
        before.phase,
        EngramSourceObservationPhase::Recorded { .. }
    ));
    assert!(
        before.continuation_released && !before.delivery_retired && !before.finalization_complete
    );
    let original_writer = claimed.state.persist_tx.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let mut cache = SqlitePersistConnectionCache::new();
    let prime = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
    persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &prime).unwrap();
    let mut watermark = prime.watermark;
    let state = claimed.state.clone();
    let id = claimed.session_id.clone();
    let task = std::thread::spawn(move || state.kill_session(&id));
    let mut finalization_seen = false;
    let mut batch = PersistFenceBatch::default();
    while !task.is_finished() {
        if let Ok(PersistRequest::Fence(fence)) = rx.recv_timeout(Duration::from_millis(20)) {
            let mut delta = collect_persist_delta_from_shared_state(&claimed.state.inner, watermark);
            if matches!(&fence.target, PersistFenceTarget::EngramSourceFinalization { .. }) {
                finalization_seen = true;
                if quarantine {
                    delta.changed_sessions.retain(|record| record.session.id != claimed.session_id);
                    delta.deferred_session_ids.push(claimed.session_id.clone());
                }
                persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta).unwrap();
                watermark = delta.watermark;
                let coupled = fence.target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap();
                fence.finish(Err(PersistFenceError::WriteFailed("local completion acknowledgement lost".to_owned())));
                assert_eq!(coupled, !quarantine);
            } else {
                batch.accept(PersistRequest::Fence(fence));
                persist_delta_with_fences(&mut cache, claimed.state.persistence_path.as_path(), &delta, &mut batch).unwrap();
                watermark = delta.watermark;
            }
        }
    }
    let error = task.join().unwrap().err().expect("unacknowledged local completion must retain the session");
    assert!(finalization_seen, "Delete must attempt ordinary local completion before deciding refusal");
    claimed.state.persist_tx = original_writer;
    assert!(error.message.contains("Session kept"));
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        0,
        "the barrier must precede ordinary delivered-grant teardown"
    );
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        0,
        "local delivered finalization does not authorize a retirement close"
    );
    assert!(
        claimed.state.inner.lock().unwrap().engram_source_sightings[0]
            .observations
            .is_empty()
    );
    claimed.state.kill_session(&claimed.session_id).unwrap();
}

#[test]
fn source_observation_delete_completes_delivered_local_duty_before_teardown() {
    let claimed = opening_fixture("source-observation-delete-local-completion");
    assert!(matches!(deliver_turn_dispatch(&claimed.state, claimed.dispatch()), TurnDispatchDeliveryOutcome::Delivered));
    let _ = received_prompt(&claimed);
    let before = original_intent(&claimed);
    assert!(matches!(before.phase, EngramSourceObservationPhase::Recorded { .. }));
    assert!(before.continuation_released && !before.delivery_retired && !before.finalization_complete);
    claimed.state.kill_session(&claimed.session_id)
        .expect("ordinary local completion permits teardown without a recovery round trip");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.find_session_index(&claimed.session_id).is_none());
    assert!(!inner.engram_source_sightings.iter().flat_map(|owner| &owner.observations).any(|intent| intent.id == before.id));
    assert_eq!(claimed.transport.requests().iter().filter(|request| request.request["operation"] == "turn_checkpoint").count(), 1);
}

#[test]
fn source_observation_delete_does_not_wait_for_optional_completed_compaction() {
    let claimed = opening_fixture("source-observation-delete-completed");
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    let _ = received_prompt(&claimed);
    let mut completed = original_intent(&claimed);
    finish_claimed_turn(&claimed, &claimed.runtime_token());
    completed.finalization_complete = true;
    // Reintroduce only the already-proved payload, simulating a lost optional
    // compaction write. No recording, settlement or local duty remains.
    claimed.state.inner.lock().unwrap().engram_source_sightings[0]
        .observations
        .push(completed);
    claimed.state.kill_session(&claimed.session_id).unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .find_session_index(&claimed.session_id)
            .is_none()
    );
}

#[test]
fn source_observation_deleted_session_does_not_orphan_same_run_admission() {
    for restore in [false, true] {
        assert_removed_observer_recovers(0, restore);
    }
}

#[test]
fn source_observation_direct_removal_does_not_orphan_same_run_admission() {
    assert_removed_observer_recovers(1, false);
}

#[test]
fn source_observation_retained_filter_does_not_orphan_same_run_admission() {
    assert_removed_observer_recovers(2, false);
}

#[test]
fn source_observation_hidden_session_keeps_its_persisted_recovery_record() {
    assert_removed_observer_recovers(3, true);
}

#[test]
fn source_observation_delete_recovery_allows_same_run_peer_delivery() {
    let claimed = held_observation("source-observation-delete-peer-delivery");
    let original = original_intent(&claimed);
    let project = claimed.record(|record| record.session.project_id.clone().unwrap());
    let peer = create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let runtime = inner.sessions[inner.find_session_index(&claimed.session_id).unwrap()]
            .runtime
            .clone();
        let index = inner.find_session_index(&peer).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = runtime;
        record.engram.routing_token = Some("same-run-peer-routing".to_owned());
        record.engram.work_binding = Some(original.binding.clone());
        record.engram.context_nudge_pending = false;
        assert!(engram_source_observation_holds_admission(&inner, index));
    }
    assert!(claimed.state.kill_session(&claimed.session_id).is_err());
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    let grant = "recovered-same-run-peer-grant";
    claimed.transport.responses.lock().unwrap().extend([
        grant_reply(grant),
        begin_reply(grant),
        checkpoint_reply(grant),
    ]);
    let dispatch = dispatch_live_root_for_removal(&claimed.state, &peer);
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    match claimed
        .runtime_rx
        .try_recv()
        .expect("same-run peer must actually reach its provider")
    {
        CodexRuntimeCommand::Prompt { session_id, .. } => assert_eq!(session_id, peer),
        _ => panic!("expected the newly admitted peer prompt"),
    }
    assert!(claimed.runtime_rx.try_recv().is_err());
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .count(),
        2
    );
}

fn dispatch_live_root_for_removal(state: &AppState, session: &str) -> TurnDispatch {
    match state
        .dispatch_turn(
            session,
            SendMessageRequest {
                text: "Continue from recovered source tracking.".to_owned(),
                attachments: vec![],
                expanded_text: None,
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("a recovered same-run peer must proceed to admission"),
    }
}

#[test]
fn source_observation_delete_refuses_whole_delegated_subtree_before_teardown() {
    let claimed = held_observation("source-observation-delete-child");
    let (parent, _) = super::delegation::attach_observation_delegation(&claimed);
    let error = claimed.state.kill_session(&parent).err().unwrap();
    assert!(error.message.contains(&claimed.session_id));
    for id in [&parent, &claimed.session_id] {
        let inner = claimed.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_visible_session_index(id).unwrap()];
        assert!(record.engram.source_observation_delete_requested);
        assert!(record.session.preview.contains("Session kept"));
    }
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        0
    );
    assert!(
        claimed
            .state
            .create_read_only_delegation(
                &parent,
                serde_json::from_value(json!({
                    "prompt": "A child created during Delete must not escape its affected set.",
                    "cwd": claimed.root, "agent": "Codex"
                }))
                .unwrap()
            )
            .is_err()
    );
    assert!(
        claimed
            .state
            .resume_local_session_queue(&parent, None)
            .is_err(),
        "parent Resume must identify the blocking child, not dispatch a parent prompt"
    );
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    claimed.state.kill_session(&parent).unwrap();
    let inner = claimed.state.inner.lock().unwrap();
    assert!(inner.find_session_index(&parent).is_none());
    assert!(inner.find_session_index(&claimed.session_id).is_none());
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_retires_late_receipt_and_continuation() {
    let claimed = held_observation("source-observation-delete-late-receipt");
    let (gate, owner, mut intent) = claimed
        .state
        .source_observation_snapshot(&claimed.session_id)
        .unwrap();
    let (request, receipt) = claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .observations
        .values()
        .next()
        .unwrap()
        .clone();
    intent.observing_session = receipt["observing_session"].as_str().map(str::to_owned);
    validate_retained_source_observation_receipt(&intent, &request, &receipt).unwrap();
    intent.phase = EngramSourceObservationPhase::Recorded { request, receipt };
    assert!(claimed.state.kill_session(&claimed.session_id).is_err());
    // The actual producer reply was committed before retirement. Its delayed
    // consumer carries the exact old gate and owner into the phase transition.
    assert!(
        claimed
            .state
            .replace_source_observation_phase(&claimed.session_id, &gate, &owner, intent)
            .is_err()
    );
    claimed
        .state
        .release_source_observation_continuation(&claimed.session_id);
    claimed.record(|record| {
        assert!(
            record
                .engram
                .source_observation_gate
                .as_ref()
                .unwrap()
                .retired
        );
        assert!(record.engram.source_observation_continuation.is_none());
        assert!(!engram_source_observation_can_deliver(record));
        assert!(record.session.preview.contains("Session kept"));
    });
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_during_capture_keeps_original_owner_without_delivery() {
    let claimed = opening_fixture("source-observation-delete-capture");
    let state = claimed.state.clone();
    let id = claimed.session_id.clone();
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(
                state.kill_session(&id).is_err(),
                "an admitted capture reserves its original owner"
            );
            assert!(
                state.resume_local_session_queue(&id, None).is_err(),
                "recovery cannot race the unfinished capture"
            );
        }));
    });
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Held { .. }
    ));
    let intent = original_intent(&claimed);
    assert!(intent.delivery_retired);
    claimed.record(|record| {
        assert!(record.engram.source_observation_delete_requested);
        assert!(record.engram.source_observation_continuation.is_none());
    });
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    claimed.state.kill_session(&claimed.session_id).unwrap();
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .count(),
        1
    );
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_recovery_failure_keeps_original_obligation() {
    let claimed = held_observation("source-observation-delete-failed-recovery");
    assert!(claimed.state.kill_session(&claimed.session_id).is_err());
    let before = original_intent(&claimed);
    claimed
        .transport
        .named_roots
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .lose_next_observation_reply = true;
    let error = claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .err()
        .unwrap();
    assert!(error.message.contains("original store and connection"));
    assert_eq!(original_intent(&claimed).phase, before.phase);
    claimed.record(|record| {
        assert!(record.engram.source_observation_delete_requested);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.source_observation_continuation.is_none());
    });
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    let requests = claimed.transport.requests();
    let observations: Vec<_> = requests
        .iter()
        .filter(|request| request.request["operation"] == "execution_observe")
        .collect();
    assert_eq!(observations.len(), 3);
    assert!(
        observations
            .windows(2)
            .all(|pair| pair[0].request == pair[1].request
                && pair[0].connection == pair[1].connection)
    );
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_refused_policy_requires_replacement_duties() {
    let claimed = opening_fixture("source-observation-delete-refused-policy");
    {
        let mut roots = claimed.transport.named_roots.lock().unwrap();
        roots.as_mut().unwrap().refuse_next_observation_policy = true;
        roots.as_mut().unwrap().lose_next_observation_reply = true;
    }
    assert!(matches!(
        deliver_turn_dispatch(&claimed.state, claimed.dispatch()),
        TurnDispatchDeliveryOutcome::Held { .. }
    ));
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let owner = &mut inner.engram_source_sightings[0];
        let refused = owner
            .observations
            .iter()
            .find(|intent| {
                matches!(
                    intent.phase,
                    EngramSourceObservationPhase::RefusedPolicy { .. }
                )
            })
            .unwrap()
            .clone();
        assert!(source_observation_has_removal_duty(owner, &refused));
        owner.observations.retain(|intent| intent.id == refused.id);
        owner.version += 1;
        assert!(
            source_observation_has_removal_duty(owner, &refused),
            "missing replacement is not resolved history"
        );
    }
    assert!(claimed.state.kill_session(&claimed.session_id).is_err());
    assert!(
        claimed
            .state
            .resume_local_session_queue(&claimed.session_id, None)
            .is_err(),
        "missing replacement cannot produce a false recovery-success disposition"
    );
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_persistence_failure_requires_coupled_ack_before_recovery() {
    for quarantine in [false, true] {
        let mut claimed = held_observation(if quarantine {
            "source-observation-delete-quarantined"
        } else {
            "source-observation-delete-lost-ack"
        });
        let original = original_intent(&claimed);
        let (tx, rx) = std::sync::mpsc::channel();
        claimed.state.persist_tx = tx;
        let mut cache = SqlitePersistConnectionCache::new();
        let delta = collect_persist_delta_from_shared_state(&claimed.state.inner, 0);
        let mut watermark = delta.watermark;
        persist_delta_via_cache(&mut cache, claimed.state.persistence_path.as_path(), &delta)
            .unwrap();
        for attempt in 0..2 {
            let state = claimed.state.clone();
            let id = claimed.session_id.clone();
            let task = std::thread::spawn(move || {
                if attempt == 0 {
                    state.kill_session(&id).map(|_| ())
                } else {
                    state.resume_local_session_queue(&id, None).map(|_| ())
                }
            });
            let mut batch = PersistFenceBatch::default();
            let mut removal_fence_seen = false;
            while !task.is_finished() {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(PersistRequest::Fence(fence)) => {
                        let mut delta = collect_persist_delta_from_shared_state(
                            &claimed.state.inner,
                            watermark,
                        );
                        if matches!(
                            &fence.target,
                            PersistFenceTarget::EngramSourceRemoval { .. }
                        ) {
                            removal_fence_seen = true;
                            assert!(
                                delta
                                    .changed_sessions
                                    .iter()
                                    .any(|record| record.session.id == claimed.session_id),
                                "every marker ACK attempt must select its session above the writer watermark"
                            );
                            if attempt == 0 {
                                if quarantine {
                                    delta
                                        .changed_sessions
                                        .retain(|record| record.session.id != claimed.session_id);
                                    delta.deferred_session_ids.push(claimed.session_id.clone());
                                }
                                persist_delta_via_cache(
                                    &mut cache,
                                    claimed.state.persistence_path.as_path(),
                                    &delta,
                                )
                                .unwrap();
                                watermark = delta.watermark;
                                assert_eq!(
                                    fence
                                        .target
                                        .is_already_durable(cache.connection.as_ref().unwrap())
                                        .unwrap(),
                                    !quarantine,
                                    "metadata alone cannot acknowledge the retained session's removal projection"
                                );
                                fence.finish(Err(PersistFenceError::WriteFailed(
                                    "removal marker acknowledgement lost".to_owned(),
                                )));
                                continue;
                            }
                        }
                        batch.accept(PersistRequest::Fence(fence));
                        persist_delta_with_fences(
                            &mut cache,
                            claimed.state.persistence_path.as_path(),
                            &delta,
                            &mut batch,
                        )
                        .unwrap();
                        watermark = delta.watermark;
                    }
                    Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(error) => panic!("removal writer disconnected: {error}"),
                }
            }
            let result = task.join().unwrap();
            assert!(removal_fence_seen);
            if attempt == 0 {
                assert!(result.is_err());
                assert_eq!(
                    claimed
                        .transport
                        .requests()
                        .iter()
                        .filter(|request| request.request["operation"] == "execution_observe")
                        .count(),
                    1,
                    "uncertain removal ACK must not start recovery transport"
                );
                assert!(
                    claimed
                        .state
                        .inner
                        .lock()
                        .unwrap()
                        .find_visible_session_index(&claimed.session_id)
                        .is_some()
                );
                if !quarantine {
                    *claimed.state.inner.lock().unwrap() =
                        load_state(claimed.state.persistence_path.as_path())
                            .unwrap()
                            .unwrap();
                    claimed.state.select_test_scripted_engram_budget_clock();
                    install_control_only_transport(&claimed.state, claimed.transport.clone());
                    watermark = 0;
                }
            } else {
                result.unwrap();
                let inner = claimed.state.inner.lock().unwrap();
                let record = &inner.sessions[inner
                    .find_visible_session_index(&claimed.session_id)
                    .unwrap()];
                assert!(
                    record.orchestrator_auto_dispatch_blocked
                        && record.engram.source_observation_delete_requested
                );
                assert!(inner.engram_source_sightings[0].observations.is_empty());
                assert_eq!(
                    inner.engram_source_sightings[0].latest.as_ref(),
                    Some(&original.sighting)
                );
            }
            batch.fail(PersistFenceError::Shutdown);
        }
        let requests = claimed.transport.requests();
        let observations: Vec<_> = requests
            .iter()
            .filter(|request| request.request["operation"] == "execution_observe")
            .collect();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].request, observations[1].request);
        assert_eq!(observations[0].connection, observations[1].connection);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.request["operation"] == "turn_checkpoint")
                .count(),
            1
        );
        assert!(claimed.runtime_rx.try_recv().is_err());
    }
}

fn assert_removed_observer_recovers(removal: u8, restore: bool) {
    let claimed = held_observation(&format!("source-observation-remove-{removal}"));
    let before = original_intent(&claimed);
    assert!(matches!(
        before.phase,
        EngramSourceObservationPhase::Prepared { .. }
    ));
    let project = claimed.record(|record| record.session.project_id.clone().unwrap());
    let peer = create_test_project_session(&claimed.state, Agent::Codex, &project, &claimed.root);
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let index = inner.find_session_index(&peer).unwrap();
        inner.sessions[index].engram.routing_token = Some("same-run-peer-routing".to_owned());
        inner.sessions[index].engram.work_binding = Some(before.binding.clone());
        inner.sessions[index].engram.context_nudge_pending = false;
        assert!(
            engram_source_observation_holds_admission(&inner, index),
            "the fixture peer must have the exact association held by the pending observation"
        );
    }
    if removal == 0 {
        let error = claimed
            .state
            .kill_session(&claimed.session_id)
            .err()
            .expect("Delete must refuse before teardown");
        assert!(error.message.contains("Session kept"));
        assert!(error.message.contains(&claimed.session_id));
    } else {
        let mut inner = claimed.state.inner.lock().unwrap();
        if removal == 1 {
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            assert!(inner.remove_session_at(index).is_none());
        } else if removal == 2 {
            inner.retain_sessions(|record| record.session.id != claimed.session_id);
        } else {
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            inner.sessions[index].hidden = true;
            inner.stamp_session_at_index(index);
        }
        claimed.state.commit_locked(&mut inner).unwrap();
    }
    {
        let inner = claimed.state.inner.lock().unwrap();
        let retained_index = inner
            .find_visible_session_index(&claimed.session_id)
            .expect("original recovery owner stays accessible");
        assert!(
            inner.sessions[retained_index]
                .engram
                .source_observation_delete_requested
        );
        assert!(
            AppState::wire_session_from_record(
                &claimed.state.server_instance_id,
                &inner.sessions[retained_index]
            )
            .source_tracking_recovery
        );
        assert!(
            AppState::wire_session_summary_from_record(
                &claimed.state.server_instance_id,
                &inner.sessions[retained_index]
            )
            .source_tracking_recovery
        );
        assert!(
            inner.sessions[retained_index]
                .session
                .preview
                .contains("Session kept")
        );
        assert!(
            inner.sessions[retained_index]
                .engram
                .source_observation_continuation
                .is_none()
        );
        let retained = inner
            .engram_source_sightings
            .iter()
            .flat_map(|owner| &owner.observations)
            .find(|intent| intent.id == before.id)
            .expect("removal cannot discard uncertain accounting");
        assert_eq!(retained.phase, before.phase);
        assert_eq!(retained.connection, before.connection);
        assert_eq!(retained.routing_token, before.routing_token);
        assert!(retained.delivery_retired);
        let index = inner.find_session_index(&peer).unwrap();
        assert!(
            engram_source_observation_holds_admission(&inner, index),
            "removal must not release the hold by discarding its obligations"
        );
    }
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        0,
        "refusal precedes teardown of the original grant"
    );
    if restore {
        *claimed.state.inner.lock().unwrap() = load_state(claimed.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        claimed.state.select_test_scripted_engram_budget_clock();
        install_control_only_transport(&claimed.state, claimed.transport.clone());
    }
    claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None)
        .unwrap();
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        let original = &inner.sessions[inner
            .find_visible_session_index(&claimed.session_id)
            .unwrap()];
        assert!(original.orchestrator_auto_dispatch_blocked);
        assert!(
            !original.queued_prompts.is_empty(),
            "recovery cannot dispatch or consume the old queued head"
        );
        assert!(original.session.preview.contains("retry Delete"));
        let index = inner.find_session_index(&peer).unwrap();
        inner.sessions[index].engram.work_binding = Some(before.binding.clone());
        inner.sessions[index].engram.routing_token = Some("same-run-peer-routing".to_owned());
        assert!(
            !engram_source_observation_holds_admission(&inner, index),
            "durable recovery must release the observation's hold on its same-run peer"
        );
    }
    let requests = claimed.transport.requests();
    let observations: Vec<_> = requests
        .iter()
        .filter(|request| request.request["operation"] == "execution_observe")
        .collect();
    assert_eq!(
        observations.len(),
        2,
        "recover the producer's committed reply with the original exact request"
    );
    assert_eq!(observations[0].request, observations[1].request);
    assert_eq!(observations[0].connection, observations[1].connection);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .count(),
        1
    );
    claimed.state.kill_session(&claimed.session_id).unwrap();
    assert!(
        claimed
            .state
            .inner
            .lock()
            .unwrap()
            .find_session_index(&claimed.session_id)
            .is_none()
    );
    assert!(
        claimed.runtime_rx.try_recv().is_err(),
        "the removed provider payload must never replay"
    );
}
