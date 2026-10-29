// Owns ordinary ON prompt intake while a closing checkpoint retains authority.
// Does not authorize admission, provider fallback, or recovery scheduling.
use super::*;

fn checkpoint_owner_image(state: &AppState, session: &str) -> Value {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    json!({
        "operation": record.engram.checkpoint_operation,
        "generation": record.engram.dispatch_generation,
        "active": record.active_turn_generation,
        "runtime": format!("{:?}", record.runtime.runtime_token()),
        "pause": record.orchestrator_auto_dispatch_blocked,
        "token": record.engram.routing_token,
        "grant": record.engram.active_grant_id,
    })
}

#[test]
fn project_containment_checkpoint_on_send_queues_during_rpc_without_borrowing_owner() {
    let (state, _project, session, _root, receiver) = fixture();
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
    let before = checkpoint_owner_image(&state, &session);
    let result = state.dispatch_turn(&session, prompt());
    let during = checkpoint_owner_image(&state, &session);
    let queued = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            record.queued_prompts.len(),
            record.engram.checkpoint_in_progress,
            engram_session_obligation_locked(&inner, &session).is_some(),
        )
    };
    let no_provider = receiver.try_recv().is_err();
    let only_checkpoint = transport.requests().len() == 1;
    // Always release/join before assertions so a causal RED cannot strand a worker.
    gate.release();
    let outcome = closing.join().unwrap();
    assert!(
        matches!(result, Ok(DispatchTurnResult::Queued)),
        "ordinary ON Send must queue behind the checkpoint RPC"
    );
    assert_eq!(
        before, during,
        "queue intake must not borrow or mutate the close owner"
    );
    assert_eq!(queued, (1, true, true));
    assert!(no_provider && only_checkpoint);
    assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert!(!record.engram.checkpoint_in_progress);
    assert_eq!(
        serde_json::to_value(
            &record
                .engram
                .checkpoint_operation
                .as_ref()
                .unwrap()
                .original
                .request
        )
        .unwrap(),
        request.request
    );
    drop(inner);
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.sessions[loaded.find_session_index(&session).unwrap()]
            .queued_prompts
            .len(),
        1
    );
}

#[test]
fn project_containment_checkpoint_on_send_queues_while_presend_ack_is_held() {
    let (state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    state.install_control_test_transport(transport.clone());
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                entered_tx.send(()).unwrap();
                let _ = release_rx.recv();
            }));
        });
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
    receive(&entered_rx, "checkpoint pre-send ACK");
    let before = checkpoint_owner_image(&state, &session);
    let result = state.dispatch_turn(&session, prompt());
    let during = checkpoint_owner_image(&state, &session);
    let no_transport = transport.requests().is_empty();
    let no_provider = receiver.try_recv().is_err();
    // Disconnect also releases the hook if the dispatch returned an error.
    drop(release_tx);
    let outcome = closing.join().unwrap();
    assert!(
        matches!(result, Ok(DispatchTurnResult::Queued)),
        "local ACK latency must not reject valid prompt intake"
    );
    assert_eq!(before, during);
    assert!(no_transport && no_provider);
    assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_checkpoint_on_unknown_result_keeps_new_prompt_queued_and_exact_duty() {
    let (state, _project, session, _root, receiver) = fixture();
    let transport =
        ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("fixture checkpoint reply lost"),
        ))]);
    state.install_control_test_transport(transport.clone());
    let outcome = state.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    assert!(matches!(outcome, EngramCheckpointOutcome::Failed(_)));
    let before = checkpoint_owner_image(&state, &session);
    let result = state.dispatch_turn(&session, prompt());
    assert!(
        matches!(result, Ok(DispatchTurnResult::Queued)),
        "Unknown duty withholds execution, not ordinary queue intake"
    );
    assert_eq!(checkpoint_owner_image(&state, &session), before);
    assert!(
        state
            .dispatch_next_queued_turn(&session, false)
            .unwrap()
            .is_none()
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(!record.queued_prompts[0].has_engram_intent());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert!(!record.engram.checkpoint_in_progress);
    let operation = record.engram.checkpoint_operation.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(&operation.original.request).unwrap(),
        transport.requests()[0].request
    );
    assert!(matches!(
        operation.attempts.last().unwrap().result,
        Some(EngramCheckpointAttemptResult::Unknown { .. })
    ));
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_absent_lookup_does_not_validate_unrelated_ledger() {
    let (state, _project, session, _root, _receiver) = fixture();
    let mut inner = state.inner.lock().unwrap();
    inner
        .projects
        .iter_mut()
        .for_each(|project| project.engram = None);
    TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.set(0));
    assert!(EngramCheckpointRecoveryView::capture(&inner, &session).is_none());
    let visits = TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.get());
    assert_eq!(
        visits, 0,
        "a non-Engram session without an operation needs no whole-ledger walk"
    );
}

#[test]
fn project_containment_checkpoint_selected_lookup_is_local_but_ledger_only_duty_still_excludes() {
    let (state, _project, session, _transport) = checkpoint_with_unacknowledged_send();
    let mut inner = state.inner.lock().unwrap();
    TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.set(0));
    assert!(EngramCheckpointRecoveryView::capture(&inner, &session).is_some());
    let visits = TEST_ENGRAM_FULL_LEDGER_VALIDATIONS.with(|count| count.get());
    assert_eq!(
        visits, 0,
        "selected immutable validation must not serialize unrelated reports"
    );
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].engram.checkpoint_operation = None;
    assert!(EngramCheckpointRecoveryView::capture(&inner, &session).is_none());
    assert!(
        require_no_engram_session_obligation(&inner, &session).is_err(),
        "a missing mirror does not erase the independent ledger duty"
    );
}

#[test]
fn project_containment_checkpoint_on_mailbox_intake_keeps_provenance_order_and_pause() {
    let (state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Err(EngramTransportError::deadline("lost close reply")),
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
            None
        ),
        EngramCheckpointOutcome::Failed(_)
    ));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    let before = checkpoint_owner_image(&state, &session);
    for sequence in [9, 7] {
        let mut request = prompt();
        request.text = format!("Mailbox wake {sequence}");
        request.source_session_id = Some(session.clone());
        request.source_mailbox = Some(MailboxMessageSource {
            mailbox_id: "checkpoint-mailbox".into(),
            message_id: format!("wake-{sequence}"),
            sequence,
            unread_count: sequence,
        });
        assert!(matches!(
            state.dispatch_turn(&session, request).unwrap(),
            DispatchTurnResult::Queued
        ));
    }
    let mut empty = prompt();
    empty.text.clear();
    assert!(state.dispatch_turn(&session, empty).is_err());
    assert_eq!(checkpoint_owner_image(&state, &session), before);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
    assert_eq!(record.queued_prompts.len(), 1);
    let queued = &record.queued_prompts[0];
    assert_eq!(queued.source, QueuedPromptSource::Mailbox);
    assert!(!queued.has_engram_intent());
    assert_eq!(queued.pending_prompt.text, "Mailbox wake 9");
    let source = queued.pending_prompt.source.as_ref().unwrap();
    assert!(source.is_mailbox());
    assert_eq!(source.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(source.mailbox.as_ref().unwrap().sequence, 9);
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_selected_lookup_rejects_duplicate_selected_identity() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let mut inner = state.inner.lock().unwrap();
    let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
    inner
        .engram_session_obligations
        .checkpoint_operations
        .push(operation);
    assert!(EngramCheckpointRecoveryView::capture(&inner, &session).is_none());
    assert!(require_no_engram_session_obligation(&inner, &session).is_err());
    assert!(transport.requests().is_empty());
}
