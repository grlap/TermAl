// Owns bounded reply custody and prospective checkpoint publication boundaries.
// Fixtures use disposable stores; no live authority or background recovery.
use super::*;

fn raw_reply(grant: &str) -> Value {
    let ScriptedEngramControlResponse::Reply(Ok(value)) = checkpoint_reply(grant) else {
        panic!("reply")
    };
    value
}

fn saved_operation(state: &AppState) -> EngramCheckpointOperation {
    state
        .inner
        .lock()
        .unwrap()
        .engram_session_obligations
        .checkpoint_operations[0]
        .clone()
}

#[test]
fn project_containment_checkpoint_earlier_receipt_requires_exact_original_and_attempt_prefix() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let mut inner = state.inner.lock().unwrap();
    let mut expected = inner.engram_session_obligations.checkpoint_operations[0].clone();
    let mut attempt = expected.attempts[0].clone();
    attempt.predecessor = Some(attempt.id.clone());
    attempt.id = Uuid::new_v4().to_string();
    expected.attempts.push(attempt.clone());
    let mut canonical = expected.clone();
    attempt.predecessor = Some(attempt.id.clone());
    attempt.id = Uuid::new_v4().to_string();
    canonical.attempts.push(attempt);
    canonical.validate().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].engram.checkpoint_operation = Some(canonical.clone());
    inner.engram_session_obligations.checkpoint_operations[0] = canonical.clone();
    let before = serde_json::to_value(&inner.engram_session_obligations).unwrap();
    let response = raw_reply("retained-grant");

    let (ledger, owns, accounted) = prospective_checkpoint_result(
        &inner,
        &session,
        &expected,
        Some(&response),
        Some(&EngramCheckpointAttemptResult::Accepted),
    )
    .unwrap();
    assert!(
        !owns,
        "an earlier reply cannot acquire the successor's authority"
    );
    assert!(accounted.attempts[0].result.is_none());
    assert!(matches!(
        accounted.attempts[1].result,
        Some(EngramCheckpointAttemptResult::Accepted)
    ));
    assert_eq!(
        serde_json::to_value(&accounted.attempts[2]).unwrap(),
        serde_json::to_value(&canonical.attempts[2]).unwrap()
    );
    assert_ne!(
        accounted.protection.phase,
        EngramSessionObligationPhase::Confirmed
    );
    ledger.validate().unwrap();

    for mismatch in [
        "request",
        "original_owner",
        "attempt_owner",
        "prefix",
        "attempt_id",
    ] {
        let mut stale = expected.clone();
        match mismatch {
            "request" => {
                let EngramControlRequest::TurnCheckpoint {
                    idempotency_key, ..
                } = &mut stale.original.request
                else {
                    unreachable!()
                };
                idempotency_key.push_str("-different");
            }
            "original_owner" => stale.original.active_turn_generation += 1,
            "attempt_owner" => stale.attempts[1].active_turn_generation += 1,
            "prefix" => {
                stale.attempts[0].result =
                    Some(EngramCheckpointAttemptResult::Unknown { code: None });
            }
            "attempt_id" => stale.attempts[1].id = Uuid::new_v4().to_string(),
            _ => unreachable!(),
        }
        assert!(
            prospective_checkpoint_result(
                &inner,
                &session,
                &stale,
                Some(&response),
                Some(&EngramCheckpointAttemptResult::Accepted),
            )
            .is_err(),
            "changed {mismatch} must refuse accounting"
        );
    }
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        before
    );
    assert_eq!(
        serde_json::to_value(
            inner.sessions[index]
                .engram
                .checkpoint_operation
                .as_ref()
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&canonical).unwrap()
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_earlier_receipt_cannot_replace_a_recorded_attempt_result() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let mut inner = state.inner.lock().unwrap();
    let expected = inner.engram_session_obligations.checkpoint_operations[0].clone();
    let mut canonical = expected.clone();
    let mut successor = canonical.attempts[0].clone();
    successor.predecessor = Some(successor.id.clone());
    successor.id = Uuid::new_v4().to_string();
    canonical.attempts[0].result = Some(EngramCheckpointAttemptResult::Unknown { code: None });
    canonical.attempts.push(successor);
    canonical.validate().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].engram.checkpoint_operation = Some(canonical.clone());
    inner.engram_session_obligations.checkpoint_operations[0] = canonical;
    let before = serde_json::to_value(&inner.engram_session_obligations).unwrap();
    let error = prospective_checkpoint_result(
        &inner,
        &session,
        &expected,
        Some(&raw_reply("retained-grant")),
        Some(&EngramCheckpointAttemptResult::Accepted),
    )
    .err()
    .expect("conflicting earlier result must be rejected");
    assert!(
        error
            .to_string()
            .contains("conflicting checkpoint attempt result")
    );
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        before
    );
    assert!(transport.requests().is_empty());
}

