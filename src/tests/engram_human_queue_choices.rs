// Exact legacy choices are provenance, never delivery evidence. The connected
// fixture writes real SQLite and holds only the existing write-ahead ACK.
use super::*;

fn selection(state: &AppState, session: &str) -> HumanPromptSelection {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    HumanPromptSelection {
        prompt: projected_pending_prompts(record)
            .into_iter()
            .next()
            .unwrap(),
        session_mutation_stamp: record.mutation_stamp,
    }
}

pub(super) struct ChoiceWriter {
    tx: mpsc::Sender<PersistRequest>,
    held: mpsc::Receiver<Box<PersistFence>>,
    thread: Option<std::thread::JoinHandle<()>>,
    hold_promotion: Arc<std::sync::atomic::AtomicBool>,
}

impl ChoiceWriter {
    pub(super) fn attach(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        let (held_tx, held) = mpsc::channel();
        state.persist_tx = tx.clone();
        let inner = state.inner.clone();
        let path = state.persistence_path.clone();
        let hold_promotion = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_hold = hold_promotion.clone();
        let thread = std::thread::spawn(move || {
            let mut cache = SqlitePersistConnectionCache::new();
            let mut fences = PersistFenceBatch::default();
            while let Ok(request) = rx.recv() {
                if matches!(request, PersistRequest::Shutdown) {
                    break;
                }
                if let PersistRequest::Fence(fence) = request {
                    if matches!(
                        &fence.target,
                        PersistFenceTarget::EngramProjectContainment(_)
                    ) && worker_hold.swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        held_tx.send(fence).unwrap();
                        continue;
                    }
                    if matches!(
                        &fence.target,
                        PersistFenceTarget::PromptDeliveryWriteAhead { .. }
                    ) {
                        let delta = collect_persist_delta_from_shared_state(&inner, 0);
                        persist_delta_via_cache(&mut cache, path.as_path(), &delta).unwrap();
                        let connection = cache.connection_for(path.as_path()).unwrap();
                        assert!(
                            fence.target.is_already_durable(connection).unwrap(),
                            "actual SQLite must prove the exact write-ahead"
                        );
                        held_tx.send(fence).unwrap();
                        continue;
                    }
                    fences.accept(PersistRequest::Fence(fence));
                }
                let delta = collect_persist_delta_from_shared_state(&inner, 0);
                persist_delta_with_fences(&mut cache, path.as_path(), &delta, &mut fences).unwrap();
            }
            fences.fail(PersistFenceError::WorkerStopped);
        });
        Self {
            tx,
            held,
            thread: Some(thread),
            hold_promotion,
        }
    }

    pub(super) fn write_ahead(&self) -> Box<PersistFence> {
        phase_sync::receive(&self.held, "the exact human-choice write-ahead")
    }

    pub(super) fn hold_next_promotion(&self) {
        self.hold_promotion
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for ChoiceWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(PersistRequest::Shutdown);
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("SQLite choice writer must not panic");
    }
}

