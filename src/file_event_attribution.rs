// Split out of src/sse_broadcast.rs (record_active_turn_file_changes).
// Owns watcher attribution: freeze owners under StateInner, resolve paths
// off-lock, then consume each capture once. Does not own watcher dedup or
// engram workspace_files_changed, which remain in src/workspace_watch.rs.
// No durable queue or filesystem authority is introduced. Publication anchors
// live only as long as the turn or its captures.

struct FileChangeTrackingLifetime {
    incarnation: Uuid,
    prompt_anchor: Option<(String, usize)>,
    // Accessed only while StateInner is held. Captures retain this Arc after a
    // successor replaces the record's lifetime, without retaining the session.
    publication: Mutex<FileChangeTrackingPublication>,
}

#[derive(Default)]
struct FileChangeTrackingPublication {
    completed: bool,
    late_summary_id: Option<String>,
}

impl FileChangeTrackingLifetime {
    fn new(prompt_anchor: Option<(String, usize)>) -> Self {
        Self {
            incarnation: Uuid::new_v4(),
            prompt_anchor,
            publication: Mutex::new(FileChangeTrackingPublication::default()),
        }
    }
}

struct FileChangeOwnerCapture {
    session_id: String,
    tracking: Arc<FileChangeTrackingLifetime>,
    turn_generation: u64,
    start_count: Option<usize>,
    grace_deadline: Option<std::time::Instant>,
    captured_at: std::time::Instant,
    runtime: Option<RuntimeToken>,
    stop_generation: u64,
    stop_in_progress: bool,
    reset_required: bool,
    paused: bool,
    workdir: String,
    dispatch_generation: u64,
    routing_token: Option<String>,
    grant_id: Option<String>,
    intent_fingerprint: Option<String>,
    naming: Option<(EngramAuthorityStoreKey, String)>,
    source_root: Option<EngramTurnSourceRoot>,
    delete_requested: bool,
}

impl FileChangeOwnerCapture {
    fn from_record(record: &mut SessionRecord, captured_at: std::time::Instant) -> Self {
        let tracking = record.file_change_tracking.get_or_insert_with(|| {
            // Older/manual tracking setups have no prompt anchor. They still
            // acquire a non-reused process-local lifetime on first capture.
            Arc::new(FileChangeTrackingLifetime::new(None))
        });
        Self {
            session_id: record.session.id.clone(),
            tracking: Arc::clone(tracking),
            turn_generation: record.active_turn_generation,
            start_count: record.active_turn_start_message_count,
            grace_deadline: record.active_turn_file_change_grace_deadline,
            captured_at,
            runtime: record.runtime.runtime_token(),
            stop_generation: record.runtime_stop_generation,
            stop_in_progress: record.runtime_stop_in_progress,
            reset_required: record.runtime_reset_required,
            paused: record.orchestrator_auto_dispatch_blocked,
            workdir: record.session.workdir.clone(),
            dispatch_generation: record.engram.dispatch_generation,
            routing_token: record.engram.routing_token.clone(),
            grant_id: record.engram.active_grant_id.clone(),
            intent_fingerprint: record.engram.active_turn_intent_fingerprint.clone(),
            naming: record.engram.active_turn_naming_identity.clone(),
            source_root: record.engram.active_turn_source_root.clone(),
            delete_requested: record.engram.source_observation_delete_requested,
        }
    }

