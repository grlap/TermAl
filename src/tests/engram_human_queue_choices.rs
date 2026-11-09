//! Exact ordinary choices and real receipt/WAL ownership, beside passive history.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;

pub(super) fn selection(state: &AppState, session: &str) -> HumanPromptSelection {
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
}

impl ChoiceWriter {
    pub(super) fn attach(state: &mut AppState) -> Self {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        let (held_tx, held) = mpsc::channel();
        state.persist_tx = tx.clone();
        let inner = state.inner.clone();
        let path = state.persistence_path.clone();
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
        }
    }

    pub(super) fn write_ahead(&self) -> Box<PersistFence> {
        phase_sync::receive(&self.held, "the exact human-choice write-ahead")
    }
}

impl Drop for ChoiceWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(PersistRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if std::thread::panicking() {
                if result.is_err() {
                    use std::io::Write as _;
                    let _ = writeln!(
                        std::io::stderr(),
                        "SQLite choice writer also panicked during test unwind"
                    );
                }
            } else {
                result.expect("SQLite choice writer must not panic");
            }
        }
    }
}

pub(super) fn receipted_pending_fixture() -> OrdinaryRoot {
    let turn = OrdinaryRoot::new("receipted-exact-resume");
    let source = QueuedPromptSource::User;
    recovery::install_passive_original(&turn);
    turn.edit(|record| {
        let mut pending = selection_prompt("legacy-proven-pending");
        pending.text = "Exactly this legacy content".into();
        queue_receipted_orchestrator_prompt_on_record(record, pending, Vec::new());
        // The historical User label is fixed before the first durable save.
        record.queued_prompts[0].source = source;
        record.queued_prompts[0].engram_interrupted = true;
        record
            .queued_prompt_durability_holds
            .get_mut("legacy-proven-pending")
            .unwrap()
            .content = queued_prompt_durability_content(&record.queued_prompts[0]);
        record.human_queue_choices.insert(
            "legacy-proven-pending".into(),
            json!({
                "decisionId":"historical-choice-only", "originalSource":source,
                "selectedPrompt":record.queued_prompts[0].pending_prompt,
                "originalPayload":{"text":"Original choice\nżółw 🐢"}
            }),
        );
        record.set_auto_dispatch_blocked(true);
    });
    // Only the real loader proves the immutable entry durable.
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded_record = loaded
        .sessions
        .into_iter()
        .find(|record| record.session.id == turn.session)
        .unwrap();
    let runtime = turn.record(|record| record.runtime.clone());
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session).unwrap();
        inner.sessions[index] = loaded_record;
        inner.sessions[index].runtime = runtime;
    }
    turn
}

#[test]
fn human_prompt_unknown_choice_without_a_receipt_retains_content_and_history() {
    let turn = queue::retained_queue_fixture();
    let state = turn.state.clone();
    let session = turn.session.clone();
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
    let error = state
        .resume_selected_human_prompt(&session, selected)
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
        assert!(
            record.prompt_deliveries.is_empty(),
            "selection cannot manufacture a receipt"
        );
        assert!(
            record.human_queue_choices.is_empty(),
            "refusal cannot write a new authorization"
        );
        assert_eq!(record.mutation_stamp, stamp);
        let sparse = PersistedSessionRecord::from_record(record).clone_without_messages();
        let loaded = serde_json::from_value::<PersistedSessionRecord>(
            serde_json::to_value(&sparse).unwrap(),
        )
        .unwrap()
        .into_record()
        .unwrap();
        let mut normalized = head.clone();
        normalized["promotion_disposition_known"] = json!(true);
        assert_eq!(
            serde_json::to_value(&loaded.queued_prompts[0]).unwrap(),
            normalized,
            "complete transcript proves no promotion without changing original content"
        );
        assert!(loaded.prompt_deliveries.is_empty());
        let mut truncated = sparse;
        truncated.message_start_index = 1;
        let unknown = truncated.into_record().unwrap();
        assert_eq!(
            serde_json::to_value(&unknown.queued_prompts[0]).unwrap(),
            head,
            "truncated history cannot prove that the old prompt was never promoted"
        );
        assert!(unknown.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    let mut fresh = prompt();
    fresh.text = "A distinct new human request".into();
    let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => {
            panic!("unknown content cannot strand distinct explicit intake")
        }
    };
    assert_ne!(dispatch.human_prompt_id(), Some("original-held-head"));
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        turn.prompt()
            .prompt
            .contains("A distinct new human request")
    );
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        )
    });
    recovery::assert_passive_original(&turn, &duties);
}

