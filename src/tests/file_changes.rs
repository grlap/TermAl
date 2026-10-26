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

struct BlockedFileChangeCapture {
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<usize>>,
}

impl BlockedFileChangeCapture {
    fn finish(mut self) -> usize {
        self.release.take().unwrap().send(()).unwrap();
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for BlockedFileChangeCapture {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn block_file_change_capture(
    state: &Arc<AppState>,
    events: Vec<WorkspaceFileChangeEvent>,
) -> BlockedFileChangeCapture {
    let state = Arc::clone(state);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&dropped);
        FILE_CHANGE_DELETED_OWNER_TEST_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |count| {
                counter.store(count, std::sync::atomic::Ordering::SeqCst);
            }));
        });
        let first = std::cell::Cell::new(true);
        PATH_NORMALIZATION_TEST_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |_| {
                if first.replace(false) {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            }));
        });
        state.record_active_turn_file_changes(&events);
        PATH_NORMALIZATION_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
        FILE_CHANGE_DELETED_OWNER_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
        dropped.load(std::sync::atomic::Ordering::SeqCst)
    });
    let blocked = BlockedFileChangeCapture {
        release: Some(release_tx),
        worker: Some(worker),
    };
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    blocked
}

fn file_change_capture_fixture() -> (Arc<AppState>, String, String) {
    let state = Arc::new(test_app_state());
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let owner = {
        let mut inner = state.inner.lock().unwrap();
        let created = inner.create_session(
            Agent::Codex,
            None,
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let record = inner.sessions.last_mut().unwrap();
        record.active_turn_generation = 1;
        record.active_turn_start_message_count = Some(record.session.messages.len());
        record.file_change_tracking = Some(Arc::new(FileChangeTrackingLifetime::new(Some((
            "original-prompt".to_owned(),
            global_message_index(record, record.session.messages.len()),
        )))));
        push_session_markdown_note_on_record(
            record,
            "original-prompt".to_owned(),
            "Original prompt",
            "fixture".to_owned(),
        );
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: root.join("sibling-root").to_string_lossy().into_owned(),
            common_dir_key: "original-store".to_owned(),
            short_ref: "original-work".to_owned(),
            claim_id: "original-claim".to_owned(),
            generation: 1,
            sealed_revision: None,
        });
        record.engram.active_grant_id = Some("private-grant".to_owned());
        record.engram.routing_token = Some("private-routing-token".to_owned());
        record.engram.active_turn_naming_identity = Some((
            EngramAuthorityStoreKey {
                database_path: root.join("store"),
                project_id: "original-project".to_owned(),
            },
            "original-work".to_owned(),
        ));
        created.session.id
    };
    let path = root.join("deleted.rs").to_string_lossy().into_owned();
    (state, owner, path)
}

fn file_change_capture_event(
    owner: &str,
    path: &str,
    kind: WorkspaceFileChangeKind,
) -> WorkspaceFileChangeEvent {
    WorkspaceFileChangeEvent {
        path: path.to_owned(),
        kind,
        root_path: None,
        session_id: Some(owner.to_owned()),
        mtime_ms: None,
        size_bytes: None,
    }
}