    fn still_owns(&self, record: &SessionRecord) -> bool {
        let completed = self
            .tracking
            .publication
            .lock()
            .expect("file tracking mutex poisoned")
            .completed;
        record.is_local_session()
            && record
                .file_change_tracking
                .as_ref()
                .is_some_and(|tracking| tracking.incarnation == self.tracking.incarnation)
            && record.active_turn_generation == self.turn_generation
            && record.session.workdir == self.workdir
            && record.runtime_stop_generation == self.stop_generation
            && record.runtime_stop_in_progress == self.stop_in_progress
            && record.runtime_reset_required == self.reset_required
            && record.orchestrator_auto_dispatch_blocked == self.paused
            && record.engram.dispatch_generation == self.dispatch_generation
            && record.engram.routing_token == self.routing_token
            && record.engram.active_grant_id == self.grant_id
            && record.engram.active_turn_intent_fingerprint == self.intent_fingerprint
            && record.engram.active_turn_naming_identity == self.naming
            && record.engram.active_turn_source_root == self.source_root
            && record.engram.source_observation_delete_requested == self.delete_requested
            && (record.runtime.runtime_token() == self.runtime
                || (completed && record.runtime.runtime_token().is_none()))
            && (record.active_turn_start_message_count == self.start_count
                || (completed && record.active_turn_start_message_count.is_none()))
            && self
                .tracking
                .prompt_anchor
                .as_ref()
                .is_none_or(|(id, position)| {
                    // Persistence trims only the resident prefix. The original
                    // global position then still anchors this unchanged lifetime.
                    // A resident removal/replacement must continue to refuse it.
                    *position < record.message_start_index
                        || record
                            .session
                            .messages
                            .get(*position - record.message_start_index)
                            .is_some_and(|message| message.id() == id)
                })
    }
}

struct PreparedFileChange {
    path: String,
    kind: WorkspaceFileChangeKind,
    path_check: bool,
    matches: bool,
}

// Not Clone: applying consumes the owned batch, so the same capture cannot be
// replayed after kind merging or transcript publication.
struct PreparedFileChangeCapture {
    owner: FileChangeOwnerCapture,
    changes: Vec<PreparedFileChange>,
}

fn prepare_file_change_capture(
    owner: FileChangeOwnerCapture,
    changes: &[WorkspaceFileChangeEvent],
    scoped_paths: &HashSet<String>,
    normalized: &mut HashMap<String, PathBuf>,
) -> PreparedFileChangeCapture {
    let root_key = owner
        .source_root
        .as_ref()
        .filter(|_| owner.grant_id.is_some())
        .map(|root| engram_path_key(&normalize_user_facing_path(FsPath::new(&root.root))));
    let mut prepared = Vec::new();
    for change in changes {
        let path = change.path.trim();
        if path.is_empty() {
            continue;
        }
        let in_root = root_key.as_deref().is_some_and(|root| {
            engram_path_within(
                &engram_path_key(&normalize_user_facing_path(FsPath::new(path))),
                root,
            )
        });
        if !in_root
            && (change.session_id.is_none() && scoped_paths.contains(path)
                || change
                    .session_id
                    .as_deref()
                    .is_some_and(|id| id != owner.session_id))
        {
            continue;
        }
        let matches = if in_root {
            true
        } else {
            let root = normalized
                .entry(owner.workdir.clone())
                .or_insert_with(|| normalize_path_best_effort(FsPath::new(&owner.workdir)))
                .clone();
            let candidate = normalized
                .entry(path.to_owned())
                .or_insert_with(|| normalize_path_best_effort(FsPath::new(path)));
            *candidate == root || candidate.starts_with(&root)
        };
        prepared.push(PreparedFileChange {
            path: path.to_owned(),
            kind: change.kind,
            path_check: !in_root,
            matches,
        });
    }
    PreparedFileChangeCapture {
        owner,
        changes: prepared,
    }
}

fn merge_captured_file_changes(
    target: &mut BTreeMap<String, WorkspaceFileChangeKind>,
    captured: BTreeMap<String, WorkspaceFileChangeKind>,
) {
    for (path, kind) in captured {
        target
            .entry(path)
            .and_modify(|previous| {
                *previous = merge_workspace_file_change_kind(*previous, kind);
            })
            .or_insert(kind);
    }
}

