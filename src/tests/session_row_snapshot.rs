//! Retained session-row cuts share unchanged rows without sharing mutable
//! metadata, membership, or transcript sequence housekeeping.
use super::*;

fn populate_rows(inner: &mut StateInner) {
    for name in ["first", "second", "remote"] {
        inner.create_session(
            Agent::Codex,
            Some(name.to_owned()),
            "fixture".to_owned(),
            None,
            None,
        );
    }
    let remote = &mut inner.sessions[2];
    remote.remote_id = Some("upstream".to_owned());
    remote.remote_session_id = Some("remote-session".to_owned());
}

fn rows() -> StateInner {
    let mut inner = StateInner::new();
    populate_rows(&mut inner);
    inner
}

fn persisted_rows(rows: &SessionSequence) -> Value {
    serde_json::to_value(
        rows.iter()
            .map(PersistedSessionRecord::from_record)
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn session_row_cut_survives_unstamped_edits_remove_and_remote_sync_rollback() {
    let mut inner = rows();
    let cut = inner.sessions.clone();
    let original = persisted_rows(&cut);
    let stamps = cut.iter().map(|row| row.mutation_stamp).collect::<Vec<_>>();
    let rollback = RemoteSyncRollback::capture(&inner);
    let original_number = inner.next_session_number;
    let id = cut[0].session.id.clone();

    // An unstamped direct edit must detach just as stamped mutation does.
    inner.sessions[0].session.name = "direct edit".to_owned();
    assert_eq!(inner.sessions[0].mutation_stamp, stamps[0]);
    assert_eq!(cut[0].session.name, "first");
    assert!(!inner.sessions.shares_body(0, &cut, 0));
    assert!(inner.sessions.shares_body(1, &cut, 1));
    let changed = inner.session_mut_by_index(1).unwrap();
    changed.session.name = "stamped edit".to_owned();
    assert!(changed.mutation_stamp > stamps[1]);
    assert_eq!(cut[1].mutation_stamp, stamps[1]);

    let mut removed = inner.remove_session_at(0).unwrap();
    removed.session.name = "owned removal edit".to_owned();
    assert_eq!(cut[0].session.name, "first");
    assert_eq!(inner.sessions[0].session.id, cut[1].session.id);
    assert!(inner.removed_session_ids.contains(&id));
    inner.create_session(Agent::Codex, None, "fixture".to_owned(), None, None);
    assert_eq!(persisted_rows(&cut), original);

    rollback.restore(&mut inner);
    assert_eq!(inner.next_session_number, original_number);
    assert!(inner.removed_session_ids.is_empty());
    assert_eq!(persisted_rows(&inner.sessions), original);
    for index in 0..cut.len() {
        assert!(inner.sessions.shares_body(index, &cut, index));
    }
    // The restored collection still detaches when subsequently changed.
    inner.sessions[0].session.name = "after rollback".to_owned();
    assert_eq!(persisted_rows(&cut), original);
}

#[test]
fn session_rows_keep_array_persistence_and_order_through_restore() {
    let mut inner = rows();
    let previous_window = "2000-01-01T00:00:00.000Z";
    for (_, row) in inner.sessions.matching_mut(|row| row.is_local_session()) {
        row.provider_handoff_observations.observed_since = previous_window.to_owned();
    }
    let before = PersistedState::from_inner(&inner);
    let encoded = serde_json::to_value(&before).unwrap();
    assert!(encoded["sessions"].is_array());
    assert_eq!(
        encoded["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["session"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        inner
            .sessions
            .iter()
            .map(|row| row.session.id.as_str())
            .collect::<Vec<_>>()
    );
    let restored = serde_json::from_value::<PersistedState>(encoded)
        .unwrap()
        .into_inner()
        .unwrap();
    assert_eq!(
        persisted_rows(&restored.sessions),
        persisted_rows(&inner.sessions)
    );
    // Stamps and process-local handoff observation windows restart on load.
    // Check the new windows independently, then compare all other wire fields.
    for row in restored
        .sessions
        .iter()
        .filter(|row| row.is_local_session())
    {
        let history = AppState::wire_session_from_record("epoch", row)
            .provider_handoff_history
            .expect("local observation window");
        assert_ne!(history.observed_since, previous_window);
        assert!(chrono::DateTime::parse_from_rfc3339(&history.observed_since).is_ok());
    }
    assert!(restored.sessions.iter().all(|row| row.mutation_stamp == 0));
    let wire = |rows: &SessionSequence| {
        rows.iter()
            .map(|row| {
                let mut session = AppState::wire_session_from_record("epoch", row);
                session.session_mutation_stamp = None;
                if let Some(history) = &mut session.provider_handoff_history {
                    history.observed_since = "<process-local-window>".to_owned();
                }
                serde_json::to_value(session).unwrap()
            })
            .collect::<Vec<_>>()
    };
    let expected_wire = wire(&inner.sessions);
    let actual_wire = wire(&restored.sessions);
    assert_eq!(actual_wire, expected_wire);
}

#[test]
fn session_row_housekeeping_detaches_only_one_pending_local_row_once() {
    let mut state = test_app_state();
    // The normal broadcaster queues the cut; this test does not run a worker.
    state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let mut inner = state.inner.lock().unwrap();
    populate_rows(&mut inner);
    let initial = inner.sessions.clone();
    let initial_revision = inner.revision;
    state.commit_locked(&mut inner).unwrap();
    assert_eq!(inner.revision, initial_revision + 1);
    for index in 0..initial.len() {
        assert!(
            inner.sessions.shares_body(index, &initial, index),
            "no-op commit detached row {index}"
        );
    }

    // Set up pending local and remote work before retaining the next cut.
    // Even an inconsistent pending local marker on a proxy is never consumed.
    inner.sessions[0].body_sequence.replacement_pending = true;
    inner.sessions[0]
        .body_sequence
        .unrepresented_messages
        .insert("changed".to_owned());
    inner.sessions[2].body_sequence.replacement_pending = true;
    let pending = inner.sessions.clone();
    let stamp = pending[0].mutation_stamp;
    state.commit_locked(&mut inner).unwrap();
    assert!(!inner.sessions.shares_body(0, &pending, 0));
    assert!(inner.sessions.shares_body(1, &pending, 1));
    assert!(inner.sessions.shares_body(2, &pending, 2));
    assert_eq!(
        inner.sessions[0].body_sequence.value,
        pending[0].body_sequence.value + 1
    );
    assert_eq!(inner.sessions[0].mutation_stamp, stamp);
    assert!(!inner.sessions[0].body_sequence.replacement_pending);
    assert!(
        inner.sessions[0]
            .body_sequence
            .unrepresented_messages
            .is_empty()
    );
    assert!(pending[0].body_sequence.replacement_pending);
    assert!(inner.sessions[2].body_sequence.replacement_pending);
    let finished = inner.sessions.clone();
    state.commit_locked(&mut inner).unwrap();
    for index in 0..finished.len() {
        assert!(
            inner.sessions.shares_body(index, &finished, index),
            "second no-op commit detached row {index}"
        );
    }
    assert_eq!(
        inner.sessions[0].body_sequence.value,
        finished[0].body_sequence.value
    );
}

#[test]
fn a_watcher_batch_beside_a_pending_cut_detaches_only_the_rows_it_writes() {
    let mut state = test_app_state();
    // The normal broadcaster queues the cut; this test does not run a worker.
    state.state_broadcast_mailbox = Some(Arc::new(StateBroadcastMailbox::default()));
    let mut inner = state.inner.lock().unwrap();
    populate_rows(&mut inner);
    // Row 0 owns an active turn, so the batch captures (writes) only it.
    inner.sessions[0].active_turn_start_message_count = Some(0);
    state.commit_locked(&mut inner).unwrap();
    {
        let pending = state
            .state_broadcast_mailbox
            .as_ref()
            .unwrap()
            .pending
            .lock()
            .unwrap();
        assert!(matches!(
            pending.work.back(),
            Some(StateBroadcastWork::ImmutableSnapshot(cut)) if cut.pending_sessions.is_some()
        ));
    }
    let held = inner.sessions.clone();
    drop(inner);

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: "src/lib.rs".to_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    }]);

    let inner = state.inner.lock().unwrap();
    assert!(!inner.sessions.shares_body(0, &held, 0));
    for index in 1..held.len() {
        assert!(
            inner.sessions.shares_body(index, &held, index),
            "the watcher batch detached unwritten row {index}"
        );
    }
}