#[test]
fn file_change_capture_retains_original_evidence_across_owner_changes() {
    for change in [
        "stop",
        "successor",
        "workdir",
        "root-claim",
        "root-store",
        "root-seal",
        "naming",
        "routing",
        "grant",
        "intent",
        "dispatch",
        "runtime",
        "missing-anchor",
    ] {
        let (state, owner, path) = file_change_capture_fixture();
        let _runtime_receiver = if change == "runtime" {
            let (handle, receiver) = test_codex_runtime_handle("original-runtime");
            state.inner.lock().unwrap().sessions[0].runtime = SessionRuntime::Codex(handle);
            Some(receiver)
        } else {
            None
        };
        let original_preview = state.inner.lock().unwrap().sessions[0]
            .session
            .preview
            .clone();
        let capture = block_file_change_capture(
            &state,
            vec![file_change_capture_event(
                &owner,
                &path,
                WorkspaceFileChangeKind::Deleted,
            )],
        );
        {
            let mut inner = state
                .inner
                .try_lock_now()
                .expect("capture must release state before I/O");
            let record = &mut inner.sessions[0];
            match change {
                "stop" => {
                    record.set_auto_dispatch_blocked(true);
                    record.runtime_stop_generation += 1;
                    finish_active_turn_file_change_tracking(record);
                }
                "successor" => {
                    clear_active_turn_file_change_tracking(record);
                    record.active_turn_generation += 1;
                    record.active_turn_start_message_count = Some(record.session.messages.len());
                    record.file_change_tracking =
                        Some(Arc::new(FileChangeTrackingLifetime::new(Some((
                            "successor-prompt".to_owned(),
                            global_message_index(record, record.session.messages.len()),
                        )))));
                }
                "workdir" => record.session.workdir.push_str("/successor"),
                // All keep the root generation and path unchanged. Neither is
                // enough to identify the captured naming authority.
                "root-claim" => {
                    record
                        .engram
                        .active_turn_source_root
                        .as_mut()
                        .unwrap()
                        .claim_id = "replacement-claim".to_owned()
                }
                "root-store" => {
                    record
                        .engram
                        .active_turn_source_root
                        .as_mut()
                        .unwrap()
                        .common_dir_key = "replacement-store".to_owned()
                }
                "root-seal" => {
                    record
                        .engram
                        .active_turn_source_root
                        .as_mut()
                        .unwrap()
                        .sealed_revision = Some("sealed".to_owned())
                }
                "naming" => {
                    record
                        .engram
                        .active_turn_naming_identity
                        .as_mut()
                        .unwrap()
                        .0
                        .project_id = "replacement-project".to_owned()
                }
                "routing" => record.engram.routing_token = Some("replacement-routing".to_owned()),
                "grant" => record.engram.active_grant_id = Some("replacement-grant".to_owned()),
                "intent" => {
                    record.engram.active_turn_intent_fingerprint =
                        Some("replacement-intent".to_owned())
                }
                "dispatch" => record.engram.dispatch_generation += 1,
                "runtime" => {
                    let (handle, _receiver) = test_codex_runtime_handle("replacement-runtime");
                    record.runtime = SessionRuntime::Codex(handle);
                }
                "missing-anchor" => record
                    .session
                    .messages
                    .retain(|message| message.id() != "original-prompt"),
                _ => unreachable!(),
            }
        }
        capture.finish();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[0];
        assert!(
            record.active_turn_file_changes.is_empty(),
            "{change} must not borrow a successor accumulator"
        );
        let notes = record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Markdown {
                    title, markdown, ..
                } if title == "Unresolved file changes" => Some(markdown),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(notes.len(), 1, "{change}");
        let evidence: serde_json::Value = serde_json::from_str(
            notes[0]
                .split("```json\n")
                .nth(1)
                .unwrap()
                .split("\n```")
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(evidence["turnGeneration"], 1, "{change}");
        assert!(evidence.get("promptId").is_none());
        assert!(evidence.get("trackingIncarnation").is_none());
        assert_eq!(record.session.preview, original_preview, "{change}");
        assert_eq!(evidence["files"][&path], "deleted", "{change}");
        assert!(!notes[0].contains("private-routing-token"));
        assert!(!notes[0].contains("private-grant"));
        assert!(!notes[0].contains("successor-prompt"));
        assert!(
            !record
                .session
                .messages
                .iter()
                .any(|message| matches!(message, Message::FileChanges { .. })),
            "{change} must not fabricate a completed summary"
        );
    }
}

#[test]
fn file_change_capture_preserves_deleted_junction_and_case_handling() {
    let (state, owner, _) = file_change_capture_fixture();
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let target = root.join("workspace");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("Existing.rs"), "fixture").unwrap();
    let link = root.join("junction");
    super::engram_child_temp::link_directory(&link, &target);
    let mut cases = vec![
        (target.clone(), target.join("missing.rs"), true),
        (target.clone(), link.join("Existing.rs"), true),
        (link.clone(), target.join("Existing.rs"), true),
        (target.clone(), root.join("outside.rs"), false),
    ];
    #[cfg(windows)]
    cases.push((
        target.clone(),
        PathBuf::from(target.join("Existing.rs").to_string_lossy().to_uppercase()),
        true,
    ));
    // A deleted alias cannot be canonicalized. Keep the existing lexical
    // fallback, even though an ancestor-based resolver could infer the target.
    cases.push((target.clone(), link.join("missing.rs"), false));
    for (workdir, candidate, expected) in cases {
        let workdir = workdir.to_string_lossy().into_owned();
        assert_eq!(path_contains(&workdir, &candidate), expected);
        let path = candidate.to_string_lossy().into_owned();
        {
            let mut inner = state.inner.lock().unwrap();
            inner.sessions[0].session.workdir = workdir;
            inner.sessions[0].active_turn_file_changes.clear();
        }
        state.record_active_turn_file_changes(&[file_change_capture_event(
            &owner,
            &path,
            WorkspaceFileChangeKind::Modified,
        )]);
        assert_eq!(
            state.inner.lock().unwrap().sessions[0]
                .active_turn_file_changes
                .contains_key(&path),
            expected
        );
    }
    #[cfg(windows)]
    fs::remove_dir(link).expect("the junction should be removed");
    #[cfg(unix)]
    fs::remove_file(link).expect("the symbolic link should be removed");
}

