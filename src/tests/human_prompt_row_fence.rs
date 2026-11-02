//! Human sends acknowledge a committed own-row cut, independent of later
//! queue edits. Reuses the real SQLite fence writer and runtime fixtures.
use super::*;

fn row_target(inner: &StateInner, id: &str) -> PersistFenceTarget {
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    PersistFenceTarget::HumanPromptRow {
        session_id: id.to_owned(), minimum: PersistSessionWriteStamp::capture(record),
    }
}

fn accept_human_fence(fixture: &mut FenceFixture) -> PersistFenceWaiter {
    loop {
        let request = phase_sync::receive(&fixture.rx, "Human own-row fence arrives off StateInner");
        let waiter = match &request {
            PersistRequest::Fence(fence) if matches!(&fence.target, PersistFenceTarget::HumanPromptRow { .. }) =>
                Some(PersistFenceWaiter { completion: fence.completion.clone() }),
            _ => None,
        };
        fixture.batch.accept(request);
        if let Some(waiter) = waiter { return waiter; }
    }
}

fn human_request(text: &str) -> SendMessageRequest {
    SendMessageRequest { text: text.into(), expanded_text: None, attachments: Vec::new(),
        source_session_id: None, source_mailbox: None }
}

fn inspect_state_before_ack(
    state: &AppState,
    pending: &[&PersistFenceWaiter],
    phase: &str,
    inspect: impl FnOnce(&mut StateInner) + Send + 'static,
) {
    let state = state.clone();
    let pending = pending.iter().map(|waiter| waiter.completion.clone()).collect::<Vec<_>>();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let helper = std::thread::spawn(move || {
        // Another sender may briefly hold the lock after its intake ACK.
        // Acquire it while the selected fence remains unresolved instead of
        // requiring every concurrent sender to be quiescent at one instant.
        let mut inner = state.inner.lock().unwrap();
        for completion in pending {
            assert_eq!(completion.poll_at(std::time::Instant::now()), None,
                "StateInner must be available before the fence resolves");
        }
        inspect(&mut inner);
        acquired_tx.send(()).unwrap();
    });
    phase_sync::receive(&acquired_rx, phase);
    helper.join().unwrap();
}

#[test]
fn concurrent_human_sends_ack_latest_rows_and_deliver_each_prompt_once() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let mut state = fixture.app_state();
    state.inner = fixture.inner.clone();
    let project = create_test_project(&state, fixture.root.path(), "Human row cuts");
    let session = create_test_project_session(&state, Agent::Codex, &project, fixture.root.path());
    {
        let mut inner = state.inner.lock().unwrap();
        assert!(human_prompt_persistence_fence(&inner, &session).unwrap().is_none(),
            "plain sessions retain master's fence-free path");
        inner.projects.iter_mut().find(|entry| entry.id == project).unwrap().engram =
            Some(EngramProjectSettings::default());
        assert!(human_prompt_persistence_fence(&inner, &session).unwrap().is_some(),
            "scope depends on the Engram project, not enabled observations or a grant");
    }
    let (runtime, input, _process) = test_shared_codex_runtime("concurrent-human-row-cuts");
    install_single_wire_codex_fixture(&state, Some(runtime));
    state.persist_tx = fixture.tx.clone();
    let start = |text: &'static str| {
        let state = state.clone();
        let session = session.clone();
        std::thread::spawn(move || state.dispatch_human_turn(&session, human_request(text)))
    };
    let first = start("first concurrent Human");
    let intake_first = accept_human_fence(&mut fixture);
    let second = start("second concurrent Human");
    let intake_second = accept_human_fence(&mut fixture);
    let inspected_session = session.clone();
    inspect_state_before_ack(&state, &[&intake_first, &intake_second],
        "both intake waits release StateInner before acknowledgement", move |inner| {
        let record = &inner.sessions[inner.find_session_index(&inspected_session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 2);
        assert_ne!(record.session.status, SessionStatus::Error);
    });
    assert_eq!(poll(&intake_first), None);
    assert_eq!(poll(&intake_second), None);
    // The writer never saves the first sender's old queue image. It commits
    // only the newer combined row, proving both captured cuts at once.
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&intake_first), Some(Ok(())));
    assert_eq!(poll(&intake_second), Some(Ok(())));
    let promotion_first = accept_human_fence(&mut fixture);
    let inspected_session = session.clone();
    let inspected_state = state.clone();
    inspect_state_before_ack(&state, &[&promotion_first],
        "promotion releases StateInner before acknowledgement", move |inner| {
        let record = inner.session_mut(&inspected_session).unwrap();
        let pending = PendingPrompt { id: "later-mailbox".into(), timestamp: stamp_now(),
            text: "unrelated mailbox work".into(), expanded_text: None, source: None,
            attachments: Vec::new(), engram_interrupted: false, is_engram_retained: false };
        queue_prompt_on_record_with_source(record, pending, Vec::new(), QueuedPromptSource::Mailbox);
        inspected_state.commit_session_locked(inner, &inspected_session).unwrap();
    });
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&promotion_first), Some(Ok(())), "the newer mailbox row ACKs promotion");
    let mut dispatched = 0;
    for result in [first.join().unwrap(), second.join().unwrap()] {
        match result.unwrap() {
            DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                deliver_turn_dispatch(&state, dispatch).into_public_result().unwrap();
                dispatched += 1;
            }
            DispatchTurnResult::Queued => {}
        }
    }
    assert_eq!(dispatched, 1, "only the FIFO head owns the first runtime turn");
    assert!(matches!(phase_sync::receive(&input, "first Human provider delivery"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("first concurrent Human")));
    assert!(input.try_recv().is_err());
    let token = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Active);
        record.runtime.runtime_token().unwrap()
    };
    let completion_state = state.clone();
    let completion_session = session.clone();
    let completed = std::thread::spawn(move ||
        completion_state.finish_turn_ok_if_runtime_matches(&completion_session, &token));
    let promotion_second = accept_human_fence(&mut fixture);
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&promotion_second), Some(Ok(())));
    completed.join().unwrap().unwrap();
    assert!(matches!(phase_sync::receive(&input, "second Human provider delivery"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains("second concurrent Human")));
    assert!(input.try_recv().is_err(), "each Human reaches the provider once");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, "later-mailbox");
}

