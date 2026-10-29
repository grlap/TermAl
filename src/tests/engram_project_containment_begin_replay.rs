// Owns acknowledged-transfer Begin replay selection and explicit Resume races.
// Does not change generic unknown-Begin policy or exercise live authority.
use super::*;

fn unknown_begin() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostContainmentAdmissionReply>,
) {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["turn_evaluate", "turn_begin"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let _ = state.resume_session_queue(&session);
    assert!(transport.transfer_seen.load(Ordering::SeqCst));
    assert!(receiver.try_recv().is_err());
    {
        let inner = state.inner.lock().unwrap();
        assert!(containment_begin_replay_entry(&inner, &session, None).is_some());
    }
    (state, project, session, receiver, transport)
}

#[test]
fn project_containment_begin_replay_requires_current_exact_successor_and_latest_cause() {
    for change in [
        "token",
        "key",
        "delivery",
        "grant",
        "head",
        "payload",
        "owner",
        "settings",
        "refusal",
        "token_refusal",
        "attempt",
        "flight",
        "duty",
        "cancel",
        "busy",
        "stop",
        "checkpoint",
        "boot",
        "active_grant",
        "handoff",
    ] {
        let (state, project, session, receiver, transport) = unknown_begin();
        let before = transport.inner.requests().len();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            match change {
                "token" => record.engram.routing_token = Some("other-token".into()),
                "owner" => record.engram.dispatch_generation += 1,
                "flight" => {
                    record.engram.admission_in_progress = Some(Arc::new(AtomicBool::new(false)))
                }
                "busy" => record.session.status = SessionStatus::Active,
                "stop" => record.runtime_stop_in_progress = true,
                "checkpoint" => record.engram.checkpoint_in_progress = true,
                "boot" => record.engram_boot_recovery_pending = true,
                "active_grant" => {
                    record.engram.active_grant_id = record.engram.uncertain_grant_id.clone()
                }
                "handoff" => record.queued_prompts[0].engram_ordinary_handoff = true,
                "cancel" => record.queued_prompts.clear(),
                "refusal" | "token_refusal" | "attempt" => {
                    let card = record
                        .session
                        .messages
                        .matching_mut(|message| matches!(message, Message::EngramControl { .. }))
                        .rev()
                        .find_map(|(_, message)| match message {
                            Message::EngramControl { card, .. } => Some(card),
                            _ => None,
                        })
                        .unwrap();
                    let cause = card.causal_failure.as_mut().unwrap();
                    if change == "attempt" {
                        cause.attempt_id = Some("another-begin".into());
                    } else if change == "token_refusal" {
                        cause.failure_class = EngramCausalFailureClass::Remote;
                        cause.original_code = Some("invalid_routing_token".into());
                    } else {
                        cause.failure_class = EngramCausalFailureClass::ProducerRefusal;
                    }
                }
                "key" | "delivery" | "grant" => {
                    let prepared = record.queued_prompts[0]
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .prepared_begin
                        .as_mut()
                        .unwrap();
                    match change {
                        "key" => prepared.idempotency_key.push_str("-changed"),
                        "delivery" => prepared.delivery_tokens.push("another-delivery".into()),
                        _ => prepared.grant_id.push_str("-changed"),
                    }
                }
                "head" => record.queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-changed"),
                "payload" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "settings" | "duty" => {}
                _ => unreachable!(),
            }
            if change == "settings" {
                let project = inner.projects.iter_mut().find(|p| p.id == project).unwrap();
                project.engram.as_mut().unwrap().turn_gated_control = false;
            }
            if change == "duty" {
                let mut entry = inner.engram_session_obligations.entries[0].clone();
                entry.id = Uuid::new_v4().to_string();
                entry.phase = EngramSessionObligationPhase::Intended;
                entry.settlement_evidence = Value::Null;
                inner.engram_session_obligations.entries.push(entry);
            }
            assert!(
                containment_begin_replay_entry(&inner, &session, None).is_none(),
                "{change}"
            );
        }
        // The selector itself is observational and never asks for replacement
        // authority. Invalid shapes remain on the ordinary held path.
        assert!(
            state
                .begin_contained_begin_replay_resume(&session)
                .unwrap()
                .is_none(),
            "{change}"
        );
        let _ = state.resume_session_queue(&session);
        assert_eq!(transport.inner.requests().len(), before, "{change}");
        assert!(receiver.try_recv().is_err(), "{change}");
    }
}

