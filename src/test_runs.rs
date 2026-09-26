// The test-run index (docs/features/test-runs.md, slices 1 and 2): which
// launcher runs exist, their summaries in the state snapshot, and the live
// deltas that keep clients current. Owns the wire types, the runtime-only
// index kept in `StateInner`, the rescan that turns disk reads into summaries
// (project and session attribution, the unknown reason, retention, list
// order), change detection, rescan serialization, and the background rescan
// thread with its directory stats. Does not own reading a run directory
// (`test_runs_disk.rs`) or HTTP (`test_runs_api.rs`), and never starts,
// cancels or settles a run.

/// The background thread's tick: it rescans on every tick while some indexed
/// run is `running`, and otherwise stats the tracked directories on every
/// tick and rescans when one changed. Tests drive the rescan directly and
/// never start the thread.
#[cfg(not(test))]
const TEST_RUN_TICK: Duration = Duration::from_secs(2);
/// Whether the host creates test-run cards. Off until the card UI ships
/// (tm-ncc6.13.3): a UI without the card renderer breaks on a `testRun`
/// message (docs/features/test-runs.md, "Rollout: the UI first"). While off,
/// the rescan thread never starts the card epoch, so no card is created and
/// the epoch is first stored when cards go live. Tests enable cards directly.
#[cfg(not(test))]
const TEST_RUN_CARDS_SHIPPED: bool = false;
/// The idle backstop: a full rescan at least this often, for a change made in
/// the same modification-time tick as a stat.
const TEST_RUN_IDLE_RESCAN_INTERVAL: Duration = Duration::from_secs(10);
/// Terminal runs kept in the index per project, newest first.
const TEST_RUN_TERMINAL_RUNS_PER_PROJECT: usize = 50;
/// Reads after a liveness check, per run per rescan. A run hands over from its
/// creator to at most one worker, so two reads settle it; one more is slack.
const TEST_RUN_READS_AFTER_CHECK_MAX: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunState {
    Running,
    Passed,
    Failed,
    Unknown,
}

/// Why a run reads `unknown`, when that is known (slice 2). `processGone` and
/// `noPid` come only from results read after the liveness check, so they are
/// final for that process and settle a wait. `resultsUnreadable` may still
/// resolve on a later read and never settles one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum TestRunUnknownReason {
    ProcessGone,
    ResultsUnreadable,
    NoPid,
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
    /// Only with `state: unknown`, and only when the reason is known. Sent
    /// only when set, an additive field: a missing reason means "not known".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unknown_reason: Option<TestRunUnknownReason>,
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
    /// The directories whose change means a run appeared or became readable,
    /// with the modification time each had when the last rescan listed or
    /// read it: every `review-runs` directory, and every run directory the
    /// rescan skipped for want of a readable request (the launcher creates the
    /// directory, then renames `request.json` into it). The background thread
    /// stats them between rescans (`test_run_tracked_dirs_changed`).
    tracked_dirs: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    /// Held for a whole rescan, so rescans never interleave: a scan that
    /// started earlier can never commit over a newer one.
    rescan: Arc<Mutex<()>>,
    /// When this host process first saw each indexed run, which decides
    /// whether a run already terminal then still gets a card
    /// (`test_run_cards.rs`).
    first_sight: HashMap<String, TestRunFirstSight>,
    /// Whether the card epoch is durable, so cards may be created.
    cards_enabled: bool,
    /// Set when cards become enabled in this process: the next scan
    /// re-evaluates every indexed run without a card once.
    cards_reevaluate: bool,
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
    unknown_reason: Option<TestRunUnknownReason>,
    project_id: Option<String>,
}