#[test]
fn human_prompt_selection_refuses_a_stale_content_or_session_version() {
    for change in ["text", "stamp"] {
        let turn = receipted_pending_fixture();
        let before = turn.saved();
        let mut selected = selection(&turn.state, &turn.session);
        if change == "text" {
            selected.prompt.text.push_str(" changed");
        } else {
            selected.session_mutation_stamp = selected.session_mutation_stamp.wrapping_add(1);
        }
        assert!(
            turn.state
                .resume_selected_human_prompt(&turn.session, selected)
                .is_err()
        );
        assert_eq!(turn.saved(), before);
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn human_prompt_write_ahead_keeps_its_original_owner_after_cancel_or_nonce_change() {
    for change in ["cancel", "nonce"] {
        let turn = receipted_pending_fixture();
        let mut state = turn.state.clone();
        let session = turn.session.clone();
        let original_choices = turn.record(|record| record.human_queue_choices.clone());
        turn.edit(|record| {
            queue_prompt_on_record_with_source(
                record,
                selection_prompt("independent-human"),
                vec![],
                QueuedPromptSource::Human,
            )
        });
        let writer = ChoiceWriter::attach(&mut state);
        state
            .resume_selected_human_prompt(&session, selection(&state, &session))
            .unwrap();
        let ack = writer.write_ahead();
        let nonce = turn.record(|record| {
            let receipt = &record.prompt_deliveries["legacy-proven-pending"];
            assert!(!receipt.write_ahead_acknowledged);
            assert!(receipt.resume.is_some());
            assert_eq!(
                record.queued_prompts[0].pending_prompt.id,
                "legacy-proven-pending"
            );
            receipt.write_ahead.clone()
        });
        turn.assert_no_prompt();
        // The writer proved the real SQLite WAL, but its ACK still belongs
        // to this User prompt/nonce, never the independently queued Human.
        if change == "cancel" {
            state
                .cancel_queued_prompt(&session, "legacy-proven-pending")
                .unwrap();
        } else {
            state.settle_prompt_delivery_write_ahead(
                &session,
                "legacy-proven-pending",
                &nonce,
                Err(PersistFenceError::WriteFailed(
                    "injected failure before the held ACK".into(),
                )),
            );
            turn.record(|record| {
                let receipt = &record.prompt_deliveries["legacy-proven-pending"];
                assert_eq!(receipt.disposition, PromptDeliveryDisposition::Pending);
                assert_ne!(
                    receipt.write_ahead, nonce,
                    "the failure must supersede the nonce before the old ACK is released"
                );
                assert!(!receipt.write_ahead_acknowledged);
                assert!(receipt.resume.is_none());
            });
        }
        let callback_state = state.clone();
        let callback_session = session.clone();
        let callback_nonce = nonce.clone();
        let ack = (*ack).on_resolved(move |result| {
            callback_state.settle_prompt_delivery_write_ahead(
                &callback_session,
                "legacy-proven-pending",
                &callback_nonce,
                result,
            );
        });
        std::thread::spawn(move || ack.finish(Ok(())))
            .join()
            .unwrap();
        turn.assert_no_prompt();
        turn.record(|record| {
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
                    record.prompt_deliveries["legacy-proven-pending"].disposition,
                    PromptDeliveryDisposition::Pending
                );
                assert_ne!(
                    record.prompt_deliveries["legacy-proven-pending"].write_ahead,
                    nonce
                );
                assert_eq!(record.human_queue_choices, original_choices);
            }
        });
        state.settle_prompt_delivery_write_ahead(&session, "legacy-proven-pending", &nonce, Ok(()));
        turn.assert_no_prompt();
        let selected = selection(&state, &session);
        let expected_id = selected.prompt.id.clone();
        let expected_text = selected.prompt.text.clone();
        state
            .resume_selected_human_prompt(&session, selected)
            .unwrap();
        if change == "nonce" {
            writer.write_ahead().finish(Ok(()));
        }
        let command = turn.prompt();
        assert!(command.prompt.contains(&expected_text));
        if change == "cancel" {
            turn.record(|record| {
                assert_eq!(
                    record.human_prompt_turn.as_ref().unwrap().prompt_id,
                    expected_id
                )
            });
        } else {
            assert_eq!(
                command.delivery_receipt.as_ref().unwrap().prompt_id,
                expected_id
            );
            turn.record(|record| assert!(record.human_prompt_turn.is_none()));
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn human_prompt_write_ahead_keeps_its_owner_after_generic_human_capture() {
    const HUMAN_ID: &str = "receipted-human";
    for change in ["cancel", "nonce"] {
        let turn = OrdinaryRoot::new("receipted-human-post-capture");
        recovery::install_passive_original(&turn);
        turn.edit(|record| {
            let mut pending = selection_prompt(HUMAN_ID);
            pending.text = "Exactly this receipted Human content".into();
            queue_receipted_orchestrator_prompt_on_record(record, pending, Vec::new());
            // Retained Human input is fixed before its first durable save.
            // No User-to-Human conversion or delivery acknowledgement occurs.
            record.queued_prompts[0].source = QueuedPromptSource::Human;
            let content = queued_prompt_durability_content(&record.queued_prompts[0]);
            record
                .queued_prompt_durability_holds
                .get_mut(HUMAN_ID)
                .unwrap()
                .content = content;
            queue_prompt_on_record_with_source(
                record,
                selection_prompt("independent-human"),
                vec![],
                QueuedPromptSource::Human,
            );
            record.set_auto_dispatch_blocked(true);
        });
        let loaded = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let loaded_record = loaded
            .sessions
            .into_iter()
            .find(|record| record.session.id == turn.session)
            .unwrap();
        assert_eq!(
            loaded_record.queued_prompts[0].source,
            QueuedPromptSource::Human
        );
        assert_eq!(
            loaded_record.prompt_deliveries[HUMAN_ID].disposition,
            PromptDeliveryDisposition::Pending
        );
        let runtime = turn.record(|record| record.runtime.clone());
        {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session).unwrap();
            inner.sessions[index] = loaded_record;
            inner.sessions[index].runtime = runtime;
        }
        let choices = turn.record(|record| record.human_queue_choices.clone());
        let duties =
            serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations)
                .unwrap();
        let mut state = turn.state.clone();
        let session = turn.session.clone();
        let writer = ChoiceWriter::attach(&mut state);
        // Exercise the existing generic receipt Resume and its real callback.
        assert!(matches!(
            state.resume_local_session_queue(&session, None).unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        let ack = writer.write_ahead();
        let nonce = turn.record(|record| {
            let receipt = &record.prompt_deliveries[HUMAN_ID];
            assert!(!receipt.write_ahead_acknowledged);
            assert!(receipt.resume.is_some());
            assert!(receipt.turn_generation.is_none());
            receipt.write_ahead.clone()
        });
        turn.assert_no_prompt();
        let concurrent = state.clone();
        let concurrent_session = session.clone();
        let captured_nonce = nonce.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let _hook: TestHumanDrainHookOwner = install_test_human_drain_capture_hook(
            &state,
            &session,
            Box::new(move || {
                {
                    let inner = concurrent.inner.lock().unwrap();
                    let record =
                        &inner.sessions[inner.find_session_index(&concurrent_session).unwrap()];
                    let receipt = &record.prompt_deliveries[HUMAN_ID];
                    assert!(
                        receipt.write_ahead_acknowledged,
                        "the real SQLite WAL ACK must precede Human-owner capture"
                    );
                    assert_eq!(receipt.write_ahead, captured_nonce);
                    assert!(receipt.resume.is_some());
                    assert!(receipt.turn_generation.is_none());
                    assert_eq!(record.queued_prompts[0].pending_prompt.id, HUMAN_ID);
                    assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Human);
                    assert!(record.queued_prompts[0].promoted_message_index.is_none());
                }
                // The generic drain already captured its exact Human owner.
                // Mutate only now, before that owner's promotion recheck.
                if change == "cancel" {
                    concurrent
                        .cancel_queued_prompt(&concurrent_session, HUMAN_ID)
                        .unwrap();
                } else {
                    concurrent.return_unpromoted_write_ahead_to_pending(
                        &concurrent_session,
                        HUMAN_ID,
                        &captured_nonce,
                    );
                }
                done_tx
            }),
        );
        // ChoiceWriter checked the exact WAL in SQLite. Preserve the real
        // production callback; its existing completion hook bounds the race.
        ack.finish(Ok(()));
        phase_sync::receive(
            &done_rx,
            "captured Human drain returned without a successor handoff",
        );
        turn.assert_no_prompt();
        turn.record(|record| {
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(record.human_prompt_turn.is_none());
            assert_eq!(record.human_queue_choices, choices);
            if change == "cancel" {
                assert_eq!(record.queued_prompts.len(), 1);
                assert_eq!(
                    record.queued_prompts[0].pending_prompt.id,
                    "independent-human"
                );
            } else {
                assert_eq!(record.queued_prompts.len(), 2);
                let receipt = &record.prompt_deliveries[HUMAN_ID];
                assert_eq!(receipt.disposition, PromptDeliveryDisposition::Pending);
                assert_ne!(receipt.write_ahead, nonce);
                assert!(!receipt.write_ahead_acknowledged);
                assert!(receipt.resume.is_none());
            }
        });
        state.settle_prompt_delivery_write_ahead(&session, HUMAN_ID, &nonce, Ok(()));
        turn.assert_no_prompt();
        let selected = selection(&state, &session);
        let expected_id = selected.prompt.id.clone();
        let expected_text = selected.prompt.text.clone();
        let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
        assert_eq!(owner.prompt_id, expected_id);
        let fresh = state
            .resume_local_session_queue(&session, Some(owner))
            .unwrap();
        if change == "nonce" {
            assert!(matches!(
                fresh,
                Some(TurnDispatchDeliveryOutcome::Scheduled)
            ));
            let fresh_ack = writer.write_ahead();
            turn.record(|record| {
                let receipt = &record.prompt_deliveries[HUMAN_ID];
                assert_ne!(receipt.write_ahead, nonce);
                assert!(!receipt.write_ahead_acknowledged);
            });
            turn.assert_no_prompt();
            fresh_ack.finish(Ok(()));
        } else {
            assert!(matches!(
                fresh,
                Some(TurnDispatchDeliveryOutcome::Delivered)
            ));
        }
        let command = turn.prompt();
        assert!(command.prompt.contains(&expected_text));
        turn.record(|record| {
            assert_eq!(
                record.human_prompt_turn.as_ref().unwrap().prompt_id,
                expected_id
            )
        });
        if change == "nonce" {
            let receipt = command.delivery_receipt.as_ref().unwrap();
            assert_eq!(receipt.prompt_id, expected_id);
            assert_ne!(receipt.write_ahead, nonce);
        }
        turn.assert_no_prompt();
        recovery::assert_passive_original(&turn, &duties);
        turn.assert_no_control();
    }
}

#[test]
fn human_prompt_pending_receipt_uses_real_write_ahead_and_fences_late_writers() {
    let turn = receipted_pending_fixture();
    let mut state = turn.state.clone();
    let session = turn.session.clone();
    let original = turn.record(|record| record.queued_prompts[0].clone());
    let (old_nonce, choices, duties) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.prompt_deliveries["legacy-proven-pending"].disposition,
            PromptDeliveryDisposition::Pending
        );
        (
            record.prompt_deliveries["legacy-proven-pending"]
                .write_ahead
                .clone(),
            record.human_queue_choices.clone(),
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
        )
    };
    let writer = ChoiceWriter::attach(&mut state);
    state
        .resume_selected_human_prompt(&session, selection(&state, &session))
        .unwrap();
    let ack = writer.write_ahead();
    turn.assert_no_prompt();
    ack.finish(Ok(()));
    let command = turn.prompt();
    assert!(command.prompt.contains("Exactly this legacy content"));
    turn.assert_no_prompt();
    let receipt = command
        .delivery_receipt
        .as_ref()
        .expect("original receipt reaches runtime");
    assert_eq!(receipt.prompt_id, "legacy-proven-pending");
    assert_ne!(receipt.write_ahead, old_nonce);
    let token = turn.record(|record| {
        let promoted = record
            .session
            .messages
            .iter()
            .position(|message| message.id() == original.pending_prompt.id)
            .unwrap();
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == original.pending_prompt.id)
                .count(),
            1
        );
        let mut expected = original.clone();
        expected.engram_ordinary_handoff = true;
        expected.promoted_message_index = Some(global_message_index(record, promoted));
        assert_eq!(
            serde_json::to_value(&record.queued_prompts).unwrap(),
            json!([expected]),
            "ordinary Resume retains exactly the original history plus its promotion identity"
        );
        assert!(!ordinary_queue_entry_is_runnable(
            record,
            &record.queued_prompts[0]
        ));
        assert!(ordinary_queue_position(record).is_none());
        assert_eq!(
            record.prompt_deliveries["legacy-proven-pending"]
                .message_id
                .as_deref(),
            Some(original.pending_prompt.id.as_str())
        );
        assert_eq!(
            record.prompt_deliveries["legacy-proven-pending"].write_ahead,
            receipt.write_ahead
        );
        assert_eq!(
            record.prompt_deliveries["legacy-proven-pending"].turn_generation,
            Some(command.active_turn_generation)
        );
        assert_eq!(
            record.prompt_deliveries["legacy-proven-pending"]
                .entry
                .as_ref()
                .unwrap()
                .source,
            QueuedPromptSource::User
        );
        assert_eq!(record.human_queue_choices, choices);
        record.runtime.runtime_token().unwrap()
    });
    let stale = CodexPromptReceipt {
        prompt_id: "legacy-proven-pending".into(),
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
    state
        .finish_turn_ok_if_runtime_and_generation_match(
            &session,
            &token,
            command.active_turn_generation,
        )
        .unwrap();
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Idle));
    let retained = turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap());
    assert!(
        state
            .resume_local_session_queue(&session, None)
            .unwrap()
            .is_none()
    );
    turn.record(|record| {
        assert_eq!(
            record.active_turn_generation,
            command.active_turn_generation
        );
        assert_eq!(
            serde_json::to_value(&record.queued_prompts).unwrap(),
            retained
        );
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == original.pending_prompt.id)
                .count(),
            1
        );
        assert_eq!(record.human_queue_choices, choices);
    });
    turn.assert_no_prompt();
    recovery::assert_passive_original(&turn, &duties);
}