#[test]
fn human_row_proof_rejects_old_stamps_and_siblings_but_accepts_later_cancellation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let (id, sibling) = {
        let mut inner = fixture.inner.lock().unwrap();
        let cwd = fixture.root.path().to_string_lossy().into_owned();
        (inner.create_session(Agent::Codex, None, cwd.clone(), None, None).session.id,
            inner.create_session(Agent::Codex, None, cwd, None, None).session.id)
    };
    let old = fixture.collect();
    let target = {
        let mut inner = fixture.inner.lock().unwrap();
        queue_ordinary_prompt(inner.session_mut(&id).unwrap(), "later-accepted-Human");
        row_target(&inner, &id)
    };
    let waiter = fixture.enqueue(target.clone());
    fixture.receive(false);
    fixture.write(&old).unwrap();
    assert_eq!(poll(&waiter), None, "an older own row cannot ACK a later prompt");
    {
        let mut inner = fixture.inner.lock().unwrap();
        queue_ordinary_prompt(inner.session_mut(&sibling).unwrap(), "newer-sibling");
    }
    let mut sibling_delta = fixture.collect();
    sibling_delta.changed_sessions.retain(|record| record.session.id == sibling);
    fixture.write(&sibling_delta).unwrap();
    assert_eq!(poll(&waiter), None, "a newer global watermark or sibling is not own-row proof");
    {
        let mut inner = fixture.inner.lock().unwrap();
        let record = inner.session_mut(&id).unwrap();
        record.queued_prompts.clear();
        sync_pending_prompts(record);
    }
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())), "the later saved cancellation supersedes the intake cut");
    let late = fixture.enqueue(target);
    fixture.receive(false);
    let mut empty = fixture.collect();
    empty.changed_sessions.clear();
    fixture.write(&empty).unwrap();
    assert_eq!(poll(&late), Some(Ok(())), "a fence accepted after the write uses the same cached proof");
    let encoded: String = fixture.cache.connection.as_ref().unwrap().query_row(
        "SELECT value_json FROM sessions WHERE id=?1", [&id], |row| row.get(0)).unwrap();
    let stored: PersistedSessionRecord = serde_json::from_str(&encoded).unwrap();
    assert!(stored.queued_prompts.is_empty());
    assert!(stored.source_stamp.is_none(), "runtime proof is never restored from SQLite");
    assert!(!encoded.contains("sourceStamp") && !encoded.contains("persistIncarnation"));
}