#[test]
fn human_prompt_legacy_choice_without_a_receipt_records_choice_and_retains_unknown_content() {
    let (state, _project, session, receiver, transport) = queue::held_fixture();
    let selected = selection(&state, &session);
    let (head, duties, stamp) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            record.mutation_stamp,
        )
    };
    let before = transport.requests().len();
    let error = state
        .resume_selected_human_prompt(&session, selected)
        .err()
        .unwrap();
    assert!(error.message.contains("no matching delivery receipt"));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            duties
        );
        assert!(
            record.prompt_deliveries.is_empty(),
            "a choice cannot manufacture a receipt"
        );
        assert!(record.mutation_stamp > stamp);
        let choice = record
            .human_queue_choices
            .get("original-held-head")
            .unwrap();
        assert_eq!(choice.original_source, QueuedPromptSource::User);
        assert_eq!(choice.selected_prompt.id, "original-held-head");
        assert_eq!(choice.selected_prompt.text, "retained input");
        let saved = PersistedSessionRecord::from_record(record);
        let sparse = saved.clone_without_messages();
        let loaded =
            serde_json::from_value::<PersistedSessionRecord>(serde_json::to_value(sparse).unwrap())
                .unwrap()
                .into_record()
                .unwrap();
        assert_eq!(
            loaded.human_queue_choices["original-held-head"].decision_id,
            choice.decision_id
        );
        assert_eq!(
            serde_json::to_value(&loaded.queued_prompts[0]).unwrap(),
            head
        );
    }
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), before);
    let mut fresh = prompt();
    fresh.text = "A distinct new human request".into();
    let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => {
            panic!("old unknown content cannot strand a distinct new human request")
        }
    };
    assert_ne!(dispatch.human_prompt_id(), Some("original-held-head"));
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        matches!(phase_sync::receive(&receiver, "the distinct human prompt"), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("A distinct new human request"))
    );
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0]).unwrap(),
        head
    );
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        duties
    );
    assert_eq!(transport.requests().len(), before);
}

#[test]
fn human_prompt_legacy_selection_refuses_a_stale_content_or_session_version() {
    for change in ["text", "stamp"] {
        let (state, _project, session, receiver, _transport) = queue::held_fixture();
        let mut selected = selection(&state, &session);
        if change == "text" {
            selected.prompt.text.push_str(" changed");
        } else {
            selected.session_mutation_stamp = selected.session_mutation_stamp.wrapping_add(1);
        }
        assert!(
            state
                .resume_selected_human_prompt(&session, selected)
                .is_err()
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.human_queue_choices.is_empty());
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert!(receiver.try_recv().is_err());
    }
}

pub(super) fn legacy_pending_fixture() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<ScriptedEngramControlTransport>,
) {
    let (state, project, session, _root, receiver) = fixture();
    let legacy_id = "legacy-proven-pending";
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let mut pending = selection_prompt(legacy_id);
        pending.text = "Exactly this legacy content".into();
        queue_receipted_orchestrator_prompt_on_record(record, pending, Vec::new());
        // Historical fixture: construct its legacy User label before its
        // first durable save, with the genuine producer-owned Pending receipt.
        record.queued_prompts[0].source = QueuedPromptSource::User;
        record.queued_prompts[0].engram_interrupted = true;
        record
            .queued_prompt_durability_holds
            .get_mut(legacy_id)
            .unwrap()
            .content = queued_prompt_durability_content(&record.queued_prompts[0]);
        record.set_auto_dispatch_blocked(true);
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = ScriptedEngramControlTransport::new([]);
    install_control_only_transport(&state, transport.clone());
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    // The real loader proves the entry durable; it does not invent a receipt.
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded_record = loaded
        .sessions
        .into_iter()
        .find(|record| record.session.id == session)
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index] = loaded_record;
    }
    (state, project, session, receiver, transport)
}

