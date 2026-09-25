// The test-run index (docs/features/test-runs.md, slice 1): which launcher
// runs exist, their summaries in the state snapshot, and the live deltas that
// keep clients current. Owns the wire types, the runtime-only index kept in
// `StateInner`, the rescan that turns disk reads into summaries (project and
// session attribution, retention, list order), change detection, and the
// background rescan thread. Does not own reading a run directory
// (`test_runs_disk.rs`) or HTTP (`test_runs_api.rs`), and never starts,
// cancels or settles a run.

/// How often the rescan runs while some indexed run is `running`, and
/// otherwise. Tests drive the rescan directly and never start the thread.
#[cfg(not(test))]
const TEST_RUN_ACTIVE_RESCAN_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(not(test))]
const TEST_RUN_IDLE_RESCAN_INTERVAL: Duration = Duration::from_secs(10);
/// Terminal runs kept in the index per project, newest first.
const TEST_RUN_TERMINAL_RUNS_PER_PROJECT: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunState {
    Running,
    Passed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunStageState {
    Unrun,
    Running,
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunPreset {
    Full,
    Focused,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunStageSummary {
    name: String,
    state: TestRunStageState,
    exit_code: Option<i64>,
    started_at: Option<String>,
    ended_at: Option<String>,
}

/// One run as the snapshot and deltas carry it: never diagnostics or logs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunSummary {
    run_id: String,
    project_id: Option<String>,
    worktree: String,
    run_dir: String,
    preset: TestRunPreset,
    command: Option<Vec<String>>,
    command_truncated: bool,
    detached: Option<bool>,
    state: TestRunState,
    interrupted: bool,
    current_stage: Option<String>,
    stages: Vec<TestRunStageSummary>,
    owner_session_id: Option<String>,
    notify_to: Option<String>,
    notify_session_id: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    exit_code: Option<i64>,
    error: Option<String>,
    /// Opaque: the digest of the `results.json` bytes this summary was built
    /// from, which the detail is built from too, so a client can tell an open
    /// detail is stale even when every other summary field is equal. `None`
    /// without readable results.
    detail_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunDiagnostics {
    text: String,
    truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunStageDetail {
    #[serde(flatten)]
    summary: TestRunStageSummary,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    log: Option<String>,
    diagnostics: Option<TestRunDiagnostics>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunPreflight {
    name: String,
    command: Option<Vec<String>>,
    exit_code: Option<i64>,
    log: Option<String>,
    diagnostics: Option<TestRunDiagnostics>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunDetail {
    stages: Vec<TestRunStageDetail>,
    preflight: Vec<TestRunPreflight>,
    expected_fingerprint: Option<String>,
    before: Option<String>,
    after: Option<String>,
    limitations: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunLogTail {
    text: String,
    truncated: bool,
    size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunListResponse {
    runs: Vec<TestRunSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunDetailResponse {
    run: TestRunSummary,
    detail: TestRunDetail,
}

/// One indexed run: what its directory said, and the summary built from it.
#[derive(Clone, Debug)]
struct TestRunEntry {
    disk: Arc<TestRunDisk>,
    summary: TestRunSummary,
}

/// The runtime-only test-run index kept in `StateInner`, in list order. It
/// mirrors disk and is never persisted.
#[derive(Clone, Debug, Default)]
struct TestRunIndex {
    entries: Vec<TestRunEntry>,
    /// Every run directory the last rescan parsed, listed or aged out, so a
    /// run whose files did not change is never parsed again. Aged-out runs
    /// outnumber listed ones as run directories accumulate.
    parsed: HashMap<PathBuf, Arc<TestRunDisk>>,
}

impl TestRunIndex {
    fn summaries(&self) -> Vec<TestRunSummary> {
        self.entries
            .iter()
            .map(|entry| entry.summary.clone())
            .collect()
    }

    fn find(&self, run_id: &str) -> Option<&TestRunEntry> {
        self.entries
            .iter()
            .find(|entry| entry.summary.run_id == run_id)
    }

    /// Whether the host judges some indexed run `running`. An `unknown` run
    /// does not count: nothing it does needs the fast rescan.
    fn any_running(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.summary.state == TestRunState::Running)
    }
}

/// A run read from disk with its state and project, before session
/// attribution, which needs the state lock.
struct TestRunScanned {
    disk: Arc<TestRunDisk>,
    state: TestRunState,
    project_id: Option<String>,
}

/// The session id `reference` names: an id of a visible session, or the name
/// of exactly one visible root session (not a delegation child), compared
/// without case, as the mailbox resolves `--to`.
fn test_run_resolve_session(inner: &StateInner, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if let Some(index) = inner.find_session_index(reference) {
        return (!inner.sessions[index].hidden).then(|| reference.to_owned());
    }
    let children: HashSet<&str> = inner
        .delegations
        .iter()
        .map(|delegation| delegation.child_session_id.as_str())
        .collect();
    let mut matches = inner.sessions.iter().filter(|record| {
        !record.hidden
            && !children.contains(record.session.id.as_str())
            && record.session.name.eq_ignore_ascii_case(reference)
    });
    let found = matches.next()?;
    matches.next().is_none().then(|| found.session.id.clone())
}

/// The project a run belongs to: the project rooted at its worktree, else the
/// one with the longest root containing it, else the one with the smallest id
/// sharing its Git common directory.
fn test_run_project_id(
    worktree_key: &str,
    projects: &[(String, String, Option<String>)],
    common_key: &str,
) -> Option<String> {
    if let Some((id, _, _)) = projects.iter().find(|(_, root, _)| root == worktree_key) {
        return Some(id.clone());
    }
    let containing = projects
        .iter()
        .filter(|(_, root, _)| worktree_key.starts_with(&format!("{root}/")))
        .max_by_key(|(_, root, _)| root.len());
    if let Some((id, _, _)) = containing {
        return Some(id.clone());
    }
    projects
        .iter()
        .filter(|(_, _, common)| common.as_deref() == Some(common_key))
        .map(|(id, _, _)| id.clone())
        .min()
}

fn test_run_summary(inner: &StateInner, scanned: &TestRunScanned) -> TestRunSummary {
    let disk = &scanned.disk;
    let results = disk.results.as_ref();
    let stages = disk.stages();
    // Found among all stages, so a running stage past the kept ones counts.
    let current_stage = results.and_then(|results| results.current_stage.clone());
    TestRunSummary {
        run_id: disk.run_id.clone(),
        project_id: scanned.project_id.clone(),
        worktree: disk.worktree.clone(),
        run_dir: test_run_display_path(&disk.run_dir),
        preset: disk.preset,
        command: disk.command.clone(),
        command_truncated: disk.command_truncated,
        detached: disk.detached,
        state: scanned.state,
        interrupted: results.is_some_and(|results| results.interrupted),
        current_stage,
        stages,
        owner_session_id: disk
            .owner
            .as_deref()
            .and_then(|owner| test_run_resolve_session(inner, owner)),
        notify_to: disk.notify_to.clone(),
        notify_session_id: disk
            .notify_to
            .as_deref()
            .and_then(|target| test_run_resolve_session(inner, target)),
        started_at: disk.started_at.clone(),
        ended_at: results.and_then(|results| results.ended.clone()),
        exit_code: results.and_then(|results| results.exit_code),
        error: results.and_then(|results| results.error.clone()),
        detail_version: results.map(|results| results.digest.clone()),
    }
}

/// The index after a rescan whose deltas were not all sent: a run whose delta
/// was not sent keeps its previous entry (stays absent if it was new, stays
/// listed if it was removed), so the index never claims a change clients did
/// not receive, and the next rescan finds the difference again and sends it.
fn test_run_settle_entries(
    mut entries: Vec<TestRunEntry>,
    before: &HashMap<String, TestRunEntry>,
    unsent_changed: &HashSet<String>,
    unsent_removed: &[String],
) -> Vec<TestRunEntry> {
    entries.retain_mut(|entry| {
        if !unsent_changed.contains(&entry.summary.run_id) {
            return true;
        }
        match before.get(&entry.summary.run_id) {
            Some(old) => {
                *entry = old.clone();
                true
            }
            None => false,
        }
    });
    entries.extend(
        unsent_removed
            .iter()
            .filter_map(|run_id| before.get(run_id).cloned()),
    );
    entries.sort_by(|left, right| test_run_list_order(&left.disk, &right.disk));
    entries
}

/// Newest first by start time, then by run id.
fn test_run_list_order(left: &TestRunDisk, right: &TestRunDisk) -> std::cmp::Ordering {
    right
        .started_at
        .cmp(&left.started_at)
        .then_with(|| left.run_id.cmp(&right.run_id))
}

impl AppState {
    /// Rescans every local project's run directories and publishes what
    /// changed. Returns whether any indexed run is `running`, which sets the
    /// next rescan's interval.
    #[cfg(not(test))]
    fn refresh_test_runs(&self) -> bool {
        self.refresh_test_runs_with(&test_run_process_may_be_alive, &|event| {
            self.publish_delta(event)
        })
    }

    /// The rescan with process liveness and delta publication injected, so
    /// tests can fix which pids are alive and observe each publication.
    /// `publish` is called under the state lock that allocated the event's
    /// revision.
    fn refresh_test_runs_with(
        &self,
        may_be_alive: &dyn Fn(u32) -> bool,
        publish: &dyn Fn(&DeltaEvent),
    ) -> bool {
        // Project roots and the previous parses, taken under the lock; all
        // disk reads happen without it. The parse cache is moved out, not
        // copied: only this rescan uses it, and a concurrent rescan that
        // finds it empty merely parses again.
        let (roots, mut previous) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let roots: Vec<(String, String)> = inner
                .projects
                .iter()
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .map(|project| (project.id.clone(), project.root_path.clone()))
                .collect();
            (roots, std::mem::take(&mut inner.test_runs.parsed))
        };
        let mut projects = Vec::new();
        let mut common_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (id, root) in roots {
            let common = test_runs_git_common_dir(FsPath::new(&root));
            let common_key = common.as_ref().map(|common| {
                test_run_path_key(
                    &fs::canonicalize(common)
                        .unwrap_or_else(|_| common.clone())
                        .to_string_lossy(),
                )
            });
            if let (Some(common), Some(key)) = (&common, &common_key) {
                common_dirs
                    .entry(key.clone())
                    .or_insert_with(|| common.clone());
            }
            projects.push((id, test_run_path_key(&root), common_key));
        }
        let mut scanned = Vec::new();
        let mut parsed = HashMap::new();
        let mut seen = HashSet::new();
        for (common_key, common_dir) in &common_dirs {
            for run_dir in test_runs_directories(common_dir) {
                let request_stamp = test_run_file_stamp(&run_dir.join("request.json"));
                let results_stamp = test_run_file_stamp(&run_dir.join("results.json"));
                // An entry that kept an earlier read after a failed one is
                // read again whatever its stamps say.
                let disk = match previous.remove(&run_dir) {
                    Some(disk)
                        if disk.read_failures == 0
                            && disk.request_stamp == request_stamp
                            && disk.results_stamp == results_stamp =>
                    {
                        disk
                    }
                    earlier => match TestRunDisk::read(&run_dir, earlier.as_deref()) {
                        Some(disk) => Arc::new(disk),
                        None => continue,
                    },
                };
                parsed.insert(run_dir, disk.clone());
                // Run ids are random, so a repeat means a copied directory. The
                // first in sorted order wins on every scan; nothing is logged,
                // since the rescan would repeat the line every few seconds.
                if !seen.insert(disk.run_id.clone()) {
                    continue;
                }
                let state = disk.state(may_be_alive);
                let project_id =
                    test_run_project_id(&test_run_path_key(&disk.worktree), &projects, common_key);
                scanned.push(TestRunScanned {
                    disk,
                    state,
                    project_id,
                });
            }
        }
        scanned.sort_by(|left, right| test_run_list_order(&left.disk, &right.disk));
        // Every non-terminal run stays; terminal runs are kept newest first, a
        // bounded number per project. Only the kept runs get summaries.
        let mut terminal_kept: HashMap<Option<String>, usize> = HashMap::new();
        scanned.retain(|run| {
            if !run.disk.is_terminal() {
                return true;
            }
            let kept = terminal_kept.entry(run.project_id.clone()).or_default();
            *kept += 1;
            *kept <= TEST_RUN_TERMINAL_RUNS_PER_PROJECT
        });
        // Each delta is published under the lock that allocated its revision,
        // as every commit_delta_locked caller must: published after unlock,
        // another thread's later revision could reach clients first, which
        // they treat as a gap and answer with a full resync.
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.test_runs.parsed = parsed;
        let entries: Vec<TestRunEntry> = scanned
            .iter()
            .map(|run| TestRunEntry {
                summary: test_run_summary(&inner, run),
                disk: run.disk.clone(),
            })
            .collect();
        let before: HashMap<String, TestRunEntry> = inner
            .test_runs
            .entries
            .iter()
            .map(|entry| (entry.summary.run_id.clone(), entry.clone()))
            .collect();
        let removed: Vec<String> = before
            .keys()
            .filter(|run_id| !entries.iter().any(|entry| &entry.summary.run_id == *run_id))
            .cloned()
            .collect();
        let changed: Vec<TestRunSummary> = entries
            .iter()
            // The summary carries the detail version, so a changed summary
            // is exactly what clients need to hear about.
            .filter(|entry| {
                before
                    .get(&entry.summary.run_id)
                    .is_none_or(|old| old.summary != entry.summary)
            })
            .map(|entry| entry.summary.clone())
            .collect();
        let mut removed = removed;
        removed.sort();
        // Once a commit fails, nothing after it is sent; those runs keep
        // their previous entries so the next rescan sends them.
        let mut failed = false;
        let mut unsent_removed = Vec::new();
        let mut unsent_changed = HashSet::new();
        for run_id in removed {
            if failed {
                unsent_removed.push(run_id);
                continue;
            }
            match self.commit_delta_locked(&mut inner) {
                Ok(revision) => publish(&DeltaEvent::TestRunRemoved { revision, run_id }),
                Err(err) => {
                    eprintln!("test runs> failed to record a removed run: {err:#}");
                    failed = true;
                    unsent_removed.push(run_id);
                }
            }
        }
        for run in changed {
            if failed {
                unsent_changed.insert(run.run_id);
                continue;
            }
            match self.commit_delta_locked(&mut inner) {
                Ok(revision) => publish(&DeltaEvent::TestRunChanged { revision, run }),
                Err(err) => {
                    eprintln!("test runs> failed to record a changed run: {err:#}");
                    failed = true;
                    unsent_changed.insert(run.run_id);
                }
            }
        }
        inner.test_runs.entries = if failed {
            test_run_settle_entries(entries, &before, &unsent_changed, &unsent_removed)
        } else {
            entries
        };
        inner.test_runs.any_running()
    }

    /// The indexed run `run_id` and its directory.
    fn test_run_entry(&self, run_id: &str) -> Option<(TestRunSummary, PathBuf)> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let entry = inner.test_runs.find(run_id)?;
        Some((entry.summary.clone(), entry.disk.run_dir.clone()))
    }

    /// Indexed runs, optionally for one project, in list order.
    fn test_run_summaries(&self, project_id: Option<&str>) -> Vec<TestRunSummary> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .test_runs
            .entries
            .iter()
            .filter(|entry| {
                project_id.is_none_or(|project_id| {
                    entry.summary.project_id.as_deref() == Some(project_id)
                })
            })
            .map(|entry| entry.summary.clone())
            .collect()
    }

    /// Starts the background rescan: every 2 s while a run is `running`,
    /// every 10 s otherwise. Nothing else polls. It stops once shutdown is
    /// signalled, so no delta is committed after persistence has shut down.
    #[cfg(not(test))]
    fn spawn_test_run_index(&self) {
        let state = self.clone();
        let shutdown = self.subscribe_shutdown_signal();
        let spawned = std::thread::Builder::new()
            .name("termal-test-runs".to_owned())
            .spawn(move || {
                while !*shutdown.borrow() {
                    let active = state.refresh_test_runs();
                    std::thread::sleep(if active {
                        TEST_RUN_ACTIVE_RESCAN_INTERVAL
                    } else {
                        TEST_RUN_IDLE_RESCAN_INTERVAL
                    });
                }
            });
        if let Err(err) = spawned {
            eprintln!("test runs> failed to start the run index: {err}");
        }
    }
}