#[test]
fn file_change_capture_rollback_cannot_reuse_turn_generation() {
    let (state, owner, path) = file_change_capture_fixture();
    let snapshot = state.inner.lock().unwrap().sessions[0].capture_queue_promotion_snapshot();
    let capture = block_file_change_capture(
        &state,
        vec![file_change_capture_event(
            &owner,
            &path,
            WorkspaceFileChangeKind::Modified,
        )],
    );
    {
        let mut inner = state.inner.try_lock_now().unwrap();
        let record = &mut inner.sessions[0];
        let queued = QueuedPromptRecord {
            engram_ordinary_handoff: false,
            engram_waiting: false,
            promoted_message_index: None,
            promotion_disposition_known: true,
            engram_bind: None,
            engram_evaluate: None,
            engram_interrupted: false,
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: "retry-prompt".to_owned(),
                timestamp: stamp_now(),
                text: "retry".to_owned(),
                expanded_text: None,
                source: None,
            },
        };
        record.restore_queue_promotion(snapshot, queued, "rolled-back-prompt");
        assert_eq!(record.active_turn_generation, 1);
        assert!(record.active_turn_start_message_count.is_some());
        assert!(
            record.file_change_tracking.is_none(),
            "rollback invalidates even an identical generation/count"
        );
        record.file_change_tracking = Some(Arc::new(FileChangeTrackingLifetime::new(Some((
            "original-prompt".to_owned(),
            global_message_index(record, 0),
        )))));
    }
    capture.finish();
    let inner = state.inner.lock().unwrap();
    assert!(inner.sessions[0].active_turn_file_changes.is_empty());
    assert!(
        matches!(inner.sessions[0].session.messages.last(), Some(Message::Markdown { title, .. }) if title == "Unresolved file changes")
    );
}

#[test]
fn file_change_capture_completion_and_expired_grace_keep_original_events_once() {
    for capture_in_grace in [false, true] {
        let (state, owner, path) = file_change_capture_fixture();
        if capture_in_grace {
            finish_active_turn_file_change_tracking(&mut state.inner.lock().unwrap().sessions[0]);
        }
        let mut duplicate =
            file_change_capture_event(&owner, &path, WorkspaceFileChangeKind::Created);
        duplicate.session_id = None;
        let first = block_file_change_capture(
            &state,
            vec![
                file_change_capture_event(&owner, &path, WorkspaceFileChangeKind::Created),
                duplicate,
            ],
        );
        let second_path = path.replace("deleted.rs", "other.rs");
        let second = block_file_change_capture(
            &state,
            vec![
                file_change_capture_event(&owner, &path, WorkspaceFileChangeKind::Deleted),
                file_change_capture_event(&owner, &second_path, WorkspaceFileChangeKind::Modified),
            ],
        );
        {
            let mut inner = state.inner.try_lock_now().unwrap();
            let record = &mut inner.sessions[0];
            if !capture_in_grace {
                finish_active_turn_file_change_tracking(record);
            }
            // Advance the deadline rather than sleep: both captures were
            // already eligible. A new capture must be rejected now.
            record.active_turn_file_change_grace_deadline =
                Some(std::time::Instant::now() - Duration::from_secs(1));
        }
        state.record_active_turn_file_changes(&[file_change_capture_event(
            &owner,
            &path.replace("deleted.rs", "too-late.rs"),
            WorkspaceFileChangeKind::Created,
        )]);
        second.finish();
        first.finish();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[0];
        let summaries = record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::FileChanges { files, .. } => Some(files),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summaries.len(),
            1,
            "concurrent pre-expiry captures share one publication"
        );
        assert_eq!(summaries[0].len(), 2);
        assert_eq!(
            summaries[0]
                .iter()
                .find(|entry| entry.path == path)
                .unwrap()
                .kind,
            WorkspaceFileChangeKind::Modified
        );
        assert!(record.active_turn_file_changes.is_empty());
        assert!(!record.session.messages.iter().any(|message| matches!(message, Message::Markdown { title, .. } if title == "Unresolved file changes")));
    }
}

