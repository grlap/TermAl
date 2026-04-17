// active-turn file change tracking tests.
//
// domain: when an agent turn is active on a session, TermAl subscribes to
// filesystem notifications (via `notify`) against the session's workdir and
// records each touched path into `SessionRecord.active_turn_file_changes`.
// when the turn ends the accumulated set is flushed as a single
// `Message::FileChanges` summary entry in the session transcript so the user
// can see what the agent changed in that turn.
//
// the production entry points these tests exercise:
//   - `StateInner.record_active_turn_file_changes` — ingests
//     `WorkspaceFileChangeEvent`s from the watcher and dispatches them into
//     the right session's `active_turn_file_changes` map. events for paths
//     outside a session's workdir are ignored. when two events describe the
//     same path, one with a `session_id` hint and one without, the hinted
//     event wins (the unhinted copy is treated as a duplicate on other
//     sessions sharing the same project root).
//   - `push_active_turn_file_changes_on_record` — drains the map into a
//     `Message::FileChanges` transcript entry with a pluralized title.
//   - `finish_active_turn_file_change_tracking` — called when the turn ends
//     (e.g. via `finish_turn_ok_if_runtime_matches`). it closes the turn and,
//     if a turn was in fact active, opens a short grace window via
//     `active_turn_file_change_grace_deadline = now + ACTIVE_TURN_FILE_CHANGE_GRACE`
//     (currently 750 ms). if no turn was active it is a no-op that clears
//     any stale tracking state.
//
// grace window rationale: filesystem watchers debounce, so a file the agent
// modified during its turn can surface as a `WorkspaceFileChangeEvent` a few
// hundred milliseconds after the turn technically finishes. without a grace
// window those late events would either attach to the next turn (wrong
// attribution) or be dropped entirely. during the grace window,
// `record_active_turn_file_changes` still accepts matching paths, emits a
// late `Message::FileChanges` summary once the first late event lands, and
// then closes the window so subsequent late events do not double-post.
//
// session-scoped hints rationale: multiple sessions can share the same
// project root (e.g. two codex sessions pointed at the same repo). the
// watcher cannot, on its own, decide which session owns a change. if the
// runtime emits a `session_id` hint on the event, that hint is authoritative
// and the same path must not be fanned out to the other sessions sharing
// the workdir.

use super::*;

// pins: watcher events are aggregated into `active_turn_file_changes` for the
// currently active turn, but only for paths inside the session's workdir.
// a path outside the workdir must be dropped. once
// `push_active_turn_file_changes_on_record` is called, the map is drained
// and a `Message::FileChanges` entry with a singular "Agent changed 1 file"
// title is appended.
// guards against: regressions where watcher noise from unrelated dirs
// leaks into the session transcript, or where the summary message title
// drifts from the expected pluralization.
#[test]
fn active_turn_file_changes_are_summarized_on_record() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-{}",
        Uuid::new_v4()
    ));
    let changed_file = root.join("src").join("main.rs");
    let ignored_file = std::env::temp_dir().join(format!(
        "termal-active-turn-file-changes-outside-{}.rs",
        Uuid::new_v4()
    ));

    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();
    fs::write(&ignored_file, "pub fn outside() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        session_id
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: ignored_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&session_id).unwrap();
    assert_eq!(
        inner.sessions[index].active_turn_file_changes.len(),
        1,
        "only files under the session workdir should be tracked",
    );

    let message_id = inner.next_message_id();
    assert!(push_active_turn_file_changes_on_record(
        &mut inner.sessions[index],
        message_id,
    ));
    assert!(inner.sessions[index].active_turn_file_changes.is_empty());
    match inner.sessions[index].session.messages.last() {
        Some(Message::FileChanges { title, files, .. }) => {
            assert_eq!(title, "Agent changed 1 file");
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, changed_file.to_string_lossy());
            assert_eq!(files[0].kind, WorkspaceFileChangeKind::Modified);
        }
        other => panic!("expected file changes message, got {other:?}"),
    }

    drop(inner);
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(ignored_file).unwrap();
}