fn amend_late_file_change_summary(
    record: &mut SessionRecord,
    id: &str,
    captured: BTreeMap<String, WorkspaceFileChangeKind>,
) -> bool {
    let Some(index) = record.session.messages.iter().position(|message| {
        matches!(message, Message::FileChanges { .. }) && message.id() == id
    }) else { return false; };
    let Message::FileChanges { files, title, .. } = &mut record.session.messages[index]
    else { unreachable!("selected file change summary"); };
    let mut merged = files
        .iter()
        .map(|entry| (entry.path.clone(), entry.kind))
        .collect();
    merge_captured_file_changes(&mut merged, captured);
    *files = merged
        .into_iter()
        .map(|(path, kind)| FileChangeSummaryEntry { path, kind })
        .collect();
    *title = if files.len() == 1 {
        "1 file changed during turn".to_owned()
    } else {
        format!("{} files changed during turn", files.len())
    };
    record.mark_body_changed(id);
    true
}

fn publish_unresolved_file_change_capture(
    record: &mut SessionRecord,
    message_id: String,
    owner: &FileChangeOwnerCapture,
    captured: &BTreeMap<String, WorkspaceFileChangeKind>,
) {
    // Internal prompt/lifetime identifiers and runtime credentials stay off
    // the wire; the captured generation and scope identify retained evidence.
    let evidence = serde_json::json!({
        "turnGeneration": owner.turn_generation,
        "workdir": owner.workdir,
        "sourceRoot": owner.source_root.as_ref().map(|root| &root.root),
        "capturedDuring": if owner.start_count.is_some() { "active turn" } else { "completion grace window" },
        "graceRemainingMs": owner.grace_deadline.map(|deadline| deadline.saturating_duration_since(owner.captured_at).as_millis()),
        "files": captured,
    });
    push_message_on_record(
        record,
        Message::Markdown {
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: "Unresolved file changes".to_owned(),
            markdown: format!(
                "Watcher events matched the original turn's captured scope, but its ownership changed before path resolution finished. They were retained here without attributing them to the current turn.\n\n```json\n{}\n```",
                serde_json::to_string_pretty(&evidence).expect("file change evidence serializes")
            ),
        },
    );
}

