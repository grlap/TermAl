//! History reads must finish on one transcript version while mutations and
//! persistence continue, including rollback of the nonresident prefix.
//!
//! Correctness: the state mutex prevents a body mutation during capture. The
//! persistence worker only writes versions already present in memory, and its
//! watermark guard trims a record only after that version is durable. Rollback
//! retains its replacement at start_index=0 until that guard permits eviction.
//! Therefore resident bodies and the SQLite prefix pinned under the same lock
//! describe one state. WAL lets the pinned reader coexist with later writes.

use super::*;

fn messages(version: &str) -> Vec<Message> {
    (0..130)
        .map(|index| Message::Text {
            id: format!("history-{index}"),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: format!("{version}-{index}"),
            expanded_text: None,
            source: None,
            attachments: Vec::new(),
        })
        .collect()
}

fn fixture() -> (AppState, String) {
    let state = test_app_state();
    let mut inner = state.inner.lock().unwrap();
    let id = inner
        .create_session(Agent::Codex, None, "/tmp".to_owned(), None, None)
        .session
        .id;
    let index = inner.find_session_index(&id).unwrap();
    replace_session_messages_on_record(
        inner.session_mut_by_index(index).unwrap(),
        messages("old"),
        None,
    );
    inner.finish_body_sequence_commits();
    persist_state(state.persistence_path.as_ref(), &inner).unwrap();
    let watermark = inner.last_mutation_stamp;
    inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&id));
    assert_eq!(inner.sessions[index].message_start_index, 66);
    drop(inner);
    (state, id)
}

fn selector(start: usize, limit: usize) -> SessionHistorySelector<'static> {
    SessionHistorySelector {
        before: None,
        after: None,
        around: None,
        start: Some(start),
        from_start: false,
        limit,
    }
}

fn assert_version(page: &SessionHistoryResponse, version: &str) {
    for (offset, message) in page.messages.iter().enumerate() {
        let Message::Text { text, .. } = message else {
            panic!("expected text")
        };
        assert_eq!(
            text,
            &format!("{version}-{}", page.message_start_index + offset)
        );
    }
}

#[test]
fn history_snapshot_survives_rollback_and_persist_between_capture_and_page_load() {
    let (state, id) = fixture();
    let query = selector(50, 40); // Both the SQLite prefix and resident tail.
    let captured = state.capture_local_session_history(&id, query).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            messages("new"),
            None,
        );
        inner.finish_body_sequence_commits();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        let watermark = inner.last_mutation_stamp;
        inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&id));
    }
    let page = captured
        .page(&id, query, state.server_instance_id.clone())
        .unwrap();
    assert_eq!(page.messages.len(), 40);
    assert_version(&page, "old");
    let next = state
        .get_session_history(&id, None, None, None, Some(50), false, 40)
        .unwrap();
    assert_version(&next, "new");
    assert_eq!(page.body_seq, Some(1));
    assert_eq!(next.body_seq, Some(2));
}

#[test]
fn history_rollback_keeps_replacement_resident_while_persistence_is_delayed() {
    let (state, id) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let old_watermark = inner.last_mutation_stamp;
        let index = inner.find_session_index(&id).unwrap();
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            messages("new"),
            None,
        );
        // Simulate an older asynchronous persistence completion after rollback.
        inner.trim_persisted_session_tails(old_watermark, std::slice::from_ref(&id));
        assert_eq!(inner.sessions[index].message_start_index, 0);
    }
    let query = selector(0, 40);
    let captured = state.capture_local_session_history(&id, query).unwrap();
    assert!(
        captured.persistence.is_none(),
        "all new bodies remain resident until durable"
    );
    assert_version(
        &captured
            .page(&id, query, state.server_instance_id.clone())
            .unwrap(),
        "new",
    );
    let old_rows =
        load_persisted_message_range(state.persistence_path.as_ref(), &id, 0, 1).unwrap();
    assert!(matches!(&old_rows[0].1, Message::Text { text, .. } if text == "old-0"));
}

#[test]
fn history_read_makes_progress_with_tail_updates_after_capture() {
    let (state, id) = fixture();
    for _ in 0..3 {
        let query = selector(0, 40);
        let captured = state.capture_local_session_history(&id, query).unwrap();
        state
            .append_text_delta(&id, "history-129", " streaming")
            .unwrap();
        let page = captured
            .page(&id, query, state.server_instance_id.clone())
            .unwrap();
        assert_eq!(
            page.messages.len(),
            40,
            "a changing tail cannot starve an old page"
        );
        assert_version(&page, "old");
    }
}

#[test]
fn history_contains_final_update_immediately_after_persisted_eviction() {
    let (mut state, id) = fixture();
    // This fixture has no worker. Retaining its receiver delays persistence
    // without stopping or replacing a production persistence worker.
    let (persist_tx, _persist_rx) = mpsc::channel();
    state.persist_tx = persist_tx;
    let before_update = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            messages("new"),
            None,
        );
        inner.last_mutation_stamp
    };
    state
        .replace_text_message(&id, "history-0", "final before eviction")
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.trim_persisted_session_tails(before_update, std::slice::from_ref(&id));
        assert_eq!(inner.sessions[index].message_start_index, 0);
        inner.finish_body_sequence_commits();
        persist_state(state.persistence_path.as_ref(), &inner).unwrap();
        let watermark = inner.last_mutation_stamp;
        inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&id));
        assert_eq!(inner.sessions[index].message_start_index, 66);
    }
    let page = state
        .get_session_history(&id, None, None, None, Some(0), false, 1)
        .unwrap();
    assert!(
        matches!(&page.messages[0], Message::Text { text, .. } if text == "final before eviction")
    );
}

#[test]
fn history_does_not_resurrect_removed_cursor_from_unpersisted_rollback() {
    let (state, id) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let mut replacement = messages("new");
        replacement.truncate(10);
        replace_session_messages_on_record(
            inner.session_mut_by_index(index).unwrap(),
            replacement,
            None,
        );
    }
    assert!(
        state
            .get_session_history(&id, Some("history-20"), None, None, None, false, 5)
            .is_err()
    );
    let page = state
        .get_session_history(&id, None, None, None, Some(0), false, 5)
        .unwrap();
    assert_version(&page, "new");
}