fn ordinary_capacity_refusal_keeps_unsent_grant_retryable(exhaust_slots: bool) {
    let (state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    state.install_control_test_transport(transport.clone());
    let capacity_before = {
        let mut inner = state.inner.lock().unwrap();
        let pending = &mut inner.engram_pending_checkpoint_receipts;
        if exhaust_slots {
            for index in 0..ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS {
                pending.reserve_identity(
                    (format!("blocking-close-{index}"), format!("blocking-attempt-{index}")),
                    Arc::from([]),
                    if index % 2 == 0 { EngramCheckpointReceiptOrigin::Ordinary }
                    else { EngramCheckpointReceiptOrigin::OffContainment },
                ).unwrap();
            }
        } else {
            // Exhaust the existing independent byte bound, not durable data.
            pending.reserved_bytes = ENGRAM_CHECKPOINT_PENDING_MAX_BYTES;
        }
        (pending.entries.len(), pending.reserved_bytes)
    };
    let outcome = state.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    assert!(
        matches!(outcome, EngramCheckpointOutcome::Failed(ref detail) if detail.contains("capacity"))
    );
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram.checkpoint_in_progress);
        assert!(record.engram.checkpoint_operation.is_none(),
            "local capacity refusal must not publish an operation: {:?}",
            record.engram.checkpoint_operation.as_ref().map(|operation| &operation.progress));
        assert!(inner.engram_session_obligations.checkpoint_operations.is_empty());
        assert!(engram_session_obligation_locked(&inner, &session).is_none());
        assert_eq!(record.engram.active_grant_id.as_deref(), Some("retained-grant"));
        let pending = &inner.engram_pending_checkpoint_receipts;
        assert_eq!((pending.entries.len(), pending.reserved_bytes), capacity_before);
        inner.engram_session_obligations.validate().unwrap();
    }
    assert!(transport.requests().is_empty() && receiver.try_recv().is_err());
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert!(record.engram.checkpoint_operation.is_none());
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    assert_eq!(record.engram.active_grant_id.as_deref(), Some("retained-grant"));
    // Release only this fixture's blocking volatile reservations. The next
    // close must use the same original grant, without a status read or Bind.
    state.inner.lock().unwrap().engram_pending_checkpoint_receipts =
        EngramPendingCheckpointReceipts::default();
    assert!(matches!(state.checkpoint_engram_turn_off_lock(
        &session, EngramCheckpointPurpose::Settlement, None, None,
        EngramNextIntent::Wait, None, None,
    ), EngramCheckpointOutcome::Succeeded));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request["operation"], "turn_checkpoint");
    assert_eq!(requests[0].request["grant_id"], "retained-grant");
    assert_eq!(requests[0].request["routing_token"], "retained-token");
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    let inner = state.inner.lock().unwrap();
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert_eq!(inner.engram_pending_checkpoint_receipts.reserved_bytes, 0);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_pending_capacity_refusal_preserves_unsent_grant() {
    ordinary_capacity_refusal_keeps_unsent_grant_retryable(false);
}

#[test]
fn project_containment_checkpoint_pending_capacity_refusal_shared_slots_preserve_unsent_grant() {
    ordinary_capacity_refusal_keeps_unsent_grant_retryable(true);
}