#[test]
fn human_row_proof_does_not_cross_a_real_reload_incarnation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let id = fixture.inner.lock().unwrap().create_session(Agent::Codex, None,
        fixture.root.path().to_string_lossy().into_owned(), None, None).session.id;
    fixture.write(&fixture.collect()).unwrap();
    let restored = load_state(&fixture.root.path().join("termal.sqlite")).unwrap().unwrap();
    let restored_record = &restored.sessions[restored.find_session_index(&id).unwrap()];
    let old_incarnation = fixture.cache.transcripts[&id].row_stamp.as_ref().unwrap().clone();
    let minimum = PersistSessionWriteStamp::capture(restored_record);
    assert!(!old_incarnation.covers(&minimum), "equal or newer numbers cannot cross reload identity");
    let waiter = fixture.enqueue(PersistFenceTarget::HumanPromptRow {
        session_id: id.clone(), minimum,
    });
    fixture.receive(false);
    let mut empty = fixture.collect();
    empty.changed_sessions.clear();
    fixture.write(&empty).unwrap();
    assert_eq!(poll(&waiter), None);
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner.session_mut(&id).unwrap().persist_incarnation = restored_record.persist_incarnation.clone();
    }
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())), "only a committed row from the new incarnation ACKs");
}

#[test]
fn human_row_proof_cannot_ack_a_quarantined_own_row_from_a_saved_sibling() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (mut fixture, state, bad, good) = partial_synchronous_fixture(false);
    let (bad_target, good_target) = {
        let mut inner = state.inner.lock().unwrap();
        queue_ordinary_prompt(inner.session_mut(&bad).unwrap(), "unwritten-Human");
        queue_ordinary_prompt(inner.session_mut(&good).unwrap(), "written-sibling");
        (row_target(&inner, &bad), row_target(&inner, &good))
    };
    let bad_waiter = fixture.enqueue(bad_target);
    let good_waiter = fixture.enqueue(good_target);
    fixture.receive(false);
    fixture.batch.drain(&fixture.rx, false);
    let saved = fixture.write(&fixture.collect()).unwrap();
    assert!(!saved.contains(&bad) && saved.contains(&good));
    assert_eq!(poll(&good_waiter), Some(Ok(())));
    assert_eq!(poll(&bad_waiter), None, "a rejected row retains its old cached stamp");
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&bad).unwrap();
        record.session.messages[0] = message("body-2");
        record.message_positions = build_message_positions(&record.session.messages);
    }
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&bad_waiter), Some(Ok(())), "a later corrected own-row commit can ACK");
}

#[test]
fn human_row_proof_does_not_ack_a_rolled_back_transaction() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let id = fixture.inner.lock().unwrap().create_session(Agent::Codex, None,
        fixture.root.path().to_string_lossy().into_owned(), None, None).session.id;
    fixture.write(&fixture.collect()).unwrap();
    let target = {
        let mut inner = fixture.inner.lock().unwrap();
        queue_ordinary_prompt(inner.session_mut(&id).unwrap(), "must-be-committed");
        row_target(&inner, &id)
    };
    let minimum = match &target {
        PersistFenceTarget::HumanPromptRow { minimum, .. } => minimum.clone(),
        _ => unreachable!(),
    };
    let waiter = fixture.enqueue(target);
    fixture.receive(false);
    fixture.cache.connection.as_ref().unwrap().execute_batch(
        "CREATE TRIGGER reject_human_row BEFORE INSERT ON sessions
         BEGIN SELECT RAISE(ABORT, 'scripted Human row write failure'); END;"
    ).unwrap();
    assert!(fixture.write(&fixture.collect()).is_err());
    assert!(matches!(poll(&waiter), Some(Err(PersistFenceError::WriteFailed(error)))
        if error.contains("scripted Human row write failure")));
    let row = &fixture.cache.transcripts[&id];
    assert!(!row.transcript_valid && row.identities.is_empty());
    assert!(!row.row_stamp.as_ref().unwrap().covers(&minimum),
        "a rollback never advances the last verified own-row receipt");
}