#[test]
fn human_prompt_legacy_write_ahead_keeps_its_owner_across_the_generic_drain() {
    for change in ["cancel", "nonce"] {
        let (mut state, _project, session, receiver, transport) = legacy_pending_fixture();
        let legacy_id = "legacy-proven-pending";
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            queue_prompt_on_record_with_source(
                &mut inner.sessions[index],
                selection_prompt("independent-human"),
                vec![],
                QueuedPromptSource::Human,
            );
            inner.stamp_session_at_index(index);
            state.commit_locked(&mut inner).unwrap();
        }
        let writer = ChoiceWriter::attach(&mut state);
        state
            .resume_selected_human_prompt(&session, selection(&state, &session))
            .unwrap();
        let ack = writer.write_ahead();
        let nonce = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(!record.prompt_deliveries[legacy_id].write_ahead_acknowledged);
            record.prompt_deliveries[legacy_id].write_ahead.clone()
        };
        let concurrent = state.clone();
        let concurrent_session = session.clone();
        let captured_nonce = nonce.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let _hook = install_test_human_drain_capture_hook(
            &state,
            &session,
            Box::new(move || {
                {
                    let inner = concurrent.inner.lock().unwrap();
                    let record =
                        &inner.sessions[inner.find_session_index(&concurrent_session).unwrap()];
                    let receipt = &record.prompt_deliveries[legacy_id];
                    assert!(
                        receipt.write_ahead_acknowledged,
                        "actual SQLite ACK reached the production continuation"
                    );
                    assert_eq!(receipt.write_ahead, captured_nonce);
                    assert!(receipt.resume.is_some());
                    assert_eq!(record.queued_prompts[0].pending_prompt.id, legacy_id);
                }
                if change == "cancel" {
                    concurrent
                        .cancel_queued_prompt(&concurrent_session, legacy_id)
                        .unwrap();
                } else {
                    concurrent.return_unpromoted_write_ahead_to_pending(
                        &concurrent_session,
                        legacy_id,
                        &captured_nonce,
                    );
                }
                done_tx
            }),
        );
        // ChoiceWriter proved the actual WAL in SQLite before handing out
        // this fence. Its real callback enters the generic owner-bound drain.
        ack.finish(Ok(()));
        phase_sync::receive(
            &done_rx,
            "old WAL continuation returned without a successor send",
        );
        assert!(receiver.try_recv().is_err());
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.human_prompt_turn.is_none());
            if change == "cancel" {
                assert_eq!(record.queued_prompts.len(), 1);
                assert_eq!(
                    record.queued_prompts[0].pending_prompt.id,
                    "independent-human"
                );
            } else {
                assert_eq!(record.queued_prompts.len(), 2);
                assert_eq!(
                    record.prompt_deliveries[legacy_id].disposition,
                    PromptDeliveryDisposition::Pending
                );
                assert_ne!(record.prompt_deliveries[legacy_id].write_ahead, nonce);
            }
        }
        state.settle_prompt_delivery_write_ahead(&session, legacy_id, &nonce, Ok(()));
        assert!(
            receiver.try_recv().is_err(),
            "late old ACK cannot authorize any head"
        );
        let selected = selection(&state, &session);
        let expected = selected.prompt.text.clone();
        state
            .resume_selected_human_prompt(&session, selected)
            .unwrap();
        if change == "nonce" {
            writer.write_ahead().finish(Ok(()));
        }
        assert!(
            matches!(phase_sync::receive(&receiver, "fresh exact Resume owns its prompt"),
            CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains(&expected))
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            transport.requests().len(),
            2,
            "no new control request from legacy Human selection"
        );
    }
}

#[test]
fn human_prompt_legacy_pending_receipt_uses_real_write_ahead_and_fences_late_writers() {
    let (mut state, _project, session, receiver, transport) = legacy_pending_fixture();
    let legacy_id = "legacy-proven-pending";
    let (old_nonce, duties) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(human_choice_receipt_pending(record, legacy_id));
        (
            record.prompt_deliveries[legacy_id].write_ahead.clone(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        )
    };
    let writer = ChoiceWriter::attach(&mut state);
    state
        .resume_selected_human_prompt(&session, selection(&state, &session))
        .unwrap();
    let ack = writer.write_ahead();
    assert!(
        receiver.try_recv().is_err(),
        "a durable write-ahead without its ACK cannot hand off"
    );
    ack.finish(Ok(()));
    let command = match phase_sync::receive(&receiver, "the once-only legacy human selection") {
        CodexRuntimeCommand::Prompt { command, .. } => command,
        _ => panic!("unexpected runtime command"),
    };
    assert!(command.prompt.contains("Exactly this legacy content"));
    assert!(receiver.try_recv().is_err());
    let receipt = command
        .delivery_receipt
        .as_ref()
        .expect("existing receipt owner must reach the runtime");
    assert_eq!(receipt.prompt_id, legacy_id);
    assert_ne!(receipt.write_ahead, old_nonce);
    let token = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.prompt_deliveries[legacy_id]
                .entry
                .as_ref()
                .unwrap()
                .source,
            QueuedPromptSource::User
        );
        assert_eq!(
            record.human_queue_choices[legacy_id].selected_prompt.id,
            legacy_id
        );
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            duties
        );
        record.runtime.runtime_token().unwrap()
    };
    let stale = CodexPromptReceipt {
        prompt_id: legacy_id.into(),
        write_ahead: old_nonce,
    };
    assert!(!state.begin_prompt_delivery_runtime_write(
        &session,
        &token,
        command.active_turn_generation,
        Some(&stale)
    ));
    assert!(state.begin_prompt_delivery_runtime_write(
        &session,
        &token,
        command.active_turn_generation,
        Some(receipt)
    ));
    assert!(!state.begin_prompt_delivery_runtime_write(
        &session,
        &token,
        command.active_turn_generation,
        Some(receipt)
    ));
    assert_eq!(
        transport.requests().len(),
        2,
        "no control RPC from human reuse"
    );
}