#[test]
fn project_containment_checkpoint_pending_concurrent_reservations_bound_slots_and_bytes() {
    let (state, _project, _session, _transport) = checkpoint_with_unacknowledged_send();
    let original = saved_operation(&state);
    let store = Arc::new(Mutex::new(EngramPendingCheckpointReceipts::default()));
    let jobs: Vec<_> = (0..24)
        .map(|index| {
            let mut operation = original.clone();
            operation.original.id = format!("reserve-{index}");
            // Exercise the byte cap before the slot cap: full provenance is charged.
            operation.original.runtime_episode = "r".repeat(900_000);
            let store = store.clone();
            std::thread::spawn(move || store.lock().unwrap().reserve(&operation).is_ok())
        })
        .collect();
    let accepted = jobs
        .into_iter()
        .map(|job| job.join().unwrap())
        .filter(|accepted| *accepted)
        .count();
    let store = store.lock().unwrap();
    assert!(accepted > 0 && accepted < ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS);
    assert_eq!(store.entries.len(), accepted);
    assert!(store.reserved_bytes <= ENGRAM_CHECKPOINT_PENDING_MAX_BYTES);
    assert_eq!(
        store
            .entries
            .values()
            .map(|entry| entry.reserved_bytes)
            .sum::<usize>(),
        store.reserved_bytes
    );
}

#[test]
fn project_containment_checkpoint_pending_preserves_extra_fields_expansion_and_exact_owner() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let operation = saved_operation(&state);
    let mut response = raw_reply("retained-grant");
    // The actual wire frame could use literal control escapes. Retained JSON
    // must preserve their Value, not truncate unknown receipt fields to fit.
    response["future"] = json!("\u{1}".repeat(50_000));
    let encoded = bounded_checkpoint_json(&response, ENGRAM_CHECKPOINT_REPLY_MAX_BYTES).unwrap();
    assert!(encoded.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES);
    let reservation = state.reserve_checkpoint_receipt(&operation).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let pending = &mut inner.engram_pending_checkpoint_receipts;
        pending.receive(&operation, encoded.clone(), true).unwrap();
        pending.receive(&operation, encoded.clone(), true).unwrap();
        let mut conflict = response.clone();
        conflict["future"] = json!("different");
        assert!(
            pending
                .receive(
                    &operation,
                    bounded_checkpoint_json(&conflict, ENGRAM_CHECKPOINT_REPLY_MAX_BYTES).unwrap(),
                    true
                )
                .is_err()
        );
        let mut late = operation.clone();
        late.attempts[0].active_turn_generation += 1;
        assert!(pending.receive(&late, encoded.clone(), true).is_err());
        assert!(pending.reserve(&operation).is_err());
        assert_eq!(
            pending.entries[&checkpoint_receipt_key(&operation)]
                .response
                .as_ref()
                .unwrap()
                .as_ref(),
            encoded.as_ref()
        );
        let persisted = serde_json::to_value(PersistedState::from_inner(&inner));
        // A separately persisted session image also cannot carry the buffer.
        let index = inner.find_session_index(&session).unwrap();
        assert!(!contains_exact_json(
            &serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[index]))
                .unwrap(),
            &response
        ));
        assert!(!persisted.unwrap().to_string().contains("future"));
        inner.sessions.remove(index);
    }
    drop(reservation);
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_pending_checkpoint_receipts.entries.len(),
        1,
        "deleting the row cannot discard independent accepted custody"
    );
    assert!(
        !inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .next()
            .unwrap()
            .accounting
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_pending_unsupported_adapter_result_stays_unknown() {
    let (state, _project, session, _root, receiver) = fixture();
    let mut value = raw_reply("retained-grant");
    value["future"] = json!("x".repeat(ENGRAM_CHECKPOINT_REPLY_MAX_BYTES + 1));
    let transport =
        ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Ok(value))]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        state.checkpoint_engram_turn_off_lock(
            &session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None
        ),
        EngramCheckpointOutcome::Failed(_)
    ));
    let inner = state.inner.lock().unwrap();
    let operation = &inner.engram_session_obligations.checkpoint_operations[0];
    assert!(matches!(
        operation.attempts[0].result,
        Some(EngramCheckpointAttemptResult::Unknown { .. })
    ));
    assert!(!matches!(
        operation.progress,
        EngramExistingCheckpointProgress::Accepted { .. }
    ));
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

fn historical_padding(
    operation: &EngramCheckpointOperation,
    padding: usize,
) -> EngramCheckpointOperation {
    let mut history = operation.clone();
    history.original.id = "independent-history".into();
    let EngramControlRequest::TurnCheckpoint {
        grant_id,
        idempotency_key,
        ..
    } = &mut history.original.request
    else {
        unreachable!()
    };
    *grant_id = "historical-grant".into();
    *idempotency_key = "historical-key".into();
    history.protection.id = history.original.id.clone();
    history.protection.grant_id = grant_id.clone();
    history.protection.frozen_request = serde_json::to_value(&history.original).unwrap();
    history.attempts[0].id = history.original.id.clone();
    history.attempts[0].result = Some(EngramCheckpointAttemptResult::Accepted);
    let mut response = raw_reply("historical-grant");
    response["historicalExtra"] = json!("h".repeat(padding));
    history.progress = EngramExistingCheckpointProgress::Accepted {
        response: response.clone(),
    };
    history.protection.settlement_evidence = response;
    history.protection.phase = EngramSessionObligationPhase::Confirmed;
    history
}