#[test]
fn project_containment_begin_replay_reserves_before_ack_and_releases_only_its_owner() {
    let (state, _project, session, receiver, transport) = unknown_begin();
    let before = transport.inner.requests().len();
    let scope = state
        .begin_contained_begin_replay_resume(&session)
        .unwrap()
        .unwrap();
    assert!(state.begin_contained_begin_replay_resume(&session).is_err());
    std::thread::scope(|threads| {
        threads
            .spawn(|| assert!(state.resume_session_queue(&session).is_err()))
            .join()
            .unwrap();
    });
    {
        let inner = state.inner.lock().unwrap();
        assert!(containment_begin_replay_resume_allowed(&inner, &session));
    }
    drop(scope);
    assert_eq!(transport.inner.requests().len(), before);
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let requests = transport.inner.requests();
    let begins = requests
        .iter()
        .filter(|r| r.request["operation"] == "turn_begin")
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 2);
    assert_eq!(begins[0].request, begins[1].request);
    let operations = requests
        .iter()
        .map(|r| r.request["operation"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        vec![
            "session_bind",
            "turn_evaluate",
            "session_status",
            "turn_evaluate",
            "turn_begin",
            "turn_begin"
        ]
    );
    state.resume_session_queue(&session).unwrap();
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_begin_replay_requires_ack_and_rejects_ack_owner_drift() {
    let (mut state, _project, session, receiver, transport) = unknown_begin();
    let before = transport.inner.requests().len();
    let original_tx = state.persist_tx.clone();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    std::thread::scope(|threads| {
        let state = &state;
        let session = &session;
        let writer = threads.spawn(move || {
            loop {
                if let PersistRequest::Fence(fence) =
                    phase_sync::receive(&rx, "exact Begin replay writer fence")
                {
                    let mut inner = state.inner.lock().unwrap();
                    state.persist_internal_locked(&inner).unwrap();
                    let index = inner.find_session_index(session).unwrap();
                    // Concurrent Resume must observe the reservation even while
                    // the first request waits for a real materialized ACK.
                    drop(inner);
                    assert!(state.resume_session_queue(session).is_err());
                    inner = state.inner.lock().unwrap();
                    inner.sessions[index].engram.dispatch_generation += 1;
                    drop(inner);
                    fence.finish(Ok(()));
                    break;
                }
            }
        });
        assert!(state.begin_contained_begin_replay_resume(&session).is_err());
        writer.join().unwrap();
    });
    state.persist_tx = original_tx;
    assert_eq!(transport.inner.requests().len(), before);
    assert!(receiver.try_recv().is_err());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert!(
            inner.sessions[index]
                .engram
                .containment_resume_owner
                .is_none()
        );
        inner.sessions[index].engram.dispatch_generation -= 1;
        inner.engram_confirmed_containment_ids.clear();
        assert!(
            containment_begin_replay_entry(&inner, &session, None).is_none(),
            "historical Confirmed is not an ACK"
        );
    }
}

#[test]
fn project_containment_begin_replay_unacknowledged_transfer_never_sends() {
    let (state, _project, session, receiver, transport) = unknown_begin();
    let before = transport.inner.requests().len();
    state
        .inner
        .lock()
        .unwrap()
        .engram_confirmed_containment_ids
        .clear();
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
    let result = state.resume_session_queue(&session);
    TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
    assert!(result.is_err());
    assert_eq!(transport.inner.requests().len(), before);
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
}

