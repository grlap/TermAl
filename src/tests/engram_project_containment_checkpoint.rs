// Owns OFF overlapping a real in-flight closing checkpoint and its receipt.
// Does not introduce a recovery scheduler or access live authority/store state.
use super::*;

#[path = "engram_project_containment_checkpoint_intake.rs"]
mod intake;

#[path = "engram_project_containment_checkpoint_flight.rs"]
mod flight;

#[path = "engram_project_containment_checkpoint_receipts.rs"]
mod receipts;

#[path = "engram_project_containment_checkpoint_review.rs"]
mod review;

#[test]
fn project_containment_checkpoint_recovery_view_never_lends_generic_admission() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    let mirror = inner.sessions[index].engram.checkpoint_operation.clone();
    let ledger = inner.engram_session_obligations.clone();
    let generation = inner.sessions[index].engram.dispatch_generation;
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_some());
    assert!(
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .is_none()
    );
    assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_none());
    assert!(EngramCheckpointRecoveryView::for_original_status(&inner, &session, None).is_none());

    inner.sessions[index].engram.checkpoint_operation = None;
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
    inner.sessions[index].engram.checkpoint_operation = mirror;
    inner
        .engram_session_obligations
        .checkpoint_operations
        .clear();
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
    inner.engram_session_obligations = ledger.clone();
    inner.engram_session_obligations.checkpoint_operations[0]
        .protection
        .reason_code = "different_valid_reason".into();
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
    inner.engram_session_obligations = ledger;
    inner.sessions[index].engram.dispatch_generation += 1;
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
    inner.sessions[index].engram.dispatch_generation = generation;
    inner.sessions[index].session.parent_delegation_id = Some("missing-child-provenance".into());
    assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_certified_view_requires_current_owner_settings_and_budget() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "control_store_busy".into(),
            message: "fixture busy".into(),
        })),
    )]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        state.checkpoint_engram_turn_off_lock(
            &session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        ),
        EngramCheckpointOutcome::Failed(_)
    ));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_some());
        assert!(
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .is_none()
        );
        inner.sessions[index].runtime_stop_in_progress = true;
        assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_none());
        inner.sessions[index].runtime_stop_in_progress = false;
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
        assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_none());
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = true;
        let generation = inner.sessions[index].active_turn_generation;
        inner.sessions[index].active_turn_generation += 1;
        assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_none());
        inner.sessions[index].active_turn_generation = generation;
        let ledger = inner.engram_session_obligations.clone();
        let mut competing = ledger.checkpoint_operations[0].clone();
        competing.original.id = Uuid::new_v4().to_string();
        if let EngramControlRequest::TurnCheckpoint {
            grant_id,
            idempotency_key,
            ..
        } = &mut competing.original.request
        {
            grant_id.push_str("-independent");
            idempotency_key.push_str("-independent");
            competing.protection.grant_id = grant_id.clone();
        }
        competing.protection.id = competing.original.id.clone();
        competing.protection.frozen_request = serde_json::to_value(&competing.original).unwrap();
        competing.attempts[0].id = competing.original.id.clone();
        inner
            .engram_session_obligations
            .checkpoint_operations
            .push(competing);
        assert!(inner.engram_session_obligations.validate().is_ok());
        assert!(certified_checkpoint_replay_target_locked(&inner, &session, None).is_none());
        inner.engram_session_obligations = ledger;
        inner.test_engram_dispatch_budget = Some(Duration::ZERO);
    }
    let target = {
        let inner = state.inner.lock().unwrap();
        certified_checkpoint_replay_target_locked(&inner, &session, None).unwrap()
    };
    assert!(
        state
            .retry_certified_engram_checkpoint(&target, None, Some(target.budget_clock.now()))
            .is_err()
    );
    assert_eq!(
        transport.requests().len(),
        1,
        "an exhausted shared deadline sends no retry"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_settled_history_never_supplies_a_new_grant_owner() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([
        checkpoint_reply("retained-grant"),
        checkpoint_reply("successor-grant"),
    ]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        state.checkpoint_engram_turn_off_lock(
            &session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        ),
        EngramCheckpointOutcome::Succeeded
    ));
    let historical_id = {
        let mut inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_none());
        let index = inner.find_session_index(&session).unwrap();
        let historical_id = inner.sessions[index]
            .engram
            .checkpoint_operation
            .as_ref()
            .unwrap()
            .original
            .id
            .clone();
        inner.sessions[index].engram.active_grant_id = Some("successor-grant".into());
        inner.sessions[index].active_turn_generation += 1;
        state.commit_locked(&mut inner).unwrap();
        historical_id
    };
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    let current = inner
        .engram_session_obligations
        .entries
        .iter()
        .find(|entry| entry.grant_id == "successor-grant")
        .unwrap();
    assert_ne!(current.id, historical_id);
    assert_eq!(current.phase, EngramSessionObligationPhase::Confirmed);
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request["grant_id"], "retained-grant");
    assert_eq!(requests[1].request["grant_id"], "successor-grant");
    assert!(receiver.try_recv().is_err());
}