#[test]
fn project_containment_off_checkpoint_reserves_custody_before_sending() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    install_control_only_transport(&state, transport.clone());
    state
        .inner
        .lock()
        .unwrap()
        .engram_pending_checkpoint_receipts
        .reserved_bytes = ENGRAM_CHECKPOINT_PENDING_MAX_BYTES;
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    assert!(
        transport.requests().is_empty(),
        "OFF must reserve receipt bytes before its RPC"
    );
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    inner.engram_session_obligations.validate().unwrap();
    assert!(receiver.try_recv().is_err());
}

fn off_checkpoint_history(
    entry: &EngramSessionObligation,
    padding: usize,
) -> EngramSessionObligation {
    let mut history = entry.clone();
    history.id = "off-checkpoint-history".into();
    history.local_session_id = "historical-off-session".into();
    history.connection.session_id = history.local_session_id.clone();
    history.external_ref = "termal:session:historical-off-session".into();
    history.grant_id = "historical-off-grant".into();
    history.routing_token = "historical-off-token".into();
    let mut request: EngramControlRequest = serde_json::from_value(history.frozen_request).unwrap();
    let EngramControlRequest::TurnCheckpoint {
        routing_token,
        grant_id,
        idempotency_key,
        ..
    } = &mut request
    else {
        unreachable!()
    };
    *routing_token = history.routing_token.clone();
    *grant_id = history.grant_id.clone();
    *idempotency_key = "historical-off-checkpoint-key".into();
    history.frozen_request = serde_json::to_value(request).unwrap();
    let mut response = raw_reply(&history.grant_id);
    response["historyPadding"] = json!("h".repeat(padding));
    history.phase = EngramSessionObligationPhase::Confirmed;
    history.settlement_evidence = json!({"operation":"turn_checkpoint", "response":response});
    history.validate().unwrap();
    history
}

