//! Exact response attribution and observer visibility before writer COMMIT.
//! Uses the parent's disposable producer, real SQLite writer and root path.

use super::*;

struct CausalWriterRelease(mpsc::Sender<()>);

impl Drop for CausalWriterRelease {
    fn drop(&mut self) {
        // Release before the scoped join even when projection/assertion
        // unwinds. A disconnected receiver must not cause another panic.
        let _ = self.0.send(());
    }
}

fn visible_causes(fixture: &CausalRoot) -> Vec<Value> {
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    record.session.messages.iter().filter_map(|message| {
        let wire = serde_json::to_value(message).unwrap();
        wire.get("causalFailure").cloned()
    }).collect()
}

fn causal_writer_publication_boundary(fail: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    let (token, original_head, generation) = fixture.start_and_queue();
    let before = fixture.durable_cards();
    assert!(before.iter().all(|card| card.get("causalFailure").is_none()));
    let mut events = fixture.state.subscribe_delta_events();
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.writer_gate.lock().unwrap() = Some(CausalWriterGate {
        arrived: arrived_tx, release: release_rx, any_tick: false,
    });
    fixture.fail_writes.store(fail, std::sync::atomic::Ordering::Release);
    let (before_commit, emitted_before_commit, projections_hidden, persistence_normalized) = std::thread::scope(|scope| {
        let release = CausalWriterRelease(release_tx);
        let state = fixture.state.clone();
        let session = fixture.session.clone();
        let worker = std::thread::Builder::new().stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                state.checkpoint_engram_turn_off_lock(
                    &session, EngramCheckpointPurpose::TurnTerminal, Some(&token), None,
                    EngramNextIntent::Continue, Some(EngramExecutionOutcome::Succeeded), None,
                );
            }).unwrap();
        let arrived = arrived_rx.recv_timeout(DEADLOCK_GUARD);
        // Always release before asserting or joining, including a failed
        // diagnostic. No writer survives the scope or temporary store owner.
        let visible = visible_causes(&fixture);
        let (projections_hidden, persistence_normalized) = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            let session = AppState::wire_session_from_record(&fixture.state.server_instance_id, record);
            let tail = AppState::wire_session_tail_from_record(
                &fixture.state.server_instance_id, record, 100, true,
            );
            let hidden = [serde_json::to_value(session).unwrap(), serde_json::to_value(tail).unwrap()]
                .iter().all(|wire| wire["messages"].as_array().unwrap().iter()
                    .all(|message| message.get("causalFailure").is_none()));
            let persisted = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
            let normalized = !persisted.to_string().contains("publicationPending")
                && persisted["session"]["messages"].as_array().unwrap().iter()
                    .any(|message| message.get("causalFailure").is_some());
            (hidden, normalized)
        };
        let history = fixture.state.get_session_history(
            &fixture.session, None, None, None, None, true, SESSION_HISTORY_PAGE_MAX_MESSAGES,
        ).unwrap();
        let history_hidden = serde_json::to_value(history).unwrap()["messages"]
            .as_array().unwrap().iter().all(|message| message.get("causalFailure").is_none());
        let emitted = std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| event.contains("causalFailure"));
        drop(release);
        worker.join().unwrap();
        arrived.expect("writer must reach the causal record before COMMIT");
        (visible, emitted, projections_hidden && history_hidden, persistence_normalized)
    });
    // Publication completes from the writer's ACK, not in the caller; join it
    // deterministically when the write succeeds. A failed ACK stays hidden.
    if !fail {
        fixture.await_causal_publication();
    }
    let after = visible_causes(&fixture);
    let durable_after = if fail {
        // Observe the actual writer error before inspecting its old rows.
        loop {
            if fixture.committed.recv_timeout(DEADLOCK_GUARD).unwrap().is_err() { break; }
        }
        let stored = load_persisted_message_range(
            &fixture.state.persistence_path, &fixture.session, 0, 100,
        ).unwrap();
        stored.into_iter().filter_map(|(_, message)| {
            let wire = serde_json::to_value(message).unwrap();
            wire.get("causalFailure").cloned()
        }).collect::<Vec<_>>()
    } else {
        fixture.durable_cards().into_iter()
            .filter_map(|card| card.get("causalFailure").cloned()).collect()
    };
    // Restore the fixture connection posture solely for orderly teardown.
    fixture.fail_writes.store(false, std::sync::atomic::Ordering::Release);
    assert!(before_commit.is_empty(), "uncommitted cause reached transcript hydration");
    assert!(!emitted_before_commit, "uncommitted cause reached the normal message delta");
    assert!(projections_hidden, "uncommitted cause reached session/tail serialization");
    assert!(persistence_normalized, "ephemeral visibility must not remove the durable cause");
    if fail {
        assert!(after.is_empty(), "failed save exposed causal evidence");
        assert!(durable_after.is_empty(), "failed transaction must leave old rows intact");
        // A failed causal ACK is local to its optional details, not a
        // transcript barrier for later messages or generic card visibility.
        let mut later_events = fixture.state.subscribe_delta_events();
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let id = inner.next_message_id();
        let record = inner.session_mut_by_index(index).unwrap();
        let message_index = push_message_on_record(record, Message::Text {
            id, timestamp: stamp_now(), author: Author::Assistant,
            text: "Later message remains visible".to_owned(), attachments: vec![],
            expanded_text: None, source: None,
        });
        let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
        let revision = fixture.state.commit_persisted_delta_locked(&mut inner).unwrap();
        fixture.state.publish_message_created_delta_parts(&inner, revision, creates);
        let wire = serde_json::to_value(&inner.sessions[index].session).unwrap();
        assert!(wire["messages"].as_array().unwrap().last().unwrap()["text"]
            .as_str().unwrap().contains("Later message"));
        assert!(wire["messages"].as_array().unwrap().iter()
            .all(|message| message.get("causalFailure").is_none()));
        drop(inner);
        assert!(std::iter::from_fn(|| later_events.try_recv().ok())
            .any(|event| event.contains("Later message remains visible")));
    } else {
        assert_eq!(after, durable_after, "visible evidence must match COMMIT readback");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0]["operation"], "turn_checkpoint");
    }
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert_eq!(serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap(), original_head);
    assert_eq!(record.engram.dispatch_generation, generation);
    assert!(fixture.receiver.try_recv().is_err(), "diagnostic publication cannot deliver a provider prompt");
}