#[test]
fn file_change_capture_tail_trim_preserves_late_summary() {
    for capture_in_grace in [false, true] {
        let (state, owner, path) = file_change_capture_fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            let record = &mut inner.sessions[0];
            for index in 0..SESSION_IN_MEMORY_MESSAGE_LIMIT {
                push_session_markdown_note_on_record(
                    record,
                    format!("turn-output-{index}"),
                    "Turn output",
                    "fixture".to_owned(),
                );
            }
            if capture_in_grace {
                finish_active_turn_file_change_tracking(record);
            }
        }
        let first = block_file_change_capture(
            &state,
            vec![file_change_capture_event(
                &owner,
                &path,
                WorkspaceFileChangeKind::Created,
            )],
        );
        let second_path = path.replace("deleted.rs", "other.rs");
        let second = block_file_change_capture(
            &state,
            vec![file_change_capture_event(
                &owner,
                &second_path,
                WorkspaceFileChangeKind::Modified,
            )],
        );
        {
            let mut inner = state.inner.try_lock_now().unwrap();
            if !capture_in_grace {
                finish_active_turn_file_change_tracking(&mut inner.sessions[0]);
            }
            inner.sessions[0].session.status = SessionStatus::Idle;
            // This fixture has no persist worker: the disconnected channel
            // commits the complete transcript to SQLite synchronously.
            let (_, dispatch) = state
                .commit_locked_with_persist_dispatch(&mut inner)
                .unwrap();
            assert_eq!(dispatch, PersistDispatch::Synchronous);
            let watermark = inner.last_mutation_stamp;
            // Use the persist worker's real trimming path after completion,
            // while both already-eligible captures are still resolving.
            inner.trim_persisted_session_tails(watermark, std::slice::from_ref(&owner));
            let record = &inner.sessions[0];
            assert_eq!(
                record.session.messages.len(),
                SESSION_IN_MEMORY_MESSAGE_LIMIT
            );
            assert!(record.message_start_index > 0);
            assert!(
                !record
                    .session
                    .messages
                    .iter()
                    .any(|message| message.id() == "original-prompt")
            );
        }
        second.finish();
        first.finish();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[0];
        let summaries = record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::FileChanges { files, .. } => Some(files),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summaries.len(),
            1,
            "resident tail trim must retain the original turn's late publication"
        );
        assert_eq!(
            summaries[0].len(),
            2,
            "both captures amend the same summary"
        );
        assert!(
            summaries[0]
                .iter()
                .any(|file| file.path == path && file.kind == WorkspaceFileChangeKind::Created)
        );
        assert!(
            summaries[0]
                .iter()
                .any(|file| file.path == second_path
                    && file.kind == WorkspaceFileChangeKind::Modified)
        );
        assert!(!record.session.messages.iter().any(|message| matches!(message, Message::Markdown { title, .. } if title == "Unresolved file changes")));
    }
}

#[test]
fn file_change_capture_deleted_owner_is_never_reassigned() {
    for hidden in [false, true] {
        let (state, owner, path) = file_change_capture_fixture();
        let capture = block_file_change_capture(
            &state,
            vec![file_change_capture_event(
                &owner,
                &path,
                WorkspaceFileChangeKind::Modified,
            )],
        );
        {
            let mut inner = state.inner.try_lock_now().unwrap();
            let workdir = inner.sessions[0].session.workdir.clone();
            if hidden {
                inner.sessions[0].hidden = true;
            } else {
                inner.remove_session_at(0);
            }
            inner.create_session(Agent::Codex, None, workdir, None, None);
            let successor = inner.sessions.last_mut().unwrap();
            successor.active_turn_start_message_count = Some(successor.session.messages.len());
        }
        assert_eq!(
            capture.finish(),
            1,
            "deleted owner has a counted, path-free discard"
        );
        let inner = state.inner.lock().unwrap();
        assert!(
            inner
                .sessions
                .iter()
                .all(|record| record.active_turn_file_changes.is_empty())
        );
        assert!(
            inner
                .sessions
                .iter()
                .all(
                    |record| !record.session.messages.iter().any(|message| match message {
                        Message::FileChanges { .. } => true,
                        Message::Markdown { title, .. } => title == "Unresolved file changes",
                        _ => false,
                    })
                )
        );
    }
}