#[test]
fn human_prompt_legacy_handing_off_reload_keeps_choice_and_ambiguity_without_replay() {
    let (mut state, _project, session, receiver, transport) = legacy_pending_fixture();
    let legacy_id = "legacy-proven-pending";
    let writer = ChoiceWriter::attach(&mut state);
    state
        .resume_selected_human_prompt(&session, selection(&state, &session))
        .unwrap();
    let ack = writer.write_ahead();
    let (nonce, attempt) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            record.prompt_deliveries[legacy_id].write_ahead.clone(),
            record.prompt_deliveries[legacy_id].attempt,
        )
    };
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded_record = loaded
        .sessions
        .into_iter()
        .find(|record| record.session.id == session)
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert_eq!(
            loaded_record.prompt_deliveries[legacy_id].disposition,
            PromptDeliveryDisposition::Ambiguous
        );
        assert_eq!(loaded_record.prompt_deliveries[legacy_id].attempt, attempt);
        assert_eq!(
            loaded_record.prompt_deliveries[legacy_id].write_ahead,
            nonce
        );
        assert_eq!(
            loaded_record.human_queue_choices[legacy_id]
                .selected_prompt
                .id,
            legacy_id
        );
        assert_eq!(
            loaded_record.human_queue_choices[legacy_id]
                .selected_prompt
                .text,
            "Exactly this legacy content"
        );
        assert_eq!(
            loaded_record.human_queue_choices[legacy_id].original_source,
            QueuedPromptSource::User
        );
        assert!(!human_choice_receipt_pending(&loaded_record, legacy_id));
        assert!(!human_choice_receipt_ready(&loaded_record, legacy_id));
        inner.sessions[index] = loaded_record;
    }
    state.settle_prompt_delivery_write_ahead(&session, legacy_id, &nonce, Ok(()));
    assert!(
        receiver.try_recv().is_err(),
        "an old ACK cannot convert reloaded ambiguity into permission"
    );
    ack.finish(Ok(()));
    let mut fresh = prompt();
    fresh.text = "Fresh work after an ambiguous earlier attempt".into();
    let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => {
            panic!("an unrelated ambiguous delivery must not strand a new human request")
        }
    };
    assert_ne!(dispatch.human_prompt_id(), Some(legacy_id));
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        matches!(phase_sync::receive(&receiver, "only the distinct new human request"), CodexRuntimeCommand::Prompt { command, .. }
        if command.prompt.contains("Fresh work after an ambiguous earlier attempt"))
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), 2);
}

fn selection_prompt(id: &str) -> PendingPrompt {
    PendingPrompt {
        id: id.into(),
        timestamp: stamp_now(),
        text: "legacy fixture".into(),
        expanded_text: None,
        source: None,
        attachments: Vec::new(),
        engram_interrupted: false,
        is_engram_retained: false,
    }
}