fn contains_exact_json(value: &Value, expected: &Value) -> bool {
    value == expected
        || match value {
            Value::Object(fields) => fields.values().any(|v| contains_exact_json(v, expected)),
            Value::Array(values) => values.iter().any(|v| contains_exact_json(v, expected)),
            _ => false,
        }
}

fn checkpoint_overlapped_by_off(
    lost_reply: bool,
) -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<GatedEngramControlTransport>,
    RecordedEngramControlRequest,
    Value,
) {
    let (state, project, session, _root, receiver) = fixture();
    let reply = if lost_reply {
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "fixture lost the original closing checkpoint reply",
        )))
    } else {
        checkpoint_reply("retained-grant")
    };
    let (step, gate) = gated_engram_step("turn_checkpoint", reply);
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        closing_state.checkpoint_engram_turn_off_lock(
            &closing_session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        )
    });
    let request = gate.wait();
    assert_eq!(request.connection.session_id, session);
    assert_eq!(request.request["grant_id"], "retained-grant");
    assert_eq!(request.request["routing_token"], "retained-token");
    let off = state.update_project_engram_settings(&project, settings(&state, &project, false));
    let retained = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(&inner.engram_session_obligations).unwrap()
    };
    gate.release();
    let outcome = closing.join().unwrap();
    off.expect("an already-running checkpoint cannot veto OFF");
    assert_eq!(
        matches!(outcome, EngramCheckpointOutcome::Succeeded),
        !lost_reply
    );
    assert_eq!(
        transport.requests().len(),
        1,
        "OFF never starts another checkpoint"
    );
    assert!(receiver.try_recv().is_err());
    (
        state, project, session, receiver, transport, request, retained,
    )
}

#[test]
fn project_containment_inflight_checkpoint_retains_exact_original_operation() {
    let (_state, _project, _session, _receiver, _transport, request, retained) =
        checkpoint_overlapped_by_off(false);
    assert!(
        contains_exact_json(&retained, &request.request),
        "OFF must retain the complete original report, next intent and mutation key"
    );
}

