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
fn history_capture_shares_resident_bodies_and_clones_only_the_selected_page() {
    let (state, id) = fixture();
    let query = selector(90, 3);
    let (captured, capture_clones) = MessageSequence::observe_shared_body_clones(|| {
        state.capture_local_session_history(&id, query).unwrap()
    });
    assert_eq!(capture_clones, 0);
    let expected;
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(
            captured
                .messages
                .shares_body(0, &record.session.messages, 0)
        );
        assert!(
            captured
                .messages
                .shares_body(63, &record.session.messages, 63)
        );
        expected = record
            .session
            .messages
            .iter()
            .skip(24)
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
    }
    let (page, cloned_bodies) = MessageSequence::observe_shared_body_clones(|| {
        captured
            .page(&id, query, state.server_instance_id.clone())
            .unwrap()
    });
    assert_eq!(cloned_bodies, 3);
    assert_eq!(page.message_start_index, 90);
    assert_eq!(page.messages.len(), 3);
    assert_eq!(
        serde_json::to_value(&page.messages).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_version(&page, "old");
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

// Use a real independent SQLite writer. DELETE mode is fixture-only and makes
// read contention reproducible; production remains WAL. Connections drop
// before AppState releases the guarded fixture root.
struct ContendedHistory {
    writer: std::cell::RefCell<Option<rusqlite::Connection>>,
    prepared: std::cell::RefCell<Option<rusqlite::Connection>>,
    elapsed: std::cell::Cell<Duration>,
    notices: std::cell::RefCell<Vec<Duration>>,
    progressed: std::cell::Cell<bool>,
    state: AppState,
    id: String,
}

impl ContendedHistory {
    fn new() -> Self {
        let (state, id) = fixture();
        let writer = rusqlite::Connection::open(state.persistence_path.as_ref()).unwrap();
        writer.busy_timeout(Duration::ZERO).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode = DELETE;")
            .unwrap();
        let reader = open_sqlite_history_snapshot_with_busy_timeout(
            state.persistence_path.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        Self {
            writer: std::cell::RefCell::new(Some(writer)),
            prepared: std::cell::RefCell::new(Some(reader)),
            elapsed: std::cell::Cell::new(Duration::ZERO),
            notices: std::cell::RefCell::new(Vec::new()),
            progressed: std::cell::Cell::new(false),
            state,
            id,
        }
    }

    fn release(&self) {
        if let Some(writer) = self.writer.borrow_mut().take() {
            writer.execute_batch("ROLLBACK;").unwrap();
        }
    }

    fn capture(
        &self,
        query: SessionHistorySelector<'_>,
        mut on_backoff: impl FnMut(&Self),
    ) -> Result<LocalSessionHistorySnapshot, ApiError> {
        self.state.capture_local_session_history_with_wait(
            &self.id,
            query,
            || self.elapsed.get(),
            |delay| {
                if !self.progressed.replace(true) {
                    // Another state user must really acquire the mutex and
                    // make progress while this operation is waiting.
                    let state_mutex = &self.state.inner;
                    let progressed = std::thread::scope(|scope| {
                        scope
                            .spawn(move || {
                                let Some(mut inner) = state_mutex.try_lock_now() else {
                                    return false;
                                };
                                inner.revision += 1;
                                true
                            })
                            .join()
                            .unwrap()
                    });
                    assert!(progressed, "history backoff held StateInner");
                }
                self.elapsed.set(self.elapsed.get() + delay);
                on_backoff(self);
            },
            |spent| {
                assert!(self.state.inner.try_lock_now().is_some());
                self.notices.borrow_mut().push(spent);
            },
            || {
                if let Some(reader) = self.prepared.borrow_mut().take() {
                    Ok(reader)
                } else {
                    open_sqlite_history_snapshot_with_busy_timeout(
                        self.state.persistence_path.as_ref(),
                        Duration::ZERO,
                    )
                }
            },
        )
    }
}

#[test]
fn history_contention_waits_for_release_with_one_warning_and_state_progress() {
    let fixture = ContendedHistory::new();
    let query = selector(50, 40);
    let captured = fixture
        .capture(query, |fixture| {
            if fixture.elapsed.get() < Duration::from_secs(10) {
                assert!(fixture.notices.borrow().is_empty());
            }
            if fixture.elapsed.get() >= Duration::from_secs(50) {
                fixture.release();
            }
        })
        .unwrap();
    assert!(fixture.progressed.get());
    assert_eq!(fixture.elapsed.get(), Duration::from_secs(50));
    assert_eq!(&*fixture.notices.borrow(), &[Duration::from_secs(10)]);
    let page = captured
        .page(&fixture.id, query, fixture.state.server_instance_id.clone())
        .unwrap();
    assert_eq!(page.messages.len(), 40);
    assert_version(&page, "old");
}

#[test]
fn history_contention_expires_once_with_the_existing_internal_error_class() {
    let fixture = ContendedHistory::new();
    let error = fixture
        .capture(selector(0, 10), |_| {})
        .err()
        .expect("held writer must exhaust the capture budget");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.kind, None);
    assert_eq!(fixture.elapsed.get(), Duration::from_secs(60));
    assert_eq!(&*fixture.notices.borrow(), &[Duration::from_secs(10)]);
    assert!(fixture.progressed.get());
}

#[test]
fn history_contention_recaptures_a_replaced_transcript_after_release() {
    let fixture = ContendedHistory::new();
    let changed = std::cell::Cell::new(false);
    let query = selector(50, 40);
    let captured = fixture
        .capture(query, |fixture| {
            if fixture.elapsed.get() >= Duration::from_secs(50) && !changed.replace(true) {
                fixture.release();
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.id).unwrap();
                replace_session_messages_on_record(
                    inner.session_mut_by_index(index).unwrap(),
                    messages("new"),
                    None,
                );
                inner.finish_body_sequence_commits();
                persist_state(fixture.state.persistence_path.as_ref(), &inner).unwrap();
                let watermark = inner.last_mutation_stamp;
                inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&fixture.id));
                assert_eq!(inner.sessions[index].message_start_index, 66);
            }
        })
        .unwrap();
    let page = captured
        .page(&fixture.id, query, fixture.state.server_instance_id.clone())
        .unwrap();
    assert_version(&page, "new");
    assert_eq!(page.messages.len(), 40);
    assert_eq!(&*fixture.notices.borrow(), &[Duration::from_secs(10)]);
}