#[test]
fn human_prompt_handing_off_reload_keeps_history_and_ambiguity_without_replay() {
    let turn = receipted_pending_fixture();
    let mut state = turn.state.clone();
    let session = turn.session.clone();
    let choices = turn.record(|record| record.human_queue_choices.clone());
    let writer = ChoiceWriter::attach(&mut state);
    state
        .resume_selected_human_prompt(&session, selection(&state, &session))
        .unwrap();
    let ack = writer.write_ahead();
    let (nonce, attempt) = turn.record(|record| {
        let receipt = &record.prompt_deliveries["legacy-proven-pending"];
        (receipt.write_ahead.clone(), receipt.attempt)
    });
    let disk = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let raw: String = disk
        .query_row(
            "SELECT value_json FROM sessions WHERE id=?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    let saved: PersistedSessionRecord = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        saved.queued_prompts[0].pending_prompt.text,
        "Exactly this legacy content"
    );
    assert_eq!(saved.queued_prompts[0].source, QueuedPromptSource::User);
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
        let receipt = &loaded_record.prompt_deliveries["legacy-proven-pending"];
        assert_eq!(receipt.disposition, PromptDeliveryDisposition::Ambiguous);
        assert_eq!(receipt.attempt, attempt);
        assert_eq!(receipt.write_ahead, nonce);
        assert_eq!(loaded_record.human_queue_choices, choices);
        assert!(
            loaded_record
                .queued_prompts
                .iter()
                .all(|entry| entry.pending_prompt.id != "legacy-proven-pending"),
            "restart retires the ambiguous handoff instead of restoring it as runnable work"
        );
        inner.sessions[index] = loaded_record;
    }
    state.settle_prompt_delivery_write_ahead(&session, "legacy-proven-pending", &nonce, Ok(()));
    turn.assert_no_prompt();
    let callback_state = state.clone();
    let callback_session = session.clone();
    let callback_nonce = nonce.clone();
    let ack = (*ack).on_resolved(move |result| {
        callback_state.settle_prompt_delivery_write_ahead(
            &callback_session,
            "legacy-proven-pending",
            &callback_nonce,
            result,
        );
    });
    std::thread::spawn(move || ack.finish(Ok(())))
        .join()
        .unwrap();
    let mut fresh = prompt();
    fresh.text = "Fresh work after an ambiguous earlier attempt".into();
    let dispatch = match state.dispatch_human_turn(&session, fresh).unwrap() {
        DispatchTurnResult::Dispatched(d) | DispatchTurnResult::DispatchedAfterQueue(d) => d,
        DispatchTurnResult::Queued => {
            panic!("ambiguous old receipt cannot strand distinct Human input")
        }
    };
    assert_ne!(dispatch.human_prompt_id(), Some("legacy-proven-pending"));
    assert!(matches!(
        deliver_turn_dispatch(&state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        turn.prompt()
            .prompt
            .contains("Fresh work after an ambiguous earlier attempt")
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

pub(super) fn selection_prompt(id: &str) -> PendingPrompt {
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