#[test]
fn session_row_mutable_and_owned_accesses_cannot_change_a_retained_cut() {
    // Each path gets a fresh shared cut, so an earlier detachment cannot
    // conceal a later mutable-access escape.
    let mutators: [fn(&mut SessionSequence); 6] = [
        |rows| rows[0].session.preview = "index".to_owned(),
        |rows| rows.get_mut(0).unwrap().session.preview = "get mut".to_owned(),
        |rows| {
            for (_, row) in rows.matching_mut(|row| row.session.name == "first") {
                row.session.preview = "matching mut".to_owned();
            }
        },
        |rows| {
            for (_, row) in rows.matching_indexed_mut(|index, _| index == 0) {
                row.session.preview = "matching indexed mut".to_owned();
            }
        },
        |rows| {
            for row in rows.iter_mut().take(1) {
                row.session.preview = "iterator".to_owned();
            }
        },
        |rows| {
            for row in rows.into_iter().take(1) {
                row.session.preview = "borrowed collection".to_owned();
            }
        },
    ];
    for mutate in mutators {
        let mut inner = rows();
        let cut = inner.sessions.clone();
        let original = persisted_rows(&cut);
        mutate(&mut inner.sessions);
        assert_ne!(inner.sessions[0].session.preview, cut[0].session.preview);
        assert_eq!(persisted_rows(&cut), original);
        assert!(inner.sessions.shares_body(1, &cut, 1));
        assert!(inner.sessions.shares_body(2, &cut, 2));
    }

    let mut inner = rows();
    let cut = inner.sessions.clone();
    let original = persisted_rows(&cut);
    let mut removed = inner.sessions.remove(0);
    removed.session.name = "owned removal".to_owned();
    assert_eq!(persisted_rows(&cut), original);
    let mut owned = cut.clone();
    owned.retain(|row| row.session.id != cut[1].session.id);
    assert_eq!(owned.len(), 2);
    for mut row in owned {
        row.session.name = "owned iteration".to_owned();
    }
    assert_eq!(persisted_rows(&cut), original);
}