/// Why `state` is `unknown`. `processGone` and `noPid` are classified only
/// from results read after the liveness check that found the responsible
/// process gone, or no pid at all: a launcher writes its terminal results just
/// before it exits, so an earlier read can predate them. When that read after
/// the check failed, the reason is `resultsUnreadable`, never `processGone`.
/// When the bounded reads after checks ran out on a run still handing over,
/// the reason is not known.
fn test_run_unknown_reason(
    disk: &TestRunDisk,
    state: TestRunState,
    read_after_check_failed: bool,
) -> Option<TestRunUnknownReason> {
    if state != TestRunState::Unknown {
        return None;
    }
    if disk.results.is_none() || read_after_check_failed {
        return Some(TestRunUnknownReason::ResultsUnreadable);
    }
    if disk.unknown_needs_read_after_check(state) {
        return None;
    }
    Some(match disk.responsible_pid() {
        Some(_) => TestRunUnknownReason::ProcessGone,
        None => TestRunUnknownReason::NoPid,
    })
}

/// Whether the background thread rescans on this tick: on every tick while a
/// run is `running`, at once when a tracked directory changed, and otherwise
/// once the idle interval has passed since the last rescan (or there has been
/// none). `dirs_changed` is only asked when nothing else decides.
fn test_run_rescan_due(
    active: bool,
    since_last_rescan: Option<Duration>,
    dirs_changed: impl FnOnce() -> bool,
) -> bool {
    active
        || since_last_rescan.is_none_or(|since| since >= TEST_RUN_IDLE_RESCAN_INTERVAL)
        || dirs_changed()
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
        unknown_reason: scanned.unknown_reason,
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
        self.refresh_test_runs_with(&test_run_process_writer_may_be_alive, &|event| {
            self.publish_delta(event)
        })
    }

    /// The rescan with process liveness and delta publication injected, so
    /// tests can fix which pids are alive and observe each publication.
    /// `writer_may_be_alive` gets a recorded pid and when the file carrying
    /// it was last written. `publish` is called under the state lock that
    /// allocated the event's revision.
    fn refresh_test_runs_with(
        &self,
        writer_may_be_alive: &TestRunLiveness,
        publish: &dyn Fn(&DeltaEvent),
    ) -> bool {
        // One rescan at a time, from its first read to its last commit. A
        // rescan that panicked leaves nothing half-done behind the lock, so a
        // poisoned lock is still taken.
        let rescan = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .rescan
            .clone();
        let _serialized = rescan
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Project roots and the previous parses, taken under the lock; all
        // disk reads happen without it. The parse cache is moved out, not
        // copied: rescans are serialized, so only this one uses it.
        // The card map and the published detail versions are copied too: they
        // decide the retention exemption and which failure excerpts to read
        // before the lock.
        let (roots, mut previous, card_refs, retained_by_cards, published_versions, reevaluate_cards) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let roots: Vec<(String, String)> = inner
                .projects
                .iter()
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .map(|project| (project.id.clone(), project.root_path.clone()))
                .collect();
            let published_versions: HashMap<String, Option<String>> = inner
                .test_runs
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.summary.run_id.clone(),
                        entry.summary.detail_version.clone(),
                    )
                })
                .collect();
            // Only a card that can still take its terminal snapshot keeps its
            // run indexed: one in its session's in-memory window. A card out
            // of that window is not updated yet (tm-ncc6.13.4), so exempting
            // its run would keep it past the retention cap for good.
            let retained_by_cards: HashSet<String> = inner
                .test_run_cards
                .iter()
                .filter(|(_, card)| !card.terminal && test_run_card_is_resident(&inner, card))
                .map(|(run_id, _)| run_id.clone())
                .collect();
            (
                roots,
                std::mem::take(&mut inner.test_runs.parsed),
                inner.test_run_cards.clone(),
                retained_by_cards,
                published_versions,
                inner.test_runs.cards_enabled && inner.test_runs.cards_reevaluate,
            )
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
        let mut tracked_dirs = Vec::new();
        for (common_key, common_dir) in &common_dirs {
            for run_dir in test_runs_directories(common_dir, &mut tracked_dirs) {
                let earlier = previous.remove(&run_dir);
                // A run directory still waiting for a file is stamped before
                // its files are read: the launcher creates the directory, then
                // renames `request.json` and then `results.json` into it, so a
                // file arriving after this rescan's read changes the stamp.
                let dir_stamp = earlier
                    .as_ref()
                    .is_none_or(|disk| disk.results.is_none() || disk.read_failures > 0)
                    .then(|| test_run_dir_stamp(&run_dir));
                let request_stamp = test_run_file_stamp(&run_dir.join("request.json"));
                let results_stamp = test_run_file_stamp(&run_dir.join("results.json"));
                // An entry that kept an earlier read after a failed one is
                // read again whatever its stamps say.
                let disk = match earlier {
                    Some(disk)
                        if disk.read_failures == 0
                            && disk.request_stamp == request_stamp
                            && disk.results_stamp == results_stamp =>
                    {
                        disk
                    }
                    earlier => match TestRunDisk::read_classified(&run_dir, earlier.as_deref()) {
                        TestRunRead::Run(disk) => Arc::new(disk),
                        TestRunRead::Pending => {
                            if let Some(stamp) = dir_stamp {
                                tracked_dirs.push((run_dir.clone(), stamp));
                            }
                            continue;
                        }
                        TestRunRead::Unsupported => continue,
                    },
                };
                parsed.insert(run_dir.clone(), disk.clone());
                // Run ids are random, so a repeat means a copied directory. The
                // first in sorted order wins on every scan; nothing is logged,
                // since the rescan would repeat the line every few seconds.
                if !seen.insert(disk.run_id.clone()) {
                    continue;
                }
                let mut disk = disk;
                let mut state = disk.state(writer_may_be_alive);
                let mut read_after_check_failed = false;
                // Unknown from results read before the check that found the
                // process gone: the launcher may have written its terminal
                // results and exited in between. Read them once more, now that
                // the check is behind us, and judge from that read. A read that
                // names another responsible process (a detached run's creator
                // handing over to its worker) was itself made before that
                // process's check, so it is judged the same way again.
                for _ in 0..TEST_RUN_READS_AFTER_CHECK_MAX {
                    if !disk.unknown_needs_read_after_check(state) {
                        break;
                    }
                    let checked = disk.responsible_pid().map(|(pid, _)| pid);
                    let Some(mut fresh) = TestRunDisk::read(&run_dir, Some(disk.as_ref()))
                        .filter(|fresh| fresh.read_failures == 0)
                    else {
                        // The read after the check failed; the verdict stays
                        // this rescan's, and the next rescan reads again.
                        read_after_check_failed = true;
                        break;
                    };
                    fresh.confirmed_unknown = Some(checked);
                    state = fresh.state(writer_may_be_alive);
                    disk = Arc::new(fresh);
                    parsed.insert(run_dir.clone(), disk.clone());
                }
                // Indexed before its results were written: tracked until they
                // read, so they are seen within a tick, not at the backstop.
                if disk.results.is_none() {
                    if let Some(stamp) = dir_stamp {
                        tracked_dirs.push((run_dir.clone(), stamp));
                    }
                }
                let project_id =
                    test_run_project_id(&test_run_path_key(&disk.worktree), &projects, common_key);
                scanned.push(TestRunScanned {
                    unknown_reason: test_run_unknown_reason(&disk, state, read_after_check_failed),
                    disk,
                    state,
                    project_id,
                });
            }
        }
        scanned.sort_by(|left, right| test_run_list_order(&left.disk, &right.disk));
        // Every non-terminal run stays; terminal runs are kept newest first, a
        // bounded number per project. A run whose card has not yet stored its
        // terminal snapshot stays too, so the card gets its verdict (only a
        // card in its session's in-memory window, see above). Only the kept
        // runs get summaries.
        let mut terminal_kept: HashMap<Option<String>, usize> = HashMap::new();
        scanned.retain(|run| {
            if !run.disk.is_terminal() || retained_by_cards.contains(&run.disk.run_id) {
                return true;
            }
            let kept = terminal_kept.entry(run.project_id.clone()).or_default();
            *kept += 1;
            *kept <= TEST_RUN_TERMINAL_RUNS_PER_PROJECT
        });
        // Failure excerpts, read without the lock: only for a failed run whose
        // card was built from other results, or an uncarded one with an owner
        // whose results this index has not published yet (or every uncarded
        // one with an owner, on the scan that re-evaluates them).
        let failures: HashMap<String, TestRunCardFailureRead> = scanned
            .iter()
            .filter_map(|run| {
                let results = run.disk.results.as_ref()?;
                if results.terminal_state != Some(TestRunState::Failed) {
                    return None;
                }
                let digest = Some(results.digest.as_str());
                let needed = match card_refs.get(&run.disk.run_id) {
                    Some(card) => card.failure_detail_version.as_deref() != digest,
                    None => {
                        run.disk.owner.is_some()
                            && (reevaluate_cards
                                || published_versions
                                    .get(&run.disk.run_id)
                                    .map(Option::as_deref)
                                    != Some(digest))
                    }
                };
                needed.then(|| (run.disk.run_id.clone(), test_run_card_failure(&run.disk.run_dir)))
            })
            .collect();
        // Each delta is published under the lock that allocated its revision,
        // as every commit_delta_locked caller must: published after unlock,
        // another thread's later revision could reach clients first, which
        // they treat as a gap and answer with a full resync.
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.test_runs.parsed = parsed;
        inner.test_runs.tracked_dirs = tracked_dirs;
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
        let mut published_changed = HashSet::new();
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
                Ok(revision) => {
                    published_changed.insert(run.run_id.clone());
                    publish(&DeltaEvent::TestRunChanged { revision, run });
                }
                Err(err) => {
                    eprintln!("test runs> failed to record a changed run: {err:#}");
                    failed = true;
                    unsent_changed.insert(run.run_id);
                }
            }
        }
        let entries = if failed {
            test_run_settle_entries(entries, &before, &unsent_changed, &unsent_removed)
        } else {
            entries
        };
        // Cards follow the summaries clients were told about.
        self.sync_test_run_cards_locked(&mut inner, &entries, &published_changed, &failures, publish);
        inner.test_runs.entries = entries;
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

    /// Whether a tracked directory's modification time differs from the one
    /// the last rescan recorded: a run was created or removed in a
    /// `review-runs` directory, or a skipped run directory got its request.
    /// Stats only; the state lock is not held while they run.
    fn test_run_tracked_dirs_changed(&self) -> bool {
        let tracked = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .tracked_dirs
            .clone();
        tracked
            .iter()
            .any(|(dir, stamp)| test_run_dir_stamp(dir) != *stamp)
    }

    /// Starts the background rescan. It ticks every 2 s: while a run is
    /// `running` it rescans on every tick; otherwise it stats the tracked
    /// directories and rescans when one changed, so a new run is seen within
    /// about 2 s, with a full rescan every 10 s as the backstop. Nothing else
    /// polls. It stops once shutdown is signalled, so no delta is committed
    /// after persistence has shut down.
    #[cfg(not(test))]
    fn spawn_test_run_index(&self) {
        let state = self.clone();
        let shutdown = self.subscribe_shutdown_signal();
        let spawned = std::thread::Builder::new()
            .name("termal-test-runs".to_owned())
            .spawn(move || {
                let mut active = false;
                let mut last_rescan: Option<std::time::Instant> = None;
                let mut cards_enabled = false;
                let mut epoch_fence = None;
                while !*shutdown.borrow() {
                    // The card epoch is durable before any card is created;
                    // until then rescans publish runs but create no card. The
                    // step never waits, so the index is never held up by it.
                    if TEST_RUN_CARDS_SHIPPED && !cards_enabled {
                        cards_enabled = state.step_test_run_cards_epoch(&mut epoch_fence);
                    }
                    if test_run_rescan_due(
                        active,
                        last_rescan.map(|at| at.elapsed()),
                        || state.test_run_tracked_dirs_changed(),
                    ) {
                        active = state.refresh_test_runs();
                        last_rescan = Some(std::time::Instant::now());
                    }
                    std::thread::sleep(TEST_RUN_TICK);
                }
            });
        if let Err(err) = spawned {
            eprintln!("test runs> failed to start the run index: {err}");
        }
    }
}