// pins: when two sessions share the same workdir and a single watcher
// event is emitted twice — once without a `session_id` hint and once with
// a hint naming the first session — the hint wins and only the first
// session records the change. the second session's
// `active_turn_file_changes` map must stay empty.
// guards against: regressions in the de-duplication path in
// `record_active_turn_file_changes` where a missing `session_id` would
// broadcast the change to every session whose workdir contains the path,
// attributing one agent's writes to unrelated sessions.
#[test]
fn active_turn_file_changes_prefer_session_scoped_watcher_hints() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-active-turn-file-scope-{}", Uuid::new_v4()));
    let changed_file = root.join("src").join("main.rs");
    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();

    let (first_session_id, second_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let first = inner.create_session(
            Agent::Codex,
            Some("First".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let first_session_id = first.session.id.clone();
        let second = inner.create_session(
            Agent::Codex,
            Some("Second".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let second_session_id = second.session.id.clone();
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner.find_session_index(session_id).unwrap();
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
        (first_session_id, second_session_id)
    };

    state.record_active_turn_file_changes(&[
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        },
        WorkspaceFileChangeEvent {
            path: changed_file.to_string_lossy().into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: Some(root.to_string_lossy().into_owned()),
            session_id: Some(first_session_id.clone()),
            mtime_ms: None,
            size_bytes: None,
        },
    ]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let first = inner
        .sessions
        .iter()
        .find(|record| record.session.id == first_session_id)
        .expect("first session should exist");
    let second = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should exist");
    assert_eq!(first.active_turn_file_changes.len(), 1);
    assert!(second.active_turn_file_changes.is_empty());
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

// pins: a watcher event that arrives after the turn has finished but
// before `active_turn_file_change_grace_deadline` elapses still produces a
// `Message::FileChanges` summary entry on the session transcript, even
// though `active_turn_start_message_count` is already `None`. the setup
// calls `finish_active_turn_file_change_tracking` to close the turn and
// open the grace window the way production does, then records the late
// event.
// guards against: regressions where debounced watcher events that land
// shortly after the agent reports completion are silently dropped,
// leaving the user without a record of files the agent just wrote.
#[test]
fn late_turn_file_changes_are_summarized_during_grace_window() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-late-file-change-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let files = session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::FileChanges { files, .. } => Some(files),
            _ => None,
        })
        .expect("late watcher event should create a file-change summary");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].kind, WorkspaceFileChangeKind::Created);
    assert_eq!(files[0].path, changed_file.to_string_lossy());
    fs::remove_dir_all(root).unwrap();
}

// pins: once `active_turn_file_change_grace_deadline` is in the past,
// `record_active_turn_file_changes` must treat the session as fully idle:
// drop the incoming event, clear the deadline, and emit no
// `Message::FileChanges` entry. the deadline is forced into the past by
// assigning `Instant::now() - 1ms` directly (since `Instant` has no public
// constructor for arbitrary points in time, hand-rolling a past instant
// this way is the only reliable way to exercise the expiry branch
// synchronously without sleeping ~750 ms in the test).
// guards against: regressions where the expiry check is inverted, off by
// an equality, or not applied at all — any of which would let stale
// watcher events post an orphan summary long after the turn ended.
#[test]
fn expired_late_turn_file_change_grace_window_does_not_emit_summary() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-expired-late-file-change-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    let changed_file = root.join("generated.rs");
    fs::write(&changed_file, "fn generated() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Expired Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() - Duration::from_millis(1));
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: changed_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist");
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
    assert!(
        record
            .session
            .messages
            .iter()
            .all(|message| !matches!(message, Message::FileChanges { .. }))
    );
    drop(inner);
    fs::remove_dir_all(root).unwrap();
}

// pins: calling `finish_active_turn_file_change_tracking` on a session
// whose `active_turn_start_message_count` is already `None` (i.e. no turn
// was actually running) must clear any stray tracking state without
// opening a new grace window. any pre-existing entries in
// `active_turn_file_changes` are discarded.
// guards against: regressions where a spurious stop/finalize call on an
// idle session opens a 750 ms window during which arbitrary edits by the
// user (outside any agent turn) would get misattributed to the agent.
#[test]
fn idle_finish_active_turn_file_change_tracking_does_not_open_grace_window() {
    let mut inner = StateInner::new();
    let mut record = inner.create_session(
        Agent::Codex,
        Some("Idle Files".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    record.active_turn_file_changes.insert(
        "/tmp/generated.rs".to_owned(),
        WorkspaceFileChangeKind::Created,
    );

    finish_active_turn_file_change_tracking(&mut record);

    assert!(record.active_turn_start_message_count.is_none());
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    assert!(record.active_turn_file_changes.is_empty());
}

// pins: the first late watcher event that lands inside the grace window
// both emits a `Message::FileChanges` summary and clears the grace
// deadline, so a second late event arriving moments later (still within
// the original 750 ms window) must not produce a second summary. only one
// transcript entry attributable to the just-finished turn is expected.
// guards against: regressions where the deadline is not cleared after the
// late summary is pushed, which would let a noisy debounce burst spam the
// transcript with one `Message::FileChanges` per event instead of a
// single aggregated entry per turn.
#[test]
fn late_turn_file_change_grace_window_emits_only_once() {
    let state = test_app_state();
    let root =
        std::env::temp_dir().join(format!("termal-late-file-change-once-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let first_file = root.join("first.rs");
    let second_file = root.join("second.rs");
    fs::write(&first_file, "fn first() {}\n").unwrap();
    fs::write(&second_file, "fn second() {}\n").unwrap();

    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Files".to_owned()),
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        finish_active_turn_file_change_tracking(&mut inner.sessions[index]);
        session_id
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: first_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);
    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: second_file.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: Some(root.to_string_lossy().into_owned()),
        session_id: Some(session_id.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should exist");
    let file_change_messages = session
        .messages
        .iter()
        .filter(|message| matches!(message, Message::FileChanges { .. }))
        .count();
    assert_eq!(file_change_messages, 1);
    fs::remove_dir_all(root).unwrap();
}