#[test]
fn project_containment_restart_does_not_lend_a_transfer_to_a_missing_head() {
    let (state, _project, session, receiver, transport) = unknown_begin();
    let before = transport.inner.requests().len();
    let uncertainty = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let uncertainty = inner.sessions[index].engram.uncertain_grant_id.clone();
        // A cancelled/replaced queue cannot inherit an old transfer, even
        // when the transfer itself was genuinely acknowledged before restart.
        inner.sessions[index].queued_prompts.clear();
        state.commit_locked(&mut inner).unwrap();
        uncertainty
    };
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.uncertain_grant_id, uncertainty);
    assert!(record.queued_prompts.is_empty());
    assert!(containment_begin_replay_entry(&loaded, &session, None).is_none());
    assert!(
        loaded
            .engram_session_obligations
            .entries
            .iter()
            .any(containment_admission_transfer_matches)
    );
    assert!(loaded.engram_confirmed_containment_ids.is_empty());
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    assert_eq!(transport.inner.requests().len(), before);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_boot_releases_only_the_exact_retained_successor() {
    for change in [
        "matching", "head", "payload", "key", "grant", "delivery", "token", "evaluate", "owner",
    ] {
        let (state, _project, session, receiver, transport) = unknown_begin();
        let before = transport.inner.requests().len();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            match change {
                "matching" => {}
                "head" => record.queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-replacement"),
                "payload" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" replacement"),
                "token" => record.engram.routing_token = Some("replacement-token".into()),
                "owner" => record.engram.dispatch_generation += 1,
                "evaluate" => {
                    if let EngramControlRequest::TurnEvaluate {
                        idempotency_key, ..
                    } = &mut record.queued_prompts[0]
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .request
                    {
                        idempotency_key.push_str("-replacement");
                    }
                }
                "key" | "grant" | "delivery" => {
                    let prepared = record.queued_prompts[0]
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .prepared_begin
                        .as_mut()
                        .unwrap();
                    match change {
                        "key" => prepared.idempotency_key.push_str("-replacement"),
                        "grant" => prepared.grant_id.push_str("-replacement"),
                        _ => prepared.delivery_tokens.push("replacement-delivery".into()),
                    }
                }
                _ => unreachable!(),
            }
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let released = loaded
            .engram_confirmed_containment_ids
            .contains(&loaded.engram_session_obligations.entries[0].id);
        assert_eq!(released, change == "matching", "{change}");
        assert_eq!(
            engram_session_obligation_locked(&loaded, &session).is_none(),
            change == "matching",
            "{change}"
        );
        assert_eq!(
            transport.inner.requests().len(),
            before,
            "boot does not acquire authority"
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_boot_keeps_an_explicitly_advanced_successor_released() {
    let (state, _project, session, receiver, transport) = unknown_begin();
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let before = transport.inner.requests().len();
    {
        let inner = state.inner.lock().unwrap();
        let transfer: EngramContainmentAdmissionTransfer = serde_json::from_value(
            inner.engram_session_obligations.entries[0]
                .settlement_evidence
                .clone(),
        )
        .unwrap();
        assert!(
            transfer.progress.is_some(),
            "matching Begin durably transfers to its ordinary grant owner"
        );
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .engram_confirmed_containment_ids
            .contains(&loaded.engram_session_obligations.entries[0].id)
    );
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    assert_eq!(transport.inner.requests().len(), before);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_repeated_off_retains_the_exact_transferred_begin_duty() {
    let (state, project, session, receiver, transport) = unknown_begin();
    let begins = transport
        .inner
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "turn_begin")
        .count();
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .expect("a transferred operation can be retained as its exact independent successor duty");
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.engram_session_obligations.entries.len(), 2);
    assert!(loaded.engram_session_obligations.validate().is_ok());
    assert!(engram_session_obligation_locked(&loaded, &session).is_some());
    assert!(require_no_engram_session_obligation(&loaded, &session).is_err());
    let history = loaded
        .engram_session_obligations
        .entries
        .iter()
        .find(|entry| containment_admission_transfer_matches(entry))
        .unwrap()
        .clone();
    let mut changed = loaded
        .engram_session_obligations
        .entries
        .iter()
        .find(|entry| entry.id != history.id)
        .unwrap()
        .clone();
    let mut image: EngramUnsettledControlImage =
        serde_json::from_value(changed.frozen_request.clone()).unwrap();
    assert!(image.queued_prompts[0].promoted_message_index.is_some());
    image.dispatch_generation += 1;
    changed.owner_generation += 1;
    changed.frozen_request = serde_json::to_value(image).unwrap();
    assert!(
        changed.validate().is_ok(),
        "the control image remains structurally valid"
    );
    assert!(
        !containment_admission_retained_duty_successor(&history, &changed),
        "a promoted successor cannot borrow the unpromoted owner's numeric generation"
    );
    assert!(
        EngramSessionObligationLedger::empty()
            .with_entry(history)
            .unwrap()
            .with_entry(changed)
            .is_err(),
        "wire-key reuse requires the supported exact owner lineage"
    );
    assert_eq!(
        transport
            .inner
            .requests()
            .iter()
            .filter(|request| request.request["operation"] == "turn_begin")
            .count(),
        begins
    );
    assert!(receiver.try_recv().is_err());
}

struct ReplaceOwnerAfterBeginReply {
    inner: Arc<LostContainmentAdmissionReply>,
    state: std::sync::Weak<StateMutex<StateInner>>,
}

impl EngramControlTransport for ReplaceOwnerAfterBeginReply {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let response = self.inner.request(connection, request, timeout)?;
        if matches!(request, EngramControlRequest::TurnBegin { .. }) {
            let state = self.state.upgrade().unwrap();
            let mut inner = state.lock().unwrap();
            let index = inner.find_session_index(&connection.session_id).unwrap();
            inner.sessions[index].active_turn_generation += 1;
            inner.sessions[index].engram.dispatch_generation += 1;
        }
        Ok(response)
    }
    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn project_containment_begin_replay_receipt_cannot_deliver_to_a_replaced_provider_owner() {
    let (state, _project, session, receiver, transport) = unknown_begin();
    state.install_control_test_transport(Arc::new(ReplaceOwnerAfterBeginReply {
        inner: transport.clone(),
        state: Arc::downgrade(&state.inner),
    }));
    let _ = state.resume_session_queue(&session);
    assert!(receiver.try_recv().is_err());
    let requests = transport.inner.requests();
    let begins = requests
        .iter()
        .filter(|r| r.request["operation"] == "turn_begin")
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 2);
    assert_eq!(begins[0].request, begins[1].request);
    // The matching receipt proves Begin applied, but its delivery owner is
    // stale. Existing response cleanup closes that exact grant; it must not
    // replace admission or deliver the prompt to the changed provider owner.
    let operations = requests
        .iter()
        .map(|r| r.request["operation"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        vec![
            "session_bind",
            "turn_evaluate",
            "session_status",
            "turn_evaluate",
            "turn_begin",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
    let cleanup = &requests[6];
    assert_eq!(cleanup.connection, begins[1].connection);
    assert_eq!(cleanup.connection.session_id, session);
    assert_eq!(
        cleanup.request["routing_token"],
        begins[1].request["routing_token"]
    );
    assert_eq!(cleanup.request["grant_id"], begins[1].request["grant_id"]);
    assert_eq!(cleanup.request["next_intent"], "exit");
    assert_eq!(
        cleanup.request["idempotency_key"],
        format!(
            "termal-stale-begin-checkpoint:{session}:{}",
            begins[1].request["grant_id"].as_str().unwrap()
        )
    );
}

#[test]
fn project_containment_promotion_permit_rejects_mixed_owner_tuples() {
    let (state, project, session, _root, _receiver, _transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let _scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let (intent, permit) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let permit = ENGRAM_CONTAINMENT_ADMISSION.with(|p| p.borrow().last().unwrap().clone());
        (
            EngramTurnIntentSnapshot {
                session_id: session.clone(),
                dispatch_generation: permit.replay_generation,
                intent_fingerprint: containment_prompt_fingerprint(&record.queued_prompts[0]),
            },
            permit,
        )
    };
    let pending = state.evaluate_engram_turn_off_lock(&intent).unwrap();
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    assert_ne!(
        permit.generation, permit.replay_generation,
        "fixture discriminates mixed tuples"
    );
    inner.sessions[index].engram.pending_dispatch = Some(pending);
    let entry = engram_session_obligation_locked(&inner, &session)
        .unwrap()
        .clone();
    for (generation, active, allowed) in [
        (permit.generation, permit.active_turn_generation, true),
        (
            permit.replay_generation,
            permit.active_turn_generation + 1,
            true,
        ),
        (permit.generation, permit.active_turn_generation + 1, false),
        (
            permit.replay_generation,
            permit.active_turn_generation,
            false,
        ),
    ] {
        inner.sessions[index].engram.dispatch_generation = generation;
        inner.sessions[index].active_turn_generation = active;
        assert_eq!(
            containment_admission_permit_matches(&inner, &entry, &session, &permit),
            allowed
        );
    }
}