#[test]
fn file_change_capture_missing_late_summary_is_retained_as_unresolved() {
    let (state, owner, path) = file_change_capture_fixture();
    finish_active_turn_file_change_tracking(&mut state.inner.lock().unwrap().sessions[0]);
    let capture = block_file_change_capture(
        &state,
        vec![file_change_capture_event(
            &owner,
            &path,
            WorkspaceFileChangeKind::Deleted,
        )],
    );
    let earlier_path = path.replace("deleted.rs", "earlier.rs");
    state.record_active_turn_file_changes(&[file_change_capture_event(
        &owner,
        &earlier_path,
        WorkspaceFileChangeKind::Created,
    )]);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.sessions[0]
            .session
            .messages
            .retain(|message| !matches!(message, Message::FileChanges { .. }));
    }
    capture.finish();
    let inner = state.inner.lock().unwrap();
    assert!(inner.sessions[0].active_turn_file_changes.is_empty());
    assert!(
        matches!(inner.sessions[0].session.messages.last(), Some(Message::Markdown { title, markdown, .. }) if title == "Unresolved file changes" && markdown.contains("deleted.rs"))
    );
    assert!(
        !inner.sessions[0]
            .session
            .messages
            .iter()
            .any(|message| matches!(message, Message::FileChanges { .. }))
    );
}

#[test]
fn file_change_canonicalization_does_not_hold_global_state() {
    let state = Arc::new(test_app_state());
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let owner = {
        let mut inner = state.inner.lock().unwrap();
        let owner = inner.create_session(
            Agent::Codex,
            None,
            root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let record = inner.sessions.last_mut().unwrap();
        record.active_turn_start_message_count = Some(record.session.messages.len());
        owner.session.id
    };
    let path = root.join("changed.rs").to_string_lossy().into_owned();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let watcher_state = Arc::clone(&state);
    let watcher_owner = owner.clone();
    let watcher_path = path.clone();
    let watcher = std::thread::spawn(move || {
        let first = std::cell::Cell::new(true);
        PATH_NORMALIZATION_TEST_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |_| {
                if first.replace(false) {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            }));
        });
        watcher_state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
            path: watcher_path,
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: Some(watcher_owner),
            mtime_ms: None,
            size_bytes: None,
        }]);
        PATH_NORMALIZATION_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    });

    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // The barrier proves canonicalization is in progress. This non-waiting read
    // distinguishes lock ownership without a latency threshold or sleeps.
    let readable_session = state
        .inner
        .try_lock_now()
        .map(|inner| inner.sessions.last().unwrap().session.id.clone());
    // Release and join before asserting, so the expected RED leaves no worker.
    release_tx.send(()).unwrap();
    watcher.join().unwrap();
    assert_eq!(
        readable_session.as_deref(),
        Some(owner.as_str()),
        "ordinary state reads must proceed during watcher canonicalization"
    );
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner
            .sessions
            .last()
            .unwrap()
            .active_turn_file_changes
            .get(&path),
        Some(&WorkspaceFileChangeKind::Modified)
    );
}

#[test]
fn watcher_noise_does_not_defer_idle_session_snapshots() {
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let (plan, watermark) = {
        let mut inner = state.inner.lock().unwrap();
        for _ in 0..1378 {
            inner.create_session(
                Agent::Codex,
                None,
                root.to_string_lossy().into_owned(),
                None,
                None,
            );
        }
        inner.sessions[0].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() - Duration::from_secs(1));
        let plan = inner.collect_persist_delta_plan(0);
        let watermark = plan.watermark;
        (plan, watermark)
    };

    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: root.join("changed.rs").to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    }]);

    let mut inner = state.inner.lock().unwrap();
    let delta = inner.materialize_persist_delta(plan);
    assert_eq!(
        delta.changed_sessions.len(),
        1378,
        "every selected idle session must still materialize after a file event"
    );
    assert!(delta.deferred_session_ids.is_empty());
    assert_eq!(inner.last_mutation_stamp, watermark);
    assert!(
        inner.sessions[0]
            .active_turn_file_change_grace_deadline
            .is_none()
    );
    assert!(
        inner
            .collect_persist_delta(watermark)
            .changed_sessions
            .is_empty()
    );
}

