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
    state.resume_off_checkpoint_receipt(&session).unwrap();
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
    state.resume_off_checkpoint_receipt(&session).unwrap();
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
    pending_off_receipt_with_queue_kind(queued, false)
}

fn pending_off_receipt_with_queue_kind(queued: bool, receipted: bool) -> (
    AppState, String, String, Arc<ScriptedEngramControlTransport>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, project, session, _root, receiver) = fixture();
    if queued {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let pending = PendingPrompt {
            id: "queued-before-off".into(),
            timestamp: stamp_now(),
            text: "ordinary queued input".into(),
            expanded_text: None,
            source: None,
            attachments: Vec::new(),
            engram_interrupted: false,
            is_engram_retained: false,
        };
        if receipted {
            queue_receipted_orchestrator_prompt_on_record(record, pending, Vec::new());
            // A historical User entry with a real producer-owned Pending
            // receipt, labelled before its first save as in the WAL fixture.
            record.queued_prompts[0].source = QueuedPromptSource::User;
            record.queued_prompts[0].engram_interrupted = true;
            record.queued_prompt_durability_holds.get_mut("queued-before-off").unwrap().content =
                queued_prompt_durability_content(&record.queued_prompts[0]);
            record.set_auto_dispatch_blocked(true);
        } else {
            queue_prompt_on_record(record, pending, Vec::new());
        }
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
fn human_prompt_terminal_preserves_old_duty_and_only_its_own_receipt_reconciles_it() {
    let (state, _project, session, transport, receiver) = pending_off_receipt_with_queue(false);
    let original =
        serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
    let dispatch = match state.dispatch_human_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => panic!("own old receipt must not strand a new human request"),
    };
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(matches!(
        phase_sync::receive(&receiver, "the independent human turn"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    state
        .finish_turn_ok_if_runtime_and_generation_match(&session, &token, generation)
        .unwrap();
    assert_eq!(
        transport.requests().len(),
        1,
        "human completion cannot issue an old grant checkpoint"
    );
    assert_eq!(
        serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap(),
        original,
        "a human turn does not settle the old operation"
    );
    assert!(
        !state
            .resume_off_checkpoint_receipt("another-session")
            .unwrap(),
        "a receipt cannot reconcile another session"
    );
    assert!(
        state.resume_off_checkpoint_receipt(&session).unwrap(),
        "the preserved exact original receipt supports its own local accounting"
    );
    {
        let inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_none());
        assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
    }
    assert_eq!(
        transport.requests().len(),
        1,
        "local evidence reconciliation never replays a provider effect"
    );
    let (unknown, _project, unknown_session, unknown_receiver, unknown_transport) =
        super::super::queue::held_fixture();
    let unresolved =
        serde_json::to_value(&unknown.inner.lock().unwrap().engram_session_obligations).unwrap();
    assert!(
        !unknown
            .resume_off_checkpoint_receipt(&unknown_session)
            .unwrap()
    );
    assert_eq!(
        serde_json::to_value(&unknown.inner.lock().unwrap().engram_session_obligations).unwrap(),
        unresolved
    );
    assert_eq!(unknown_transport.requests().len(), 2);
    assert!(unknown_receiver.try_recv().is_err());
}
#[test]
fn project_containment_off_checkpoint_explicit_accounting_is_local_and_does_not_drain() {
    let (state, project, session, transport, receiver) = pending_off_receipt_with_queue(true);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.operator_paused = true;
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
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
        require_no_engram_session_obligation(&state.inner.lock().unwrap(), &session)
            .unwrap_err().status,
        axum::http::StatusCode::CONFLICT,
        "the raw control obligation remains excluded until its own accounting"
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
    state.resume_off_checkpoint_receipt(&session).unwrap();
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

#[tokio::test]
async fn human_prompt_ui_selected_off_resume_dispatches_without_accounting_old_receipt() {
    use axum::{body::Body, http::{Request, StatusCode}};
    use tower::ServiceExt;
    let (state, _project, session, transport, receiver) = pending_off_receipt_with_queue(true);
    let original = serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
    let selected = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp }
    };
    let response = app_router(state.clone()).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{session}/queue/resume"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&ResumeQueueRequest {
            origin: Some(ExplicitPromptOrigin::Human), human_choice: Some(selected),
        }).unwrap())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(matches!(phase_sync::receive(&receiver, "OFF selected ordinary prompt"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("ordinary queued input")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert!(record.human_prompt_turn.is_none(), "the original ordinary source stays ordinary");
    assert!(record.human_queue_choices.is_empty());
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_off_checkpoint_accounting_preserves_receipt_on_owner_or_settings_change() {
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
        assert!(state.resume_off_checkpoint_receipt(&session).is_err(), "{changed}");
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
fn human_prompt_off_selected_unknown_head_refuses_then_plain_resume_runs_successor() {
    let (state, _project, session, transport, receiver) = pending_off_receipt_with_queue(true);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let head = &mut inner.sessions[index].queued_prompts[0];
        head.promotion_disposition_known = false;
        head.engram_interrupted = true;
    }
    enqueue_human_for_resume(&state, &session, false);
    let (old_head, original) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap())
    };
    let selection = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp }
    };
    let before_selection = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        serde_json::to_value(&record.queued_prompts).unwrap()
    };
    let refused = state.resume_selected_human_prompt(&session, selection)
        .err().expect("an unknown selected head must be refused");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(receiver.try_recv().is_err(), "selecting unknown A never authorizes successor B");
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), before_selection);
        assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
        assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
        assert!(record.human_queue_choices.is_empty());
    }
    // A bodyless OFF Resume is a separate ordinary activation, bound by its
    // own runnable queue owner rather than authority borrowed from unknown A.
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(phase_sync::receive(&receiver, "independent OFF Human successor"),
        CodexRuntimeCommand::Prompt { .. }));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), old_head);
    assert!(record.human_queue_choices.is_empty());
    assert!(record.human_prompt_turn.is_some());
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn human_prompt_reusable_selected_off_resume_runs_its_wal_without_accounting_old_receipt() {
    let (mut state, _project, session, transport, receiver) = pending_off_receipt_with_queue_kind(true, true);
    let writer = super::super::human_choices::ChoiceWriter::attach(&mut state);
    let (content_nonce, content) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let hold = &record.queued_prompt_durability_holds["queued-before-off"];
        assert!(!hold.acknowledged);
        (hold.nonce.clone(), hold.content.clone())
    };
    assert!(state.request_queued_prompt_durability(&session, "queued-before-off"));
    let guard = phase_sync::PollGuard::new();
    loop {
        let acknowledged = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let hold = &record.queued_prompt_durability_holds["queued-before-off"];
            assert_eq!(hold.nonce, content_nonce);
            assert_eq!(hold.content, content);
            hold.acknowledged
        };
        if acknowledged { break; }
        guard.wait("actual content durability acknowledgement");
    }
    assert!(receiver.try_recv().is_err(), "content ACK alone cannot resume a paused queue");
    let (selection, original) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(human_choice_receipt_pending(record, "queued-before-off"));
        (HumanPromptSelection { prompt: projected_pending_prompts(record).remove(0),
            session_mutation_stamp: record.mutation_stamp },
            serde_json::to_value(&inner.engram_session_obligations).unwrap())
    };
    state.resume_selected_human_prompt(&session, selection).unwrap();
    let wal_ack = writer.write_ahead();
    assert!(receiver.try_recv().is_err(), "provider handoff waits for the separate WAL acknowledgement");
    wal_ack.finish(Ok(()));
    assert!(matches!(phase_sync::receive(&receiver, "OFF exact receipted ordinary prompt"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("ordinary queued input")));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    let retained = &record.queued_prompts[0];
    assert_eq!(retained.pending_prompt.id, "queued-before-off");
    assert!(retained.engram_ordinary_handoff);
    let promoted = retained.promoted_message_index.expect("ordinary handoff has a transcript owner");
    assert_eq!(Some(promoted), record.message_positions.get("queued-before-off")
        .map(|position| record.message_start_index + position));
    let mut expected_row = content.clone();
    expected_row["engram_ordinary_handoff"] = json!(true);
    expected_row["promoted_message_index"] = json!(promoted);
    assert_eq!(serde_json::to_value(retained).unwrap(), expected_row,
        "only this prompt's ordinary handoff markers may change");
    assert!(ordinary_queue_position(record).is_none(), "delivered history cannot be selected again");
    assert!(record.human_prompt_turn.is_none());
    assert!(record.human_queue_choices.is_empty());
    let receipt = &record.prompt_deliveries["queued-before-off"];
    assert_eq!(receipt.disposition, PromptDeliveryDisposition::HandingOff,
        "the channel proves handoff, not a provider turn/start acknowledgement");
    assert!(receipt.write_ahead_acknowledged);
    assert_eq!(receipt.turn_generation, Some(record.active_turn_generation));
    assert!(receipt.resume.is_none());
    assert_eq!(transport.requests().len(), 1);
    let token = record.runtime.runtime_token().unwrap();
    let generation = record.active_turn_generation;
    drop(inner);
    state.finish_turn_ok_if_runtime_and_generation_match(&session, &token, generation).unwrap();
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err(), "completion and Resume cannot replay delivered history");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), expected_row);
    let receipt = &record.prompt_deliveries["queued-before-off"];
    assert_eq!(receipt.disposition, PromptDeliveryDisposition::Accepted,
        "the matching runtime turn completion proves provider acceptance");
    assert!(receipt.write_ahead_acknowledged);
    assert_eq!(receipt.turn_generation, Some(generation));
    assert!(receipt.resume.is_none());
    assert!(ordinary_queue_position(record).is_none());
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), original);
    assert_eq!(inner.engram_pending_checkpoint_receipts.entries.len(), 1);
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_off_checkpoint_missing_row_keeps_global_duty_and_custody() {
    let (state, project, session, transport, receiver) = pending_off_receipt();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions.remove(index);
    }
    assert!(state.resume_off_checkpoint_receipt(&session).is_err());
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
                    .resume_off_checkpoint_receipt(&callback_session)
                    .is_err(),
                "only the selected local accounting reservation may proceed"
            );
            let mut inner = callback_state.inner.lock().unwrap();
            let index = inner.find_session_index(&callback_session).unwrap();
            inner.sessions[index].engram.dispatch_generation += 1;
        }));
    });
    assert!(state.resume_off_checkpoint_receipt(&session).is_err());
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
    state.resume_off_checkpoint_receipt(&session).unwrap();
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