#[test]
fn project_containment_off_checkpoint_capacity_keeps_complete_receipt_for_local_accounting() {
    let (state, project, session, _root, receiver) = fixture();
    let mut response = raw_reply("retained-grant");
    response["offReceiptExtension"] = json!("v".repeat(32 * 1024));
    let exact = response.clone();
    let (step, gate) = gated_engram_step(
        "turn_checkpoint",
        ScriptedEngramControlResponse::Reply(Ok(response)),
    );
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let off_settings = settings(&state, &project, false);
    let closing_state = state.clone();
    let closing_project = project.clone();
    let closing = std::thread::spawn(move || {
        closing_state.update_project_engram_settings(&closing_project, off_settings)
    });
    gate.wait();
    let original;
    {
        let mut inner = state.inner.lock().unwrap();
        original = inner.engram_session_obligations.entries[0].clone();
        inner
            .engram_session_obligations
            .entries
            .push(off_checkpoint_history(&original, 0));
        let size = serde_json::to_vec(&inner.engram_session_obligations)
            .unwrap()
            .len();
        let padding = ENGRAM_SESSION_OBLIGATION_MAX_BYTES - size - 1024;
        inner.engram_session_obligations.entries[1] = off_checkpoint_history(&original, padding);
        inner.engram_session_obligations.validate().unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    gate.release();
    closing
        .join()
        .unwrap()
        .expect("receipt capacity cannot veto the applied OFF setting");
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_session_obligations.validate().unwrap();
        let retained = inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .any(|entry| {
                entry.accepted
                    && entry.response.as_ref().is_some_and(|response| {
                        serde_json::from_slice::<Value>(response).unwrap() == exact
                    })
            });
        assert!(
            retained,
            "the complete accepted OFF receipt must remain in bounded volatile custody"
        );
        let entry = inner
            .engram_session_obligations
            .entries
            .iter()
            .find(|entry| entry.id == original.id)
            .unwrap();
        assert_ne!(entry.phase, EngramSessionObligationPhase::Confirmed);
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(!contains_exact_json(
            &serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            &exact
        ));
        persist_state(state.persistence_path.as_path(), &inner).unwrap();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(loaded.engram_pending_checkpoint_receipts.entries.is_empty());
        assert!(engram_session_obligation_locked(&loaded, &session).is_some());
        assert_eq!(
            loaded.sessions[loaded.find_session_index(&session).unwrap()]
                .engram
                .active_grant_id
                .as_deref(),
            Some("retained-grant")
        );
        assert!(!contains_exact_json(
            &serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
            &exact
        ));
        drop(loaded);
        // Separately owned capacity work completes; receipt accounting itself
        // neither prunes this history nor changes either configured bound.
        inner.engram_session_obligations.entries[1] = off_checkpoint_history(&original, 0);
        inner.engram_session_obligations.validate().unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    state.resume_session_queue(&session).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .active_grant_id
            .is_none()
    );
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert_eq!(
        transport.requests().len(),
        1,
        "local accounting cannot repeat the authority RPC"
    );
    assert!(receiver.try_recv().is_err());
    drop(inner);
    state.resume_session_queue(&session).unwrap();
    assert_eq!(
        transport.requests().len(),
        1,
        "duplicate Resume must not repeat the checkpoint"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_pending_hard_cap_never_installs_invalid_receipt_and_recovers_locally()
 {
    let (state, _project, session, _root, receiver) = fixture();
    let mut response = raw_reply("retained-grant");
    response["futureExtra"] = json!("v".repeat(32 * 1024));
    let exact = response.clone();
    let (step, gate) = gated_engram_step(
        "turn_checkpoint",
        ScriptedEngramControlResponse::Reply(Ok(response)),
    );
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
    let original;
    let history_id;
    {
        let mut inner = state.inner.lock().unwrap();
        original = inner.engram_session_obligations.checkpoint_operations[0].clone();
        let history = historical_padding(&original, 0);
        history_id = history.original.id.clone();
        inner
            .engram_session_obligations
            .checkpoint_operations
            .push(history);
        let size = serde_json::to_vec(&inner.engram_session_obligations)
            .unwrap()
            .len();
        let padding = (ENGRAM_SESSION_OBLIGATION_MAX_BYTES - size - 1024) / 2;
        inner.engram_session_obligations.checkpoint_operations[1] =
            historical_padding(&original, padding);
        inner.engram_session_obligations.validate().unwrap();
        state.commit_locked(&mut inner).unwrap();
    }
    gate.release();
    assert!(matches!(
        closing.join().unwrap(),
        EngramCheckpointOutcome::Failed(_)
    ));
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_session_obligations.validate().unwrap();
        let canonical = &inner.engram_session_obligations.checkpoint_operations[0];
        assert!(matches!(
            canonical.progress,
            EngramExistingCheckpointProgress::PossiblySent
        ));
        assert!(canonical.attempts[0].result.is_none());
        let pending =
            &inner.engram_pending_checkpoint_receipts.entries[&checkpoint_receipt_key(&original)];
        assert_eq!(
            serde_json::from_slice::<Value>(pending.response.as_ref().unwrap()).unwrap(),
            exact
        );
        assert!(pending.accepted && !pending.accounting);
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert!(
            inner
                .engram_session_obligations
                .checkpoint_operations
                .iter()
                .any(|op| op.original.id == history_id),
            "capacity never prunes history"
        );
        // An unrelated save cannot leak the volatile receipt into metadata.
        state.commit_locked(&mut inner).unwrap();
    }
    {
        let inner = state.inner.lock().unwrap();
        persist_state(state.persistence_path.as_path(), &inner).unwrap();
    }
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.engram_pending_checkpoint_receipts.entries.is_empty());
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    assert!(!contains_exact_json(
        &serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
        &exact
    ));
    drop(loaded);
    // Simulate completion of separately owned capacity work, not implicit
    // pruning by receipt accounting: restore the pre-pressure fixture ledger.
    {
        let mut inner = state.inner.lock().unwrap();
        inner.engram_session_obligations.checkpoint_operations.pop();
        inner.engram_session_obligations.validate().unwrap();
    }
    assert!(state.account_pending_checkpoint_receipt(&session).unwrap());
    let inner = state.inner.lock().unwrap();
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert_eq!(
        transport.requests().len(),
        1,
        "same receipt local completion sends no RPC"
    );
    assert!(receiver.try_recv().is_err());
}