#[test]
fn watcher_active_file_tracking_preserves_persisted_session_snapshots() {
    let state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let (owner, before, watermark) = {
        let mut inner = state.inner.lock().unwrap();
        for _ in 0..2 {
            inner.create_session(
                Agent::Codex,
                None,
                root.to_string_lossy().into_owned(),
                None,
                None,
            );
        }
        for record in &mut inner.sessions {
            record.active_turn_start_message_count = Some(record.session.messages.len());
        }
        (
            inner.sessions[0].session.id.clone(),
            inner
                .sessions
                .iter()
                .map(|record| {
                    serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
                })
                .collect::<Vec<_>>(),
            inner.last_mutation_stamp,
        )
    };
    let path = root.join("changed.rs").to_string_lossy().into_owned();
    let event = WorkspaceFileChangeEvent {
        path: path.clone(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: None,
        session_id: Some(owner),
        mtime_ms: None,
        size_bytes: None,
    };
    state.record_active_turn_file_changes(&[event.clone()]);
    state.record_active_turn_file_changes(&[event]);

    let mut inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[0].active_turn_file_changes.get(&path),
        Some(&WorkspaceFileChangeKind::Created)
    );
    assert!(inner.sessions[1].active_turn_file_changes.is_empty());
    let after = inner
        .sessions
        .iter()
        .map(|record| serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        after, before,
        "collecting runtime file hints leaves persisted content intact"
    );
    assert!(
        inner
            .collect_persist_delta(watermark)
            .changed_sessions
            .is_empty(),
        "runtime-only tracking must not enqueue transcript rewrites"
    );
}

#[test]
fn watcher_late_summary_persists_only_the_owning_session() {
    let mut state = test_app_state();
    let root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let (sender, _receiver) = mpsc::channel();
    state.persist_tx = sender;
    let (owner, watermark) = {
        let mut inner = state.inner.lock().unwrap();
        for _ in 0..3 {
            inner.create_session(
                Agent::Codex,
                None,
                root.to_string_lossy().into_owned(),
                None,
                None,
            );
        }
        inner.sessions[0].active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() + Duration::from_secs(3600));
        (
            inner.sessions[0].session.id.clone(),
            inner.last_mutation_stamp,
        )
    };
    let path = root.join("changed.rs").to_string_lossy().into_owned();
    state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
        path: path.clone(),
        kind: WorkspaceFileChangeKind::Created,
        root_path: None,
        session_id: Some(owner.clone()),
        mtime_ms: None,
        size_bytes: None,
    }]);

    let delta = state.inner.lock().unwrap().collect_persist_delta(watermark);
    assert_eq!(
        delta.changed_sessions.len(),
        1,
        "the actual transcript summary must persist without rewriting idle sessions"
    );
    assert_eq!(delta.changed_sessions[0].session.id, owner);
    assert!(matches!(delta.changed_sessions[0].session.messages.last(),
        Some(Message::FileChanges { files, .. }) if files.len() == 1 && files[0].path == path));
}

#[test]
fn file_change_diagnostic_threshold_includes_preparation_wait_and_held_work() {
    let (sender, receiver) = mpsc::channel();
    let mutex = StateMutex::new_with_diagnostic_reporter(
        (),
        STATE_MUTEX_WARN_AFTER,
        Arc::new(move |diagnostic| sender.send(diagnostic).unwrap()),
    );
    for milliseconds in [249, 250, 251] {
        for stage in 0..3 {
            let mut diagnostic = FileChangeTrackingDiagnostic::default();
            let elapsed = Duration::from_millis(milliseconds);
            match stage {
                0 => diagnostic.preparation = elapsed,
                1 => diagnostic.waited = elapsed,
                _ => diagnostic.finish_held_work(elapsed),
            }
            mutex.report_file_change_tracking(diagnostic);
            assert_eq!(receiver.try_recv().is_ok(), milliseconds >= 250);
        }
    }
    mutex.report_file_change_tracking(FileChangeTrackingDiagnostic {
        preparation: Duration::from_millis(100),
        waited: Duration::from_millis(100),
        held_work: Duration::from_millis(50),
        ..Default::default()
    });
    assert!(matches!(
        receiver.try_recv().unwrap(),
        StateMutexDiagnostic::FileChangeTracking(_)
    ));
}

#[test]
fn file_change_diagnostic_remainder_does_not_double_count_path_checks() {
    let mut diagnostic = FileChangeTrackingDiagnostic {
        scan_inclusive: Duration::from_millis(100),
        path_checks_subset: Duration::from_millis(10),
        late_summaries: Duration::from_millis(20),
        commit: Duration::from_millis(30),
        ..Default::default()
    };
    diagnostic.finish_held_work(Duration::from_millis(1_000));
    assert_eq!(diagnostic.remainder, Duration::from_millis(850));
    // The data must expose time outside the suspect even when path work is tiny.
    assert_eq!(diagnostic.path_checks_subset, Duration::from_millis(10));
    diagnostic.finish_held_work(Duration::from_millis(1));
    assert_eq!(diagnostic.remainder, Duration::ZERO);
}