impl AppState {
    fn record_file_change_captures(&self, changes: &[WorkspaceFileChangeEvent]) {
        if changes.is_empty() {
            return;
        }
        let preparation_started = std::time::Instant::now();
        let scoped_paths = changes
            .iter()
            .filter(|change| {
                change
                    .session_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
            })
            .map(|change| change.path.trim().to_owned())
            .collect();
        let mut diagnostic = FileChangeTrackingDiagnostic {
            preparation: preparation_started.elapsed(),
            input_changes: changes.len(),
            ..Default::default()
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        diagnostic.waited = inner.waited;
        let held_started = inner.acquired_at;
        let scan_started = std::time::Instant::now();
        let now = std::time::Instant::now();
        let mut owners = Vec::new();
        for record in &mut inner.sessions {
            diagnostic.visited_sessions += 1;
            if !record.is_local_session() || record.hidden {
                continue;
            }
            if record.active_turn_start_message_count.is_none()
                && !record
                    .active_turn_file_change_grace_deadline
                    .is_some_and(|deadline| now <= deadline)
            {
                record.active_turn_file_change_grace_deadline = None;
                continue;
            }
            diagnostic.eligible_sessions += 1;
            // Direct access: runtime bookkeeping must not stamp persist inputs.
            owners.push(FileChangeOwnerCapture::from_record(record, now));
        }
        diagnostic.scan_inclusive = scan_started.elapsed();
        let capture_held = held_started.elapsed();
        drop(inner);

        let preparation_started = std::time::Instant::now();
        let mut normalized = HashMap::new();
        let captures = owners
            .into_iter()
            .map(|owner| {
                prepare_file_change_capture(owner, changes, &scoped_paths, &mut normalized)
            })
            .collect::<Vec<_>>();
        diagnostic.preparation += preparation_started.elapsed();

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        diagnostic.waited += inner.waited;
        let held_started = inner.acquired_at;
        let mut publications = Vec::new();
        let mut deleted_owner_events = 0;
        let scan_started = std::time::Instant::now();
        for capture in captures {
            let owner = capture.owner;
            let mut matched = BTreeMap::new();
            for change in capture.changes {
                // Timings here measure only the frozen containment decision,
                // never canonicalization. It remains a subset of held scan.
                let started = std::time::Instant::now();
                let matches = change.matches;
                if change.path_check {
                    let elapsed = started.elapsed();
                    diagnostic.path_checks += 1;
                    diagnostic.path_checks_subset += elapsed;
                    diagnostic.path_check_max = diagnostic.path_check_max.max(elapsed);
                }
                if matches {
                    diagnostic.path_matches += 1;
                    matched
                        .entry(change.path)
                        .and_modify(|kind| {
                            *kind = merge_workspace_file_change_kind(*kind, change.kind);
                        })
                        .or_insert(change.kind);
                }
            }
            if matched.is_empty() {
                continue;
            }
            let Some(index) = inner
                .sessions
                .iter()
                .position(|record| record.session.id == owner.session_id && !record.hidden)
            else {
                deleted_owner_events += matched.len();
                continue;
            };
            let record = &mut inner.sessions[index];
            if !owner.still_owns(record) {
                publications.push((index, owner, matched, false));
            } else if record.active_turn_start_message_count.is_some() {
                merge_captured_file_changes(&mut record.active_turn_file_changes, matched);
            } else {
                // Eligibility was fixed at capture, not at delayed apply. A
                // concurrent capture may have consumed grace in the meantime.
                publications.push((index, owner, matched, true));
            }
        }
        diagnostic.scan_inclusive += scan_started.elapsed();
        let mut commit_error = None;
        if !publications.is_empty() {
            let summaries_started = std::time::Instant::now();
            for (index, owner, matched, late) in publications {
                let message_id = inner.next_message_id();
                let record = inner
                    .session_mut_by_index(index)
                    .expect("captured session still exists");
                let previous = owner
                    .tracking
                    .publication
                    .lock()
                    .expect("file tracking mutex poisoned")
                    .late_summary_id
                    .clone();
                if late && previous.is_none() {
                    merge_captured_file_changes(&mut record.active_turn_file_changes, matched);
                    if push_active_turn_file_changes_on_record(record, message_id) {
                        diagnostic.late_summary_count += 1;
                    }
                    record.active_turn_file_change_grace_deadline = None;
                } else if late
                    && previous.as_deref().is_some_and(|id| {
                        record
                            .session
                            .messages
                            .iter()
                            .any(|message| matches!(message, Message::FileChanges { id: message_id, .. } if message_id == id))
                    })
                {
                    amend_late_file_change_summary(record, previous.as_deref().unwrap(), matched);
                } else {
                    publish_unresolved_file_change_capture(record, message_id, &owner, &matched);
                }
            }
            diagnostic.late_summaries = summaries_started.elapsed();
            diagnostic.commit_attempted = true;
            let started = std::time::Instant::now();
            commit_error = self.commit_locked(&mut inner).err();
            diagnostic.commit = started.elapsed();
            diagnostic.commit_failed = commit_error.is_some();
        }
        let held_work = capture_held + held_started.elapsed();
        drop(inner);
        diagnostic.finish_held_work(held_work);
        self.inner.report_file_change_tracking(diagnostic);
        if deleted_owner_events != 0 {
            eprintln!(
                "file changes> discarded {deleted_owner_events} matched event(s): original owner was deleted"
            );
            #[cfg(test)]
            FILE_CHANGE_DELETED_OWNER_TEST_HOOK.with(|hook| {
                if let Some(hook) = hook.borrow().as_ref() {
                    hook(deleted_owner_events);
                }
            });
        }
        if let Some(err) = commit_error {
            eprintln!("state warning> failed to persist turn file-change attribution: {err:#}");
        }
    }
}

#[cfg(test)]
thread_local! {
    static FILE_CHANGE_DELETED_OWNER_TEST_HOOK: std::cell::RefCell<Option<Box<dyn Fn(usize)>>> =
        const { std::cell::RefCell::new(None) };
}