#[test]
fn root_causal_publication_waits_for_stalled_writer_commit() {
    causal_writer_publication_boundary(false);
}

#[test]
fn root_causal_publication_withholds_failed_writer_record() {
    causal_writer_publication_boundary(true);
}

// Public causes, read under StateInner without panicking there.
fn published_causes(fixture: &CausalRoot) -> Vec<Value> {
    let causes = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.find_session_index(&fixture.session).map(|index| {
            inner.sessions[index]
                .session
                .messages
                .iter()
                .map(serde_json::to_value)
                .collect::<serde_json::Result<Vec<_>>>()
        })
    };
    causes
        .expect("the fixture session exists")
        .unwrap()
        .into_iter()
        .filter_map(|wire| wire.get("causalFailure").cloned())
        .collect()
}

// A stalled writer must not hold the lifecycle. The writer is held at its
// first causal record; the caller must return while that cause is still
// unpublished, and the cause publishes once the writer acknowledges it.
fn caller_returns_before_publication(fault: (&'static str, usize)) -> Vec<Value> {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(fault));
    let (token, _, _) = fixture.start_and_queue();
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.writer_gate.lock().unwrap() = Some(CausalWriterGate {
        arrived: arrived_tx,
        release: release_rx,
        any_tick: false,
    });
    let (returned, arrived, hidden_while_held) = std::thread::scope(|scope| {
        let release = CausalWriterRelease(release_tx);
        let (done_tx, done_rx) = mpsc::channel();
        let state = fixture.state.clone();
        let session = fixture.session.clone();
        let worker = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                let _ = state.finish_turn_ok_if_runtime_matches(&session, &token);
                let _ = done_tx.send(());
            })
            .unwrap();
        let returned = done_rx.recv_timeout(DEADLOCK_GUARD).is_ok();
        // The writer reaches the record on its own and is held there, so no
        // acknowledgement or publication can have happened yet.
        let arrived = arrived_rx.recv_timeout(DEADLOCK_GUARD).is_ok();
        let hidden = published_causes(&fixture).is_empty();
        drop(release);
        let _ = worker.join();
        (returned, arrived, hidden)
    });
    assert!(returned, "the caller waited on its optional causal publication");
    assert!(arrived, "the writer reached the causal record");
    assert!(hidden_while_held, "a cause was public before its writer ACK");
    fixture.await_causal_publication();
    let published = published_causes(&fixture);
    assert!(!published.is_empty(), "the cause publishes after the ACK");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    published
}