#[test]
fn file_change_diagnostic_log_labels_all_timings_and_counters() {
    let mut diagnostic = FileChangeTrackingDiagnostic {
        preparation: Duration::from_millis(1),
        waited: Duration::from_millis(2),
        scan_inclusive: Duration::from_millis(100),
        path_checks_subset: Duration::from_millis(10),
        path_check_max: Duration::from_millis(6),
        late_summaries: Duration::from_millis(20),
        commit: Duration::from_millis(30),
        input_changes: 4,
        visited_sessions: 5,
        eligible_sessions: 2,
        path_checks: 3,
        path_matches: 1,
        late_summary_count: 1,
        commit_attempted: true,
        commit_failed: false,
        ..Default::default()
    };
    diagnostic.finish_held_work(Duration::from_millis(1_000));
    assert_eq!(
        diagnostic.to_string(),
        concat!(
            "preparation_ms=1.000 waited_ms=2.000 held_work_ms=1000.000 ",
            "scan_inclusive_ms=100.000 path_checks_subset_ms=10.000 path_check_max_ms=6.000 ",
            "late_summaries_ms=20.000 commit_ms=30.000 remainder_ms=850.000 ",
            "input_changes=4 visited_sessions=5 eligible_sessions=2 path_checks=3 ",
            "path_matches=1 late_summary_count=1 commit_attempted=true commit_failed=false"
        )
    );
}

#[test]
fn file_change_diagnostic_queue_drops_full_and_disconnected_reports() {
    let (sender, receiver) = mpsc::sync_channel(STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY);
    let diagnostic =
        || StateMutexDiagnostic::FileChangeTracking(FileChangeTrackingDiagnostic::default());
    for _ in 0..STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY {
        assert!(try_queue_state_mutex_diagnostic(&sender, diagnostic()));
    }
    assert!(!try_queue_state_mutex_diagnostic(&sender, diagnostic()));
    drop(receiver);
    assert!(!try_queue_state_mutex_diagnostic(&sender, diagnostic()));
}

#[test]
fn file_change_diagnostics_report_active_scan_after_unlock_without_commit() {
    assert_file_change_diagnostics_after_unlock(false, false);
}

#[test]
fn file_change_diagnostics_report_grace_summary_and_commit_after_unlock() {
    assert_file_change_diagnostics_after_unlock(true, false);
}

#[test]
fn file_change_diagnostics_report_failed_commit_after_unlock() {
    assert_file_change_diagnostics_after_unlock(true, true);
}