#[test]
fn late_human_fence_keeps_its_verified_cut_across_body_cache_invalidation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let id = fixture.inner.lock().unwrap().create_session(Agent::Codex, None,
        fixture.root.path().to_string_lossy().into_owned(), None, None).session.id;
    let target = {
        let mut inner = fixture.inner.lock().unwrap();
        let record = inner.session_mut(&id).unwrap();
        record.session.messages.push(message("cached-human-body"));
        record.session.message_count = 1;
        record.message_positions = build_message_positions(&record.session.messages);
        row_target(&inner, &id)
    };
    fixture.write(&fixture.collect()).unwrap();
    assert!(fixture.cache.transcripts[&id].transcript_valid);
    assert_eq!(fixture.cache.transcripts[&id].identities.len(), 1);
    let predecessor = fixture.cache.transcript_data_version.unwrap();
    let path = fixture.root.path().join("termal.sqlite");
    let other = rusqlite::Connection::open(&path).unwrap();
    let before: String = other.query_row("SELECT value_json FROM sessions WHERE id=?1",
        [&id], |row| row.get(0)).unwrap();
    assert_eq!(other.execute("UPDATE sessions SET value_json=value_json || ' ' WHERE id=?1",
        [&id]).unwrap(), 1);
    let after: String = other.query_row("SELECT value_json FROM sessions WHERE id=?1",
        [&id], |row| row.get(0)).unwrap();
    assert_ne!(before, after, "the foreign commit changes stored bytes");
    assert_eq!(serde_json::from_str::<Value>(&before).unwrap(),
        serde_json::from_str::<Value>(&after).unwrap(), "the row remains the same valid JSON value");
    drop(other);
    let changed_version: i64 = fixture.cache.connection.as_ref().unwrap()
        .query_row("PRAGMA data_version", [], |row| row.get(0)).unwrap();
    assert_ne!(changed_version, predecessor, "the writer must observe an actual external commit");
    let late = fixture.enqueue(target.clone());
    fixture.receive(false);
    assert_eq!(poll(&late), None);
    let mut empty = fixture.collect();
    empty.changed_sessions.clear();
    assert!(fixture.write(&empty).unwrap().is_empty(), "the late fence does not rewrite its row");
    assert_eq!(poll(&late), Some(Ok(())), "a foreign rewrite cannot erase an already verified cut");
    assert!(!fixture.cache.transcripts[&id].transcript_valid,
        "the old sparse body predecessor is still invalidated");
    assert!(fixture.cache.transcripts[&id].identities.is_empty());
    let retained: String = fixture.cache.connection.as_ref().unwrap()
        .query_row("SELECT value_json FROM sessions WHERE id=?1", [&id], |row| row.get(0)).unwrap();
    assert_eq!(retained, after, "acknowledgement preserves the foreign committed bytes");
    fixture.cache.invalidate();
    let reopened = fixture.enqueue(target);
    fixture.receive(false);
    let mut empty = fixture.collect();
    empty.changed_sessions.clear();
    fixture.write(&empty).unwrap();
    assert_eq!(poll(&reopened), Some(Ok(())), "same-store reconnect retains the historical row receipt");
    assert!(!fixture.cache.transcripts[&id].transcript_valid);
}

#[test]
fn human_row_receipts_are_discarded_by_path_switch_and_tombstone() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for remove in [false, true] {
        let other = (!remove).then(FenceFixture::new);
        let mut fixture = FenceFixture::new();
        let id = fixture.inner.lock().unwrap().create_session(Agent::Codex, None,
            fixture.root.path().to_string_lossy().into_owned(), None, None).session.id;
        let target = {
            let inner = fixture.inner.lock().unwrap();
            row_target(&inner, &id)
        };
        let initial = fixture.collect();
        fixture.write(&initial).unwrap();
        let watermark = if remove {
            {
                let mut inner = fixture.inner.lock().unwrap();
                let index = inner.find_session_index(&id).unwrap();
                assert!(inner.remove_session_at(index).is_some());
            }
            let tombstone = collect_persist_delta_from_shared_state(&fixture.inner, initial.watermark);
            assert!(tombstone.changed_sessions.is_empty());
            assert!(tombstone.removed_session_ids.contains(&id));
            fixture.write(&tombstone).unwrap();
            let count: i64 = fixture.cache.connection.as_ref().unwrap().query_row(
                "SELECT count(*) FROM sessions WHERE id=?1", [&id], |row| row.get(0)).unwrap();
            assert_eq!(count, 0);
            tombstone.watermark
        } else {
            // A second fixture supplies a separately startup-validated store.
            fixture.cache.connection_for(&other.as_ref().unwrap().root.path().join("termal.sqlite")).unwrap();
            initial.watermark
        };
        assert!(!fixture.cache.transcripts.contains_key(&id));
        let waiter = fixture.enqueue(target);
        fixture.receive(false);
        // Capture the source's current durable predecessor after deletion,
        // without resubmitting the removed row or its tombstone.
        let empty = collect_persist_delta_from_shared_state(&fixture.inner, watermark);
        assert!(empty.changed_sessions.is_empty());
        assert!(empty.removed_session_ids.is_empty());
        fixture.write(&empty).unwrap();
        assert_eq!(poll(&waiter), None, "an old store or deleted row cannot ACK a late fence");
        fixture.batch.fail(PersistFenceError::Shutdown);
    }
}