fn refuse_receipt_ack_after_send(remaining: usize) {
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if remaining > 1 {
                refuse_receipt_ack_after_send(remaining - 1);
            } else {
                TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
            }
        }));
    });
}

pub(super) fn pending_off_receipt() -> (
    AppState,
    String,
    String,
    Arc<ScriptedEngramControlTransport>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    pending_off_receipt_with_queue(false)
}

fn pending_off_receipt_with_queue(queued: bool) -> (
    AppState,
    String,
    String,
    Arc<ScriptedEngramControlTransport>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, project, session, _root, receiver) = fixture();
    if queued {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        queue_prompt_on_record(
            inner.session_mut_by_index(index).unwrap(),
            PendingPrompt {
                id: "queued-before-off".into(),
                timestamp: stamp_now(),
                text: "ordinary queued input".into(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            Vec::new(),
        );
        state.commit_locked(&mut inner).unwrap();
    }
    let mut response = raw_reply("retained-grant");
    response["offAckExtension"] = json!({"original": [1, 2, 3]});
    let transport =
        ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Ok(response))]);
    install_control_only_transport(&state, transport.clone());
    // The initial OFF intent is ACKed, then receipt publication loses its
    // writer acknowledgement. This is not proof that nothing committed.
    refuse_receipt_ack_after_send(1);
    let result = state.update_project_engram_settings(&project, settings(&state, &project, false));
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(result.is_err());
    {
        let inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
        assert!(
            inner
                .engram_pending_checkpoint_receipts
                .entries
                .values()
                .all(|entry| entry.accepted && !entry.accounting)
        );
    }
    assert_eq!(transport.requests().len(), 1);
    assert!(
        state
            .snapshot()
            .engram_control_recovery
            .iter()
            .filter(|entry| entry.session_id == session)
            .any(|entry| entry
                .next_actions
                .iter()
                .any(|action| action == "resume_local_accounting"))
    );
    (state, project, session, transport, receiver)
}

#[test]
fn project_containment_off_checkpoint_unknown_ack_resume_is_local_and_does_not_drain() {
    let (state, project, session, transport, receiver) = pending_off_receipt_with_queue(true);
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram
                .active_grant_id
                .as_deref(),
            Some("retained-grant")
        );
    }
    assert_eq!(
        state
            .dispatch_turn(&session, prompt())
            .err()
            .expect("the independent OFF duty must refuse Send")
            .status,
        axum::http::StatusCode::CONFLICT,
        "an independent OFF checkpoint duty does not admit another Send"
    );
    assert!(
        !state.account_pending_checkpoint_receipt(&session).unwrap(),
        "automatic ordinary accounting cannot select the OFF producer"
    );
    assert!(
        state
            .dispatch_next_queued_turn(&session, false)
            .unwrap()
            .is_none()
    );
    assert!(receiver.try_recv().is_err());
    state.resume_session_queue(&session).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .queued_prompts
            .len(),
        1
    );
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id,
        "queued-before-off"
    );
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(
        receiver.try_recv().is_err(),
        "local receipt Resume must not fall through to dispatch"
    );
}