#[test]
fn project_containment_inflight_checkpoint_matching_completion_settles_its_obligation() {
    let (state, _project, session, receiver, transport, _request, _retained) =
        checkpoint_overlapped_by_off(false);
    let inner = state.inner.lock().unwrap();
    assert!(
        engram_session_obligation_locked(&inner, &session).is_none(),
        "the original matching completion must settle the checkpoint-only obligation"
    );
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .active_grant_id
            .is_none()
    );
    drop(inner);
    assert!(state.snapshot().engram_control_recovery.is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_inflight_checkpoint_lost_reply_restart_retains_exact_key() {
    let (state, _project, session, receiver, transport, request, _retained) =
        checkpoint_overlapped_by_off(true);
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let entry = engram_session_obligation_locked(&loaded, &session)
        .expect("an unknown original checkpoint remains excluded after restart");
    assert!(
        contains_exact_json(&entry.frozen_request, &request.request),
        "restart recovery must keep the same original request rather than invent a mutation key"
    );
    assert!(loaded.engram_confirmed_containment_ids.is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_off_before_first_send_adopts_the_published_operation() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    state.install_control_test_transport(transport.clone());
    let at_cut = Arc::new(Mutex::new(None));
    let cut = at_cut.clone();
    let off_state = state.clone();
    let off_session = session.clone();
    TEST_ENGRAM_BEFORE_TURN_CHECKPOINT_REQUEST.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let exact = {
                let inner = off_state.inner.lock().unwrap();
                let operation = &inner.engram_session_obligations.checkpoint_operations[0];
                assert!(matches!(
                    operation.progress,
                    EngramExistingCheckpointProgress::Retained
                ));
                serde_json::to_value(&operation.original.request).unwrap()
            };
            off_state
                .update_project_engram_settings(&project, settings(&off_state, &project, false))
                .unwrap();
            let inner = off_state.inner.lock().unwrap();
            assert!(contains_exact_json(
                &engram_session_obligation_locked(&inner, &off_session)
                    .unwrap()
                    .frozen_request,
                &exact
            ));
            *cut.lock().unwrap() = Some(exact);
        }));
    });
    let outcome = state.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(Some(&requests[0].request), at_cut.lock().unwrap().as_ref());
    assert!(engram_session_obligation_locked(&state.inner.lock().unwrap(), &session).is_none());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_unconfirmed_legacy_authority_is_retained_without_release() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .authority_store_key = None;
        state.commit_locked(&mut inner).unwrap();
    }
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        closing_state.checkpoint_engram_turn_off_lock(
            &closing_session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        )
    });
    let request = gate.wait();
    let off = state.update_project_engram_settings(&project, settings(&state, &project, false));
    gate.release();
    closing.join().unwrap();
    off.expect("legacy authority uncertainty must be retained rather than veto OFF");
    {
        let inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert!(!entry.authority_confirmed);
        assert_eq!(entry.phase, EngramSessionObligationPhase::Intended);
        assert!(contains_exact_json(&entry.frozen_request, &request.request));
        assert_eq!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .active_grant_id
                .as_deref(),
            Some("retained-grant")
        );
        assert!(inner.engram_confirmed_containment_ids.is_empty());
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    assert!(loaded.engram_confirmed_containment_ids.is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_duplicate_completion_has_no_new_effects() {
    let (state, _project, session, _receiver, transport, _request, _retained) =
        checkpoint_overlapped_by_off(false);
    let (operation, response, card, before) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
        let EngramExistingCheckpointProgress::Accepted { response } = &operation.progress else {
            panic!("matching receipt")
        };
        let card = record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. }
                    if card.stage == EngramControlStage::Checkpoint =>
                {
                    Some(card.clone())
                }
                _ => None,
            })
            .unwrap();
        let before = json!({"revision": inner.revision, "row": PersistedSessionRecord::from_record(record), "ledger": inner.engram_session_obligations});
        (operation.clone(), response.clone(), card, before)
    };
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_pending_checkpoint_receipts.reserve(&operation).unwrap();
        inner.engram_pending_checkpoint_receipts.receive(&operation,
            bounded_checkpoint_json(&response, ENGRAM_CHECKPOINT_REPLY_MAX_BYTES).unwrap(), true).unwrap();
    }
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    assert!(!state.finish_engram_checkpoint_record_with_operation(
        &session,
        "retained-grant",
        EngramControlCardDecision::Grant,
        card.clone(),
        None,
        false,
        Some(&operation),
        Some(response.clone()),
        Some(state.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT),
        Some(EngramCheckpointAttemptResult::Accepted),
    ));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.engram_pending_checkpoint_receipts.entries[&checkpoint_receipt_key(&operation)].accepted,
            "an unchanged duplicate still requires writer proof before custody release");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(json!({"revision": inner.revision, "row": PersistedSessionRecord::from_record(record), "ledger": inner.engram_session_obligations}), before);
    }
    assert!(state.finish_engram_checkpoint_record_with_operation(
        &session, "retained-grant", EngramControlCardDecision::Grant, card, None, false,
        Some(&operation), Some(response),
        Some(state.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT),
        Some(EngramCheckpointAttemptResult::Accepted),
    ));
    let inner = state.inner.lock().unwrap();
    assert!(!inner.engram_pending_checkpoint_receipts.entries.contains_key(&checkpoint_receipt_key(&operation)));
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        json!({"revision": inner.revision, "row": PersistedSessionRecord::from_record(record), "ledger": inner.engram_session_obligations}),
        before
    );
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_duplicate_receipt_persists_pending_close_without_observation_changes() {
    for fail_commit in [false, true] {
        let (state, project, session, receiver, transport, _request, _retained) =
            checkpoint_overlapped_by_off(false);
        let (operation, response, card, before_revision) = {
            let mut inner = state.inner.lock().unwrap();
            let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
            let EngramExistingCheckpointProgress::Accepted { response } = &operation.progress else {
                panic!("matching receipt")
            };
            let EngramControlRequest::TurnCheckpoint { report, .. } = &operation.original.request else {
                unreachable!()
            };
            assert!(report.observations.is_empty());
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let card = record.session.messages.iter().rev().find_map(|message| match message {
                Message::EngramControl { card, .. } if card.stage == EngramControlStage::Checkpoint => Some(card.clone()),
                _ => None,
            }).unwrap();
            let sighting = EngramSourceSighting {
                basis: EngramExecutionSourceBasis {
                    workspace_id: "duplicate-close-workspace".into(), source_revision: "content-v1:closed".into(),
                    source_root_generation: None, source_root_state: None,
                },
                observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            };
            inner.engram_source_sightings.push(EngramSourceSightingOwner {
                scope: EngramSourceSightingScope {
                    store: operation.original.settings.authority_store_key.clone().unwrap(), project_id: project,
                    root_execution_id: "duplicate-close-root".into(), work_id: "duplicate-close-work".into(),
                    run_id: "duplicate-close-run".into(), claim_id: "duplicate-close-claim".into(),
                    workspace_id: sighting.basis.workspace_id.clone(), source_root_generation: None, source_root_state: None,
                },
                version: 1, latest: None,
                pending_close: Some(EngramPendingClose { sighting, reported_by: Some("retained-grant".into()) }),
                observations: Vec::new(),
            });
            state.commit_locked(&mut inner).unwrap();
            (operation.clone(), response.clone(), card, inner.revision)
        };
        if fail_commit {
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            connection.execute_batch("CREATE TRIGGER fail_duplicate_close BEFORE UPDATE ON state_metadata_units
                WHEN NEW.field_name='engramSourceSightings'
                BEGIN SELECT RAISE(ABORT,'fixture pending-close failure'); END;").unwrap();
        }
        assert_eq!(state.finish_engram_checkpoint_record_with_operation(
            &session, "retained-grant", EngramControlCardDecision::Grant, card, None, false,
            Some(&operation), Some(response),
            Some(state.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT),
            Some(EngramCheckpointAttemptResult::Accepted),
        ), !fail_commit);
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        let owner = loaded.engram_source_sightings.last().unwrap();
        assert_eq!(owner.pending_close.as_ref().unwrap().reported_by.as_deref(),
            if fail_commit { Some("retained-grant") } else { None });
        assert_eq!(owner.version, if fail_commit { 1 } else { 2 });
        if !fail_commit {
            assert!(state.inner.lock().unwrap().revision > before_revision,
                "pending-close accounting must persist even with no changed observations");
        } else {
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            connection.execute_batch("DROP TRIGGER fail_duplicate_close").unwrap();
        }
        assert_eq!(transport.requests().len(), 1);
        assert!(receiver.try_recv().is_err());
    }
}

fn checkpoint_with_unacknowledged_send() -> (
    AppState,
    String,
    String,
    Arc<ScriptedEngramControlTransport>,
) {
    let (state, project, session, _root, _receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let outcome = state.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(matches!(outcome, EngramCheckpointOutcome::Failed(_)));
    assert!(
        transport.requests().is_empty(),
        "an unknown pre-send ACK cannot authorize transport"
    );
    let image = {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_session_obligations.checkpoint_operations.len(),
            1
        );
        let operation = &inner.engram_session_obligations.checkpoint_operations[0];
        assert!(matches!(
            operation.progress,
            EngramExistingCheckpointProgress::Retained
        ));
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap()
    };
    // Local confirmation stores the identical retained intent; it sends nothing.
    state.confirm_engram_containment(image).unwrap();
    (state, project, session, transport)
}

#[test]
fn project_containment_checkpoint_late_receipt_cannot_clear_a_replacement_owner() {
    for replacement in ["generation", "runtime", "grant"] {
        let (state, project, session, _root, receiver) = fixture();
        let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
        let transport = GatedEngramControlTransport::new([step]);
        state.install_control_test_transport(transport.clone());
        let closing_state = state.clone();
        let closing_session = session.clone();
        let closing = std::thread::spawn(move || {
            closing_state.checkpoint_engram_turn_off_lock(
                &closing_session,
                EngramCheckpointPurpose::Settlement,
                None,
                None,
                EngramNextIntent::Wait,
                None,
                None,
            )
        });
        gate.wait();
        let off = state.update_project_engram_settings(&project, settings(&state, &project, false));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            match replacement {
                "generation" => record.engram.dispatch_generation += 1,
                "runtime" => {
                    let (runtime, _input, process) =
                        test_shared_codex_runtime("checkpoint-successor");
                    record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                        runtime_id: runtime.runtime_id.clone(),
                        input_tx: runtime.input_tx.clone(),
                        process,
                        shared_session: None,
                    });
                }
                "grant" => record.engram.active_grant_id = Some("replacement-grant".into()),
                _ => unreachable!(),
            }
            state.commit_locked(&mut inner).unwrap();
        }
        gate.release();
        let outcome = closing.join().unwrap();
        off.unwrap();
        assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some(if replacement == "grant" {
                "replacement-grant"
            } else {
                "retained-grant"
            })
        );
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        assert!(
            matches!(
                inner.engram_session_obligations.checkpoint_operations[0].progress,
                EngramExistingCheckpointProgress::Accepted { .. }
            ),
            "the original receipt remains evidence"
        );
        assert_eq!(transport.requests().len(), 1);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_checkpoint_receipt_does_not_settle_independent_runtime_duty() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }
    let stop = install_test_stop_fence_gate(&state, &session);
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        closing_state.checkpoint_engram_turn_off_lock(
            &closing_session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        )
    });
    gate.wait();
    let off = state.update_project_engram_settings(&project, settings(&state, &project, false));
    if let Err(error) = off {
        gate.release();
        closing.join().unwrap();
        panic!("OFF must retain the checkpoint before requesting Stop: {error:?}");
    }
    stop.wait_until_claimed();
    gate.release();
    let outcome = closing.join().unwrap();
    assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
    {
        let inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert_eq!(entry.phase, EngramSessionObligationPhase::Intended);
        assert!(contained_checkpoint_progress_matches(entry));
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .active_grant_id
                .is_none()
        );
    }
    stop.release();
    let guard = phase_sync::PollGuard::new();
    loop {
        let inner = state.inner.lock().unwrap();
        if !inner.sessions[inner.find_session_index(&session).unwrap()].runtime_stop_in_progress {
            break;
        }
        drop(inner);
        guard.wait("mixed checkpoint/Stop fixture completes");
    }
    assert!(
        state
            .snapshot()
            .engram_control_recovery
            .iter()
            .any(|entry| entry.session_id == session && entry.excluded)
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_pre_send_unknown_ack_keeps_exact_metadata_through_reload() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let exact = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(&inner.engram_session_obligations.checkpoint_operations[0]).unwrap()
    };
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0]).unwrap(),
        exact
    );
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_metadata_survives_missing_or_invalid_session_row() {
    for remove in [false, true] {
        let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
        state.shutdown_persist_blocking();
        let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
        if remove {
            connection
                .execute("DELETE FROM sessions WHERE id = ?1", [&session])
                .unwrap();
        } else {
            connection.execute("UPDATE sessions SET value_json = json_set(value_json, '$.session.agent', 'unsupported_fixture_agent') WHERE id = ?1", [&session]).unwrap();
        }
        drop(connection);
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(loaded.find_session_index(&session).is_none());
        let protection = engram_session_obligation_locked(&loaded, &session).unwrap();
        assert_eq!(protection.local_session_id, session);
        assert_eq!(engram_control_recovery_summaries(&loaded).len(), 1);
        assert!(protection.frozen_request.get("request").is_some());
        assert!(transport.requests().is_empty());
    }
}