#[test]
fn history_contention_revalidates_a_cursor_removed_by_resident_rollback() {
    let fixture = ContendedHistory::new();
    let changed = std::cell::Cell::new(false);
    let query = SessionHistorySelector {
        before: Some("history-20"),
        ..selector(0, 5)
    };
    // Select by cursor rather than the explicit start used by the fixture.
    let query = SessionHistorySelector {
        start: None,
        ..query
    };
    let error = fixture
        .capture(query, |fixture| {
            if fixture.elapsed.get() >= Duration::from_secs(50) && !changed.replace(true) {
                fixture.release();
                let mut inner = fixture.state.inner.lock().unwrap();
                let index = inner.find_session_index(&fixture.id).unwrap();
                let mut replacement = messages("new");
                replacement.truncate(10);
                replace_session_messages_on_record(
                    inner.session_mut_by_index(index).unwrap(),
                    replacement,
                    None,
                );
            }
        })
        .err()
        .expect("removed cursor must not come back from SQLite");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(&*fixture.notices.borrow(), &[Duration::from_secs(10)]);
}

#[test]
fn history_capture_counts_state_lock_wait_in_the_original_budget() {
    let (state, id) = fixture();
    let guard = state.inner.lock().unwrap();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let notices = std::cell::RefCell::new(Vec::new());
    let result = state.capture_local_session_history_with_wait(
        &id,
        selector(0, 5),
        || elapsed.get(),
        |delay| elapsed.set(elapsed.get() + delay),
        |spent| notices.borrow_mut().push(spent),
        || panic!("reader must not open while StateInner is unavailable"),
    );
    drop(guard);
    let error = result
        .err()
        .expect("the original budget also bounds state admission");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(elapsed.get(), Duration::from_secs(60));
    assert_eq!(&*notices.borrow(), &[Duration::from_secs(10)]);
}

#[test]
fn history_capture_does_not_retry_same_connection_locked_errors() {
    let (state, id) = fixture();
    let error = state
        .capture_local_session_history_with_wait(
            &id,
            selector(0, 5),
            || Duration::ZERO,
            |_| panic!("LOCKED is not an external BUSY retry"),
            |_| panic!("a non-contention failure must return immediately"),
            || {
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
                    None,
                )
                .into())
            },
        )
        .err()
        .expect("same-connection lock must retain its error path");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error.kind, None);
}
