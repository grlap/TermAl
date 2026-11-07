// Classifies supplied notification paths only. Git owns ignore semantics;
// this module does not walk repositories or alter check-credit observations.
// Queries are batched per workdir, off the watcher callback and StateInner.
fn query_workspace_ignored_changes(
    root: &str, changes: &[WorkspaceFileChangeEvent], project_roots: &[PathBuf], deadline: std::time::Instant,
) -> Result<HashSet<String>> {
    if deadline <= std::time::Instant::now() { anyhow::bail!("ignore query deadline exceeded before launch"); }
    let root = normalize_path_best_effort(FsPath::new(root));
    let mut requests = BTreeMap::<Vec<u8>, String>::new();
    for change in changes {
        if change.path.is_empty() { continue; }
        let path = FsPath::new(&change.path);
        let Ok(relative) = path.strip_prefix(&root) else { continue; };
        if relative.as_os_str().is_empty() { continue; }
        let Some(relative) = relative.to_str() else { continue; };
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        let request = format!("./{relative}").into_bytes();
        requests.insert(request, change.path.clone());
    }
    if requests.is_empty() { return Ok(HashSet::new()); }
    let mut input = Vec::new();
    for path in requests.keys() { input.extend_from_slice(path); input.push(0); }
    if input.len() > WORKSPACE_PENDING_BYTES { anyhow::bail!("ignore batch too large"); }
    let git = ReviewFreezeGit::for_ignore_query(&root, Some(deadline), None)?;
    let mut command = git.command(&["check-ignore", "-z", "--stdin", "--non-matching", "-v"]);
    // Ordinary subdirectory workdirs inherit their known project's Git
    // policy. Independent projects stop discovery at their own boundary;
    // a nested repository's .git is still found before an enclosing one.
    let boundary = project_roots.iter().filter(|project| root.starts_with(project))
        .max_by_key(|project| project.components().count()).unwrap_or(&root);
    if let Some(parent) = boundary.parent() { command.env("GIT_CEILING_DIRECTORIES", parent); }
    // Preserve the index: force-tracked files matching an ignore rule
    // remain visible. No --no-index, status, ls-files or tree discovery.
    let output = run_bounded_read_input(&mut command, deadline, WORKSPACE_PENDING_BYTES * 4, input)?;
    if !output.stderr.is_empty() || !(output.status.success() || output.status.code() == Some(1)) {
        anyhow::bail!("ignore query unavailable");
    }
    let keys = requests.keys().cloned().collect::<Vec<_>>();
    let ignored = identity_ignore_matches(&keys, &output.stdout)?;
    Ok(ignored.into_iter().filter_map(|key| requests.remove(&key)).collect())
}

fn workspace_ignored_changes(root: &str, changes: &[WorkspaceFileChangeEvent], project_roots: &[PathBuf]) -> HashSet<String> {
    let deadline = std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT;
    #[cfg(test)]
    let deadline = WORKSPACE_IGNORE_DEADLINE_TEST_OVERRIDE.with(|value| value.get()).unwrap_or(deadline);
    // Failure is unknown, never evidence to hide a real source change. The
    // independent transport/turn-summary cap still prevents a UI flood.
    query_workspace_ignored_changes(root, changes, project_roots, deadline).unwrap_or_default()
}

#[cfg(test)]
thread_local! {
    static WORKSPACE_IGNORE_DEADLINE_TEST_OVERRIDE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

impl AppState {
    fn filter_workspace_publication(&self, changes: Vec<WorkspaceFileChangeEvent>) -> Vec<WorkspaceFileChangeEvent> {
        if changes.is_empty() { return changes; }
        let (roots, project_roots) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let project_roots = inner.projects.iter().filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .map(|project| normalize_user_facing_path(FsPath::new(&project.root_path))).collect::<Vec<_>>();
            let roots = inner.sessions.iter().filter(|record| record.is_local_session() && !record.hidden)
                .flat_map(|record| std::iter::once(record.session.workdir.clone())
                    .chain(record.engram.observed_source_root().map(|root| root.root.clone())))
                .chain(inner.projects.iter().filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                    .map(|project| project.root_path.clone()))
                .collect::<BTreeSet<_>>();
            (roots, project_roots)
        };
        let ancestors = changes.iter().filter(|change| !change.path.is_empty())
            .flat_map(|change| FsPath::new(&change.path).ancestors().map(engram_path_key))
            .collect::<HashSet<_>>();
        let covering = changes.iter().filter(|change| change.kind == WorkspaceFileChangeKind::Subtree && !change.path.is_empty())
            .map(|change| FsPath::new(&change.path)).collect::<Vec<_>>();
        let roots = roots.into_iter().filter(|root| {
            let path = normalize_user_facing_path(FsPath::new(root));
            ancestors.contains(&engram_path_key(&path)) || covering.iter().any(|marker| path.starts_with(marker))
        }).map(|root| normalize_path_best_effort(FsPath::new(&root)))
            .collect::<BTreeSet<_>>();
        let policies = roots.iter()
            .filter(|root| ancestors.contains(&engram_path_key(root)))
            .map(|root| {
            let ignored = workspace_ignored_changes(&root.to_string_lossy(), &changes, &project_roots);
            (root, ignored)
        }).collect::<Vec<_>>();
        let mut publication = Vec::new();
        for change in changes {
            let policy = policies.iter().filter(|(root, _)| FsPath::new(&change.path).starts_with(root))
                .max_by_key(|(root, _)| root.components().count());
            if !change.path.is_empty() && policy.is_some_and(|(_, ignored)| ignored.contains(&change.path)) {
                if change.kind == WorkspaceFileChangeKind::Subtree {
                    // Collapse may have absorbed real facts from active roots
                    // below this ignored ancestor. Retain their coverage, not
                    // the ignored ancestor's checkout contents. No tree walk.
                    for root in roots.iter().filter(|root| root.starts_with(&change.path)) {
                        let mut covered = change.clone();
                        covered.path = root.to_string_lossy().into_owned();
                        publication.push(covered);
                    }
                }
            } else {
                // Classification is not an origin-scope restriction. Open tabs
                // can reference a known root beside their session's workdir.
                publication.push(change);
            }
        }
        publication
    }
}
