// Workspace file-watcher — filesystem notifications from `notify` translated
// into `WorkspaceFileChangeEvent`s and fanned out via `workspace_file_changes`
// SSE events so the UI's diff / source / git / file-tree panels can refresh
// only the touched scopes.
//
// Covers: watcher thread (`run_workspace_file_watcher`), watched-root
// reconciliation (`reconcile_workspace_file_watch_roots`), scope collection
// from projects + sessions (`collect_workspace_file_watch_scopes`),
// root-path canonicalization, nested-root pruning, notify-event translation,
// change-kind coalescing (delete+create → Modified), and ignored-path filtering
// (`.git/`, `node_modules/`, `target/`, etc.).
//
// Extracted from state.rs into its own `include!()` fragment so state.rs
// stays focused on the core state model.

#[cfg(not(test))]
const WORKSPACE_FILE_WATCH_ROOT_REFRESH_MS: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const WORKSPACE_FILE_WATCH_COALESCE_MS: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const WORKSPACE_FILE_WATCH_RECV_TIMEOUT_MS: Duration = Duration::from_millis(100);

#[derive(Clone)]
struct WorkspaceFileWatchScope {
    root_path: PathBuf,
    session_id: Option<String>,
}

#[cfg(not(test))]
fn run_workspace_file_watcher(state: AppState) {
    let (event_tx, event_rx) = mpsc::channel::<notify::Result<NotifyEvent>>();
    let mut watcher = match RecommendedWatcher::new(
        move |result| {
            let _ = event_tx.send(result);
        },
        NotifyConfig::default(),
    ) {
        Ok(watcher) => watcher,
        Err(err) => {
            eprintln!("file watch> failed to start workspace watcher: {err}");
            return;
        }
    };

    let mut watched_roots = BTreeSet::<PathBuf>::new();
    let mut watch_scopes = Vec::<WorkspaceFileWatchScope>::new();
    let mut pending_changes = BTreeMap::<String, WorkspaceFileChangeEvent>::new();
    let mut last_change_at: Option<std::time::Instant> = None;
    let mut next_root_refresh_at = std::time::Instant::now();

    loop {
        let now = std::time::Instant::now();
        if now >= next_root_refresh_at {
            reconcile_workspace_file_watch_roots(
                &state,
                &mut watcher,
                &mut watched_roots,
                &mut watch_scopes,
            );
            next_root_refresh_at = now + WORKSPACE_FILE_WATCH_ROOT_REFRESH_MS;
        }

        match event_rx.recv_timeout(WORKSPACE_FILE_WATCH_RECV_TIMEOUT_MS) {
            Ok(Ok(event)) => {
                let changes = workspace_file_changes_from_notify_event(&event, &watch_scopes);
                state.record_active_turn_file_changes(&changes);
                for change in changes {
                    let key = workspace_file_change_event_key(&change);
                    pending_changes
                        .entry(key)
                        .and_modify(|current| {
                            current.kind =
                                merge_workspace_file_change_kind(current.kind, change.kind);
                            current.mtime_ms = change.mtime_ms;
                            current.size_bytes = change.size_bytes;
                        })
                        .or_insert(change);
                    last_change_at = Some(std::time::Instant::now());
                }
            }
            Ok(Err(err)) => {
                eprintln!("file watch> workspace watcher error: {err}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if last_change_at.is_some_and(|at| at.elapsed() >= WORKSPACE_FILE_WATCH_COALESCE_MS) {
            let changes = pending_changes.values().cloned().collect::<Vec<_>>();
            pending_changes.clear();
            last_change_at = None;
            state.publish_workspace_files_changed(changes);
        }
    }
}

#[cfg(not(test))]
fn reconcile_workspace_file_watch_roots(
    state: &AppState,
    watcher: &mut RecommendedWatcher,
    watched_roots: &mut BTreeSet<PathBuf>,
    watch_scopes: &mut Vec<WorkspaceFileWatchScope>,
) {
    let next_scopes = collect_workspace_file_watch_scopes(state);
    let next_roots = prune_nested_workspace_file_watch_roots(
        next_scopes
            .iter()
            .map(|scope| scope.root_path.clone())
            .collect::<Vec<_>>(),
    );
    let next_root_set = next_roots.iter().cloned().collect::<BTreeSet<_>>();
    for root in watched_roots
        .difference(&next_root_set)
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Err(err) = watcher.unwatch(&root) {
            eprintln!("file watch> failed to unwatch {}: {err}", root.display());
        }
        watched_roots.remove(&root);
    }

    for root in next_roots {
        if watched_roots.contains(&root) {
            continue;
        }

        match watcher.watch(&root, RecursiveMode::Recursive) {
            Ok(()) => {
                watched_roots.insert(root);
            }
            Err(err) => {
                eprintln!("file watch> failed to watch {}: {err}", root.display());
            }
        }
    }

    *watch_scopes = next_scopes;
}

fn collect_workspace_file_watch_scopes(state: &AppState) -> Vec<WorkspaceFileWatchScope> {
    let roots = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let local_project_ids = inner
            .projects
            .iter()
            .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();
        let mut roots = Vec::new();
        for project in &inner.projects {
            if project.remote_id == LOCAL_REMOTE_ID {
                roots.push((project.root_path.clone(), None));
            }
        }
        for record in &inner.sessions {
            if record.remote_id.is_some() {
                continue;
            }

            let is_local_session = record
                .session
                .project_id
                .as_deref()
                .map(|project_id| local_project_ids.contains(project_id))
                .unwrap_or(true);
            if is_local_session {
                roots.push((
                    record.session.workdir.clone(),
                    Some(record.session.id.clone()),
                ));
            }
        }
        roots
    };

    let mut scopes = roots
        .into_iter()
        .filter_map(|(root, session_id)| {
            canonical_workspace_file_watch_root(&root).map(|root_path| WorkspaceFileWatchScope {
                root_path,
                session_id,
            })
        })
        .collect::<Vec<_>>();
    scopes.sort_by(|left, right| {
        left.root_path
            .cmp(&right.root_path)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    scopes.dedup_by(|left, right| {
        left.root_path == right.root_path && left.session_id == right.session_id
    });
    scopes
}

fn canonical_workspace_file_watch_root(root: &str) -> Option<PathBuf> {
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return None;
    }

    let canonical = fs::canonicalize(trimmed).ok()?;
    canonical.is_dir().then(|| normalize_user_facing_path(&canonical))
}

fn prune_nested_workspace_file_watch_roots(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.sort_by_key(|root| root.components().count());
    let mut pruned = Vec::<PathBuf>::new();
    for root in roots {
        if pruned.iter().any(|existing| root.starts_with(existing)) {
            continue;
        }
        pruned.push(root);
    }
    pruned
}

#[cfg(not(test))]
fn workspace_file_changes_from_notify_event(
    event: &NotifyEvent,
    watch_scopes: &[WorkspaceFileWatchScope],
) -> Vec<WorkspaceFileChangeEvent> {
    let Some(kind) = workspace_file_change_kind(&event.kind) else {
        return Vec::new();
    };

    event
        .paths
        .iter()
        .filter(|path| !is_ignored_workspace_file_event_path(path))
        .flat_map(|path| workspace_file_changes_from_path(path, kind, watch_scopes))
        .collect()
}

#[cfg(not(test))]
fn workspace_file_change_kind(kind: &NotifyEventKind) -> Option<WorkspaceFileChangeKind> {
    match kind {
        NotifyEventKind::Access(_) => None,
        NotifyEventKind::Create(_) => Some(WorkspaceFileChangeKind::Created),
        NotifyEventKind::Modify(_) => Some(WorkspaceFileChangeKind::Modified),
        NotifyEventKind::Remove(_) => Some(WorkspaceFileChangeKind::Deleted),
        NotifyEventKind::Any | NotifyEventKind::Other => Some(WorkspaceFileChangeKind::Other),
    }
}

fn merge_workspace_file_change_kind(
    current: WorkspaceFileChangeKind,
    next: WorkspaceFileChangeKind,
) -> WorkspaceFileChangeKind {
    match (current, next) {
        (WorkspaceFileChangeKind::Deleted, WorkspaceFileChangeKind::Created)
        | (WorkspaceFileChangeKind::Created, WorkspaceFileChangeKind::Deleted) => {
            WorkspaceFileChangeKind::Modified
        }
        (WorkspaceFileChangeKind::Deleted, _) | (_, WorkspaceFileChangeKind::Deleted) => {
            WorkspaceFileChangeKind::Deleted
        }
        (WorkspaceFileChangeKind::Created, _) | (_, WorkspaceFileChangeKind::Created) => {
            WorkspaceFileChangeKind::Created
        }
        (WorkspaceFileChangeKind::Modified, _) | (_, WorkspaceFileChangeKind::Modified) => {
            WorkspaceFileChangeKind::Modified
        }
        _ => WorkspaceFileChangeKind::Other,
    }
}

fn workspace_file_changes_from_path(
    path: &FsPath,
    kind: WorkspaceFileChangeKind,
    watch_scopes: &[WorkspaceFileWatchScope],
) -> Vec<WorkspaceFileChangeEvent> {
    let metadata = fs::metadata(path).ok();
    let normalized_path = normalize_user_facing_path(path);
    let path_string = normalized_path.to_string_lossy().into_owned();
    let mtime_ms = metadata.as_ref().and_then(file_metadata_mtime_ms);
    let size_bytes = metadata.as_ref().map(|metadata| metadata.len());
    let mut matching_scopes = watch_scopes
        .iter()
        .filter(|scope| normalized_path.starts_with(&scope.root_path))
        .collect::<Vec<_>>();
    matching_scopes.sort_by(|left, right| {
        right
            .root_path
            .components()
            .count()
            .cmp(&left.root_path.components().count())
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    if matching_scopes.is_empty() {
        return vec![WorkspaceFileChangeEvent {
            path: path_string,
            kind,
            root_path: None,
            session_id: None,
            mtime_ms,
            size_bytes,
        }];
    }

    let mut seen = HashSet::<(PathBuf, Option<String>)>::new();
    matching_scopes
        .into_iter()
        .filter_map(|scope| {
            let key = (scope.root_path.clone(), scope.session_id.clone());
            if !seen.insert(key) {
                return None;
            }

            Some(WorkspaceFileChangeEvent {
                path: path_string.clone(),
                kind,
                root_path: Some(scope.root_path.to_string_lossy().into_owned()),
                session_id: scope.session_id.clone(),
                mtime_ms,
                size_bytes,
            })
        })
        .collect()
}

#[cfg_attr(test, allow(dead_code))]
fn workspace_file_change_event_key(change: &WorkspaceFileChangeEvent) -> String {
    format!(
        "{}\0{}\0{}",
        change.root_path.as_deref().unwrap_or(""),
        change.session_id.as_deref().unwrap_or(""),
        change.path
    )
}

#[cfg(not(test))]
fn is_ignored_workspace_file_event_path(path: &FsPath) -> bool {
    const IGNORED_COMPONENTS: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        ".termal",
        ".next",
        ".nuxt",
        ".vite",
        "dist",
        "node_modules",
        "target",
    ];

    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy();
        IGNORED_COMPONENTS
            .iter()
            .any(|ignored| value.eq_ignore_ascii_case(ignored))
    })
}