#[test]
fn project_containment_checkpoint_malformed_or_missing_metadata_is_not_defaulted() {
    for defect in ["missing", "future_progress", "mirror_mismatch"] {
        let (state, _project, session, _transport) = checkpoint_with_unacknowledged_send();
        state.shutdown_persist_blocking();
        let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
        let encoded = read_persisted_metadata_json(&connection).unwrap().unwrap();
        let mut value: Value = serde_json::from_str(&encoded).unwrap();
        match defect {
            "missing" => {
                value["engramSessionObligations"]
                    .as_object_mut()
                    .unwrap()
                    .remove("checkpointOperations");
            }
            "future_progress" => {
                value["engramSessionObligations"]["checkpointOperations"][0]["progress"]["state"] =
                    json!("future_state")
            }
            "mirror_mismatch" => {
                connection.execute("UPDATE sessions SET value_json = json_set(value_json, '$.engramCheckpointOperation.original.id', 'different-operation') WHERE id = ?1", [&session]).unwrap();
            }
            _ => unreachable!(),
        }
        replace_metadata_units_for_test(&connection, value);
        let before = read_metadata_units_unchecked(&connection).unwrap();
        drop(connection);
        assert!(
            load_state(state.persistence_path.as_path()).is_err(),
            "{defect}"
        );
        let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
        let after = read_metadata_units_unchecked(&connection).unwrap();
        assert_eq!(
            after, before,
            "failed load must not rewrite the safety record"
        );
    }
}