#[test]
fn project_containment_off_checkpoint_resume_preserves_receipt_on_owner_or_settings_change() {
    for changed in ["generation", "token", "grant", "settings"] {
        let (state, project, session, transport, receiver) = pending_off_receipt();
        let retained;
        {
            let mut inner = state.inner.lock().unwrap();
            retained = inner
                .engram_pending_checkpoint_receipts
                .entries
                .values()
                .next()
                .unwrap()
                .response
                .clone()
                .unwrap();
            let index = inner.find_session_index(&session).unwrap();
            match changed {
                "generation" => inner.sessions[index].engram.dispatch_generation += 1,
                "token" => {
                    inner.sessions[index].engram.routing_token = Some("successor-token".into())
                }
                "grant" => {
                    inner.sessions[index].engram.active_grant_id = Some("successor-grant".into())
                }
                "settings" => {
                    inner
                        .projects
                        .iter_mut()
                        .find(|p| p.id == project)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .deadline_ms = Some(123)
                }
                _ => unreachable!(),
            }
        }
        assert!(state.resume_session_queue(&session).is_err(), "{changed}");
        let inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_some());
        let pending = inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .next()
            .unwrap();
        assert_eq!(
            pending.response.as_ref().unwrap().as_ref(),
            retained.as_ref()
        );
        assert!(!pending.accounting);
        if changed == "grant" {
            assert_eq!(
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .engram
                    .active_grant_id
                    .as_deref(),
                Some("successor-grant")
            );
        }
        assert_eq!(transport.requests().len(), 1);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_off_checkpoint_missing_row_keeps_global_duty_and_custody() {
    let (state, project, session, transport, receiver) = pending_off_receipt();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions.remove(index);
    }
    assert!(state.resume_session_queue(&session).is_err());
    let inner = state.inner.lock().unwrap();
    assert!(engram_project_has_unresolved_obligations(&inner, &project));
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    assert!(
        inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .all(|entry| entry.accepted)
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_off_checkpoint_ack_rechecks_owner_and_excludes_duplicate_accounting() {
    let (state, _project, session, transport, receiver) = pending_off_receipt();
    let callback_state = state.clone();
    let callback_session = session.clone();
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(
                callback_state
                    .resume_session_queue(&callback_session)
                    .is_err(),
                "only the selected local accounting reservation may proceed"
            );
            let mut inner = callback_state.inner.lock().unwrap();
            let index = inner.find_session_index(&callback_session).unwrap();
            inner.sessions[index].engram.dispatch_generation += 1;
        }));
    });
    assert!(state.resume_session_queue(&session).is_err());
    let inner = state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .active_grant_id
            .as_deref(),
        Some("retained-grant")
    );
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    assert!(
        inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .all(|entry| entry.accepted && !entry.accounting)
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_off_checkpoint_and_ordinary_reservations_share_one_slot_pool() {
    let (state, _project, session, transport, receiver) = pending_off_receipt();
    let (ordinary_state, _ordinary_project, _ordinary_session, _ordinary_transport) =
        checkpoint_with_unacknowledged_send();
    let original = saved_operation(&ordinary_state);
    {
        let mut inner = state.inner.lock().unwrap();
        for index in 0..ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS - 1 {
            let mut operation = original.clone();
            operation.original.id = format!("ordinary-slot-{index}");
            inner
                .engram_pending_checkpoint_receipts
                .reserve(&operation)
                .unwrap();
        }
        assert_eq!(
            inner.engram_pending_checkpoint_receipts.entries.len(),
            ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS
        );
        assert!(
            inner.engram_pending_checkpoint_receipts.reserved_bytes
                <= ENGRAM_CHECKPOINT_PENDING_MAX_BYTES
        );
        let mut overflow = original.clone();
        overflow.original.id = "ordinary-slot-overflow".into();
        assert!(
            inner
                .engram_pending_checkpoint_receipts
                .reserve(&overflow)
                .is_err()
        );
    }
    state.resume_session_queue(&session).unwrap();
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.engram_pending_checkpoint_receipts.entries.len(),
        ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS - 1
    );
    assert!(
        inner
            .engram_pending_checkpoint_receipts
            .entries
            .values()
            .all(|entry| entry.origin == EngramCheckpointReceiptOrigin::Ordinary)
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_pending_writer_ack_failure_reuses_receipt_without_rpc() {
    let (state, _project, session, _root, receiver) = fixture();
    let mut response = raw_reply("retained-grant");
    response["producerExtension"] = json!({"exact": [1, 2, 3]});
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Ok(response.clone()),
    )]);
    state.install_control_test_transport(transport.clone());
    refuse_receipt_ack_after_send(2);
    let result = state.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(matches!(result, EngramCheckpointOutcome::Failed(_)));
    {
        let inner = state.inner.lock().unwrap();
        let operation = &inner.engram_session_obligations.checkpoint_operations[0];
        let pending =
            &inner.engram_pending_checkpoint_receipts.entries[&checkpoint_receipt_key(operation)];
        assert!(pending.accepted && !pending.accounting);
        assert_eq!(
            serde_json::from_slice::<Value>(pending.response.as_ref().unwrap()).unwrap(),
            response
        );
        assert!(
            engram_session_obligation_locked(&inner, &session).is_some(),
            "a received result is not a writer ACK"
        );
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        inner.engram_session_obligations.validate().unwrap();
    }
    assert!(state.account_pending_checkpoint_receipt(&session).unwrap());
    let inner = state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}