fn assert_file_change_diagnostics_after_unlock(grace: bool, fail_commit: bool) {
    let mut state = test_app_state();
    let state_slot = Arc::new(Mutex::new(None::<std::sync::Weak<StateMutex<StateInner>>>));
    let reporter_slot = Arc::clone(&state_slot);
    let (sender, receiver) = mpsc::channel();
    state.inner = Arc::new(StateMutex::new_with_diagnostic_reporter(
        StateInner::new(),
        Duration::ZERO,
        Arc::new(move |diagnostic| {
            if let StateMutexDiagnostic::FileChangeTracking(diagnostic) = diagnostic {
                let mutex = reporter_slot
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .upgrade()
                    .unwrap();
                assert!(mutex.is_not_held_by_current_thread_for_test());
                sender.send(diagnostic).unwrap();
            }
        }),
    ));
    *state_slot.lock().unwrap() = Some(Arc::downgrade(&state.inner));
    // Own every artifact through the AppState fixture's RAII root, including
    // the deliberately invalid persistence destination in the failure case.
    let fixture_root = state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let root = fixture_root.join("workspace");
    fs::create_dir(&root).unwrap();
    let (persist_sender, persist_receiver) = mpsc::channel();
    state.persist_tx = persist_sender;
    let _persist_receiver = if fail_commit {
        drop(persist_receiver);
        let blocker = fixture_root.join("not-a-directory");
        fs::write(&blocker, "block directory creation").unwrap();
        state.persistence_path = Arc::new(blocker.join("state.sqlite"));
        None
    } else {
        Some(persist_receiver)
    };
    let session_id = {
        let mut inner = state.inner.lock().unwrap();
        let mut first_id = String::new();
        for index in 0..4 {
            let created = inner.create_session(
                Agent::Codex,
                None,
                root.to_string_lossy().into_owned(),
                None,
                None,
            );
            if index == 0 {
                first_id = created.session.id.clone();
            }
            let record = inner.session_mut(&created.session.id).unwrap();
            if index == 0 && grace {
                record.active_turn_file_change_grace_deadline =
                    Some(std::time::Instant::now() + Duration::from_secs(3_600));
            } else if index != 2 {
                record.active_turn_start_message_count = Some(record.session.messages.len());
            }
            record.hidden = index == 3;
        }
        first_id
    };
    state.record_active_turn_file_changes(&[]);
    assert!(receiver.try_recv().is_err(), "empty input remains a no-op");
    let path = root.join("changed.rs").to_string_lossy().into_owned();
    let event = |path: String, session_id| WorkspaceFileChangeEvent {
        path,
        session_id,
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        mtime_ms: None,
        size_bytes: None,
    };
    state.record_active_turn_file_changes(&[
        event(path.clone(), Some(session_id.clone())),
        event(path.clone(), None),
        event(
            fixture_root
                .join("outside.rs")
                .to_string_lossy()
                .into_owned(),
            None,
        ),
        event("  ".to_owned(), None),
    ]);
    let diagnostic = receiver
        .try_recv()
        .expect("one report even without a commit");
    assert!(receiver.try_recv().is_err());
    assert_eq!(diagnostic.input_changes, 4);
    assert_eq!(diagnostic.visited_sessions, 4);
    assert_eq!(diagnostic.eligible_sessions, 2);
    assert_eq!(diagnostic.path_checks, 3);
    assert_eq!(diagnostic.path_matches, 1);
    assert_eq!(diagnostic.late_summary_count, usize::from(grace));
    assert_eq!(diagnostic.commit_attempted, grace);
    assert_eq!(diagnostic.commit_failed, fail_commit);
    assert!(diagnostic.path_check_max <= diagnostic.path_checks_subset);
    assert!(diagnostic.path_checks_subset <= diagnostic.scan_inclusive);
    assert_eq!(
        diagnostic.held_work,
        diagnostic.scan_inclusive
            + diagnostic.late_summaries
            + diagnostic.commit
            + diagnostic.remainder,
    );
    if !grace {
        assert_eq!(diagnostic.late_summaries, Duration::ZERO);
        assert_eq!(diagnostic.commit, Duration::ZERO);
    }
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session_id).unwrap();
    let record = &inner.sessions[index];
    if grace {
        assert!(record.active_turn_file_changes.is_empty());
        assert!(record.active_turn_file_change_grace_deadline.is_none());
        assert!(match record.session.messages.last() {
            Some(Message::FileChanges { files, .. }) => {
                files.len() == 1 && files[0].path == path
            }
            _ => false,
        });
    } else {
        assert_eq!(record.active_turn_file_changes.len(), 1);
        assert!(record.active_turn_file_changes.contains_key(&path));
    }
    assert!(inner.sessions[1].active_turn_file_changes.is_empty());
}

// pins: watcher events are aggregated into `active_turn_file_changes` for the
// currently active turn, but only for paths inside the session's workdir.
// a path outside the workdir must be dropped. once
// `push_active_turn_file_changes_on_record` is called, the map is drained
// and a `Message::FileChanges` entry with a singular
// "1 file changed during turn"
// title is appended.
// guards against: regressions where watcher noise from unrelated dirs
// leaks into the session transcript, or where the summary message title
// drifts from the expected pluralization.
#[test]
fn active_turn_file_changes_are_summarized_on_record() {
    let state = test_app_state();
    let root = test_temp_dir().join(format!(
        "termal-active-turn-file-changes-{}",
        Uuid::new_v4()
    ));
    let changed_file = root.join("src").join("main.rs");
    let ignored_file = test_temp_dir().join(format!(
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
            assert_eq!(title, "1 file changed during turn");
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

#[test]
fn workspace_watcher_ignores_internal_beads_and_collaboration_metadata() {
    let root = FsPath::new("/tmp/termal-workspace");
    for path in [
        root.join(".beads").join("embeddeddolt").join("journal.idx"),
        root.join(".collab").join("fable-outbox").join("verdict.md"),
        root.join(".BEADS").join("last-touched"),
    ] {
        assert!(
            is_ignored_workspace_file_event_path(&path),
            "internal coordination metadata should not be attributed to an agent turn: {}",
            path.display()
        );
    }
    assert!(
        !is_ignored_workspace_file_event_path(&root.join("src").join("main.rs")),
        "ordinary project source must remain visible to turn file tracking"
    );
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
    let root = test_temp_dir().join(format!("termal-active-turn-file-scope-{}", Uuid::new_v4()));
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
    let root = test_temp_dir().join(format!("termal-late-file-change-{}", Uuid::new_v4()));
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

    let snapshot = state.full_snapshot();
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
    let root = test_temp_dir().join(format!(
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
    let root = test_temp_dir().join(format!("termal-late-file-change-once-{}", Uuid::new_v4()));
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

    let snapshot = state.full_snapshot();
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