#[test]
fn root_causal_checkpoint_record_does_not_wait_on_publication() {
    let published = caller_returns_before_publication(("turn_checkpoint", 1));
    assert!(published.iter().any(|cause| cause["operation"] == "turn_checkpoint"));
}

#[test]
fn root_causal_dispatch_record_and_park_do_not_wait_on_publication() {
    let published = caller_returns_before_publication(("turn_evaluate", 2));
    assert!(published.iter().any(|cause| cause["operation"] == "turn_evaluate"));
}

// Kill removes the session after its teardown checkpoint, so the writer is
// held before the call: the failed checkpoint's causal card can then not be
// acknowledged while kill runs, and kill must still return.
#[test]
fn root_causal_kill_session_does_not_wait_on_publication() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    let _ = fixture.start_and_queue();
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *fixture.writer_gate.lock().unwrap() = Some(CausalWriterGate {
        arrived: arrived_tx,
        release: release_rx,
        any_tick: true,
    });
    let _ = fixture.state.persist_tx.send(PersistRequest::Delta);
    let (held, returned, checkpointed) = std::thread::scope(|scope| {
        let release = CausalWriterRelease(release_tx);
        let held = arrived_rx.recv_timeout(DEADLOCK_GUARD).is_ok();
        let (done_tx, done_rx) = mpsc::channel();
        let state = fixture.state.clone();
        let session = fixture.session.clone();
        let worker = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                let _ = state.kill_session(&session);
                let _ = done_tx.send(());
            })
            .unwrap();
        let returned = done_rx.recv_timeout(DEADLOCK_GUARD).is_ok();
        let checkpointed = fixture.transport.lost_request.lock().unwrap().is_some();
        drop(release);
        let _ = worker.join();
        (held, returned, checkpointed)
    });
    assert!(held, "the writer was held before kill");
    assert!(checkpointed, "kill sent its failing teardown checkpoint");
    assert!(returned, "kill waited on its optional causal publication");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
}

fn queued_binding_validation(mode: CausalFaultMode) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("session_bind", 2)));
    *fixture.transport.mode.lock().unwrap() = mode;
    let (token, original_head, _) = fixture.start_and_queue();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.rebind_required = true;
    }
    let _completion = fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = fixture.transport.lost_request.lock().unwrap().clone()
        .expect("queued binding validation fault must be reached");
    assert_eq!(request["operation"], "session_bind");
    let cards = fixture.durable_cards();
    let cause = &cards.last().unwrap()["causalFailure"];
    assert_eq!(cause["operation"], "session_bind");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["remoteApplication"], "unknown");
    assert!(fixture.receiver.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert_eq!(serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap(), original_head);
}

#[test]
fn root_causal_malformed_bind_reply_keeps_exact_request() {
    queued_binding_validation(CausalFaultMode::Malformed);
}

#[test]
fn root_causal_missing_binding_status_keeps_exact_request() {
    queued_binding_validation(CausalFaultMode::MissingBindingStatus);
}

#[test]
fn root_causal_missing_begin_receipt_keeps_exact_request() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_begin", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::MissingBeginReceipt;
    let (token, _, _) = fixture.start_and_queue();
    fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token).unwrap();
    let request = fixture.transport.lost_request.lock().unwrap().clone().unwrap();
    let cards = fixture.durable_cards();
    let cause = &cards.last().unwrap()["causalFailure"];
    assert_eq!(cause["operation"], "turn_begin");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["remoteApplication"], "unknown");
    assert!(fixture.receiver.try_recv().is_err());
}
