//! Tests the test-run index (docs/features/test-runs.md, slice 1): which run
//! directories are found and whose they are, the host verdict for
//! non-terminal runs, session attribution, list order and retention, the
//! deltas the rescan publishes, run detail, the stage-log tail and its
//! containment, and the wire shape. Liveness is injected so no test depends on
//! a real process's pid.
//!
//! Owns the index tests only. New module; the index is new in
//! `test_runs.rs`, `test_runs_disk.rs` and `test_runs_api.rs`.

use super::*;

struct RunFixture {
    state: AppState,
    root: PathBuf,
    project_id: String,
}

impl RunFixture {
    fn new(label: &str) -> Self {
        let state = test_app_state();
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("test-runs-{label}"));
        fs::create_dir_all(&root).expect("project root should exist");
        run_git_test_command(&root, &["init", "--quiet"]);
        let project_id = create_test_project(&state, &root, "Test runs");
        Self {
            state,
            root,
            project_id,
        }
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join(".git").join("review-runs")
    }

    fn refresh(&self, alive: &[u32]) -> bool {
        let alive = alive.to_vec();
        self.state
            .refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|event| {
                self.state.publish_delta(event)
            })
    }

    fn summaries(&self) -> Vec<TestRunSummary> {
        self.state.test_run_summaries(None)
    }

    fn summary(&self, run_id: &str) -> TestRunSummary {
        self.summaries()
            .into_iter()
            .find(|run| run.run_id == run_id)
            .unwrap_or_else(|| panic!("{run_id} should be indexed"))
    }
}

/// Writes a run directory the way the launcher lays one out.
fn write_run(runs_dir: &FsPath, run_id: &str, request: Value, results: Option<Value>) -> PathBuf {
    let run_dir = runs_dir.join(run_id);
    fs::create_dir_all(&run_dir).expect("run dir should exist");
    let mut request = request;
    request["runId"] = json!(run_id);
    fs::write(run_dir.join("request.json"), request.to_string()).expect("request should write");
    if let Some(mut results) = results {
        results["runId"] = json!(run_id);
        fs::write(run_dir.join("results.json"), results.to_string()).expect("results should write");
    }
    run_dir
}

fn full_request(root: &FsPath, started: &str) -> Value {
    json!({
        "root": root.to_string_lossy(),
        "full": true,
        "stages": [{ "name": "cargo-check" }, { "name": "rust-tests" }],
        "started": started,
    })
}

fn passed_results() -> Value {
    json!({
        "state": "passed",
        "exitCode": 0,
        "ended": "2026-09-25T10:10:00.000Z",
        "stages": [
            { "name": "cargo-check", "state": "passed", "code": 0,
              "started": "2026-09-25T10:00:05.000Z", "ended": "2026-09-25T10:01:00.000Z" },
            { "name": "rust-tests", "state": "passed", "code": 0,
              "started": "2026-09-25T10:01:00.000Z", "ended": "2026-09-25T10:10:00.000Z" },
        ],
    })
}

fn running_results(pid: Option<u32>) -> Value {
    let mut results = json!({
        "state": "running",
        "stages": [
            { "name": "cargo-check", "state": "passed", "code": 0 },
            { "name": "rust-tests", "state": "running", "started": "2026-09-25T11:00:00.000Z" },
        ],
    });
    if let Some(pid) = pid {
        results["pid"] = json!(pid);
    }
    results
}

#[test]
fn the_index_mirrors_launcher_runs_with_the_host_verdict() {
    let fixture = RunFixture::new("verdicts");
    let runs = fixture.runs_dir();
    let root = fixture.root.as_path();
    write_run(
        &runs,
        "test-passed",
        full_request(root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    write_run(
        &runs,
        "test-running",
        full_request(root, "2026-09-25T11:00:00.000Z"),
        Some(running_results(Some(222))),
    );
    write_run(
        &runs,
        "test-dead",
        full_request(root, "2026-09-25T09:00:00.000Z"),
        Some(running_results(Some(333))),
    );
    write_run(
        &runs,
        "test-no-pid",
        full_request(root, "2026-09-25T08:00:00.000Z"),
        Some(running_results(None)),
    );
    write_run(
        &runs,
        "test-no-results",
        full_request(root, "2026-09-25T07:00:00.000Z"),
        None,
    );
    let mut starting = full_request(root, "2026-09-25T12:00:00.000Z");
    starting["creatorPid"] = json!(444);
    write_run(
        &runs,
        "test-starting",
        starting,
        Some(running_results(None)),
    );
    let mut settled = passed_results();
    settled["state"] = json!("failed");
    settled["exitCode"] = json!(1);
    settled["interrupted"] = json!(true);
    settled["error"] = json!("interrupted: worker 555 exited without saving a terminal result");
    write_run(
        &runs,
        "test-interrupted",
        full_request(root, "2026-09-25T06:00:00.000Z"),
        Some(settled),
    );

    assert!(
        fixture.refresh(&[222, 444]),
        "running runs keep the fast rescan"
    );

    let order: Vec<String> = fixture
        .summaries()
        .into_iter()
        .map(|run| run.run_id)
        .collect();
    assert_eq!(
        order,
        [
            "test-starting",
            "test-running",
            "test-passed",
            "test-dead",
            "test-no-pid",
            "test-no-results",
            "test-interrupted",
        ],
        "newest first"
    );
    assert_eq!(fixture.summary("test-passed").state, TestRunState::Passed);
    let running = fixture.summary("test-running");
    assert_eq!(running.state, TestRunState::Running);
    assert_eq!(running.current_stage.as_deref(), Some("rust-tests"));
    assert_eq!(
        running.project_id.as_deref(),
        Some(fixture.project_id.as_str())
    );
    assert_eq!(running.preset, TestRunPreset::Full);
    assert_eq!(
        fixture.summary("test-starting").state,
        TestRunState::Running,
        "the creating process stands in until the worker records its pid"
    );
    for run_id in ["test-dead", "test-no-pid", "test-no-results"] {
        assert_eq!(
            fixture.summary(run_id).state,
            TestRunState::Unknown,
            "{run_id}: a missing or dead pid is never running"
        );
    }
    let interrupted = fixture.summary("test-interrupted");
    assert_eq!(interrupted.state, TestRunState::Failed);
    assert!(interrupted.interrupted);
    assert_eq!(interrupted.exit_code, Some(1));
    assert!(
        interrupted
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("interrupted:"))
    );
}

#[test]
fn a_focused_run_carries_its_bounded_command_and_detached_flag() {
    let fixture = RunFixture::new("focused");
    let long_arg = "x".repeat(600);
    write_run(
        &fixture.runs_dir(),
        "test-focused",
        json!({
            "root": fixture.root.to_string_lossy(),
            "detached": true,
            "stages": [{ "name": "focused", "command": "cargo", "args": ["test", "-q", long_arg] }],
            "started": "2026-09-25T10:00:00.000Z",
        }),
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    let run = fixture.summary("test-focused");
    assert_eq!(run.preset, TestRunPreset::Focused);
    assert_eq!(
        run.command,
        Some(vec!["cargo".to_owned(), "test".to_owned(), "-q".to_owned()])
    );
    assert!(
        run.command_truncated,
        "an argument past 512 bytes is left out"
    );
    assert_eq!(run.detached, Some(true));
    let full = RunFixture::new("predates");
    write_run(
        &full.runs_dir(),
        "test-old",
        full_request(&full.root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    full.refresh(&[]);
    assert_eq!(
        full.summary("test-old").detached,
        None,
        "a run from before the field"
    );
    assert_eq!(full.summary("test-old").command, None);
}

#[test]
fn owners_and_notification_targets_resolve_to_visible_sessions() {
    let fixture = RunFixture::new("sessions");
    let session = |name: &str| {
        let mut inner = fixture.state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some(name.to_owned()),
            fixture.root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        fixture.state.commit_locked(&mut inner).unwrap();
        session_id
    };
    let coordinator = session("Termal::Coordinator");
    let worker = session("Worker");
    let twin_a = session("Twin");
    let _twin_b = session("Twin");
    let mut by_name = full_request(&fixture.root, "2026-09-25T10:00:00.000Z");
    by_name["owner"] = json!(worker);
    by_name["notifyTo"] = json!("termal::coordinator");
    write_run(
        &fixture.runs_dir(),
        "test-by-name",
        by_name,
        Some(running_results(Some(7))),
    );
    let mut by_id = full_request(&fixture.root, "2026-09-25T10:00:01.000Z");
    by_id["owner"] = json!("session-gone");
    by_id["notifyTo"] = json!(twin_a);
    write_run(
        &fixture.runs_dir(),
        "test-by-id",
        by_id,
        Some(running_results(Some(7))),
    );
    let mut ambiguous = full_request(&fixture.root, "2026-09-25T10:00:02.000Z");
    ambiguous["notifyTo"] = json!("Twin");
    write_run(
        &fixture.runs_dir(),
        "test-ambiguous",
        ambiguous,
        Some(running_results(Some(7))),
    );
    fixture.refresh(&[7]);

    let by_name = fixture.summary("test-by-name");
    assert_eq!(by_name.owner_session_id.as_deref(), Some(worker.as_str()));
    assert_eq!(by_name.notify_to.as_deref(), Some("termal::coordinator"));
    assert_eq!(
        by_name.notify_session_id.as_deref(),
        Some(coordinator.as_str()),
        "names match without case"
    );
    let by_id = fixture.summary("test-by-id");
    assert_eq!(
        by_id.owner_session_id, None,
        "an owner that is no known session"
    );
    assert_eq!(
        by_id.notify_session_id.as_deref(),
        Some(twin_a.as_str()),
        "an id resolves even when its name is shared"
    );
    let ambiguous = fixture.summary("test-ambiguous");
    assert_eq!(ambiguous.notify_to.as_deref(), Some("Twin"));
    assert_eq!(
        ambiguous.notify_session_id, None,
        "a shared name names no one session"
    );
}

fn drain_test_run_deltas(
    receiver: &mut tokio::sync::broadcast::Receiver<String>,
) -> Vec<DeltaEvent> {
    let mut events = Vec::new();
    while let Ok(payload) = receiver.try_recv() {
        let event: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
        if matches!(
            event,
            DeltaEvent::TestRunChanged { .. } | DeltaEvent::TestRunRemoved { .. }
        ) {
            events.push(event);
        }
    }
    events
}

#[test]
fn a_detail_only_change_moves_the_detail_version() {
    // Fields only the detail shows (preflight, diagnostics, fingerprints) can
    // change while every other summary field stays equal. The detail version
    // still moves, so the change is sent, and a client that got the summary
    // from a snapshot instead of the delta can still see its detail is stale.
    let fixture = RunFixture::new("detail-version");
    let run_dir = write_run(
        &fixture.runs_dir(),
        "test-detail-only",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    write_run(
        &fixture.runs_dir(),
        "test-no-results",
        full_request(&fixture.root, "2026-09-25T09:00:00.000Z"),
        None,
    );
    fixture.refresh(&[]);
    let before = fixture.summary("test-detail-only");
    let version = before
        .detail_version
        .clone()
        .expect("results give a version");
    assert_eq!(fixture.summary("test-no-results").detail_version, None);

    let mut receiver = fixture.state.subscribe_delta_events();
    let mut results = passed_results();
    results["runId"] = json!("test-detail-only");
    results["preflight"] =
        json!([{ "name": "cargo", "command": ["cargo", "--version"], "code": 0 }]);
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    let after = fixture.summary("test-detail-only");
    assert_ne!(after.detail_version.as_deref(), Some(version.as_str()));
    assert_eq!(
        TestRunSummary {
            detail_version: None,
            ..after.clone()
        },
        TestRunSummary {
            detail_version: None,
            ..before
        },
        "only the detail version differs"
    );
    let events = drain_test_run_deltas(&mut receiver);
    assert!(
        matches!(events.as_slice(), [DeltaEvent::TestRunChanged { run, .. }] if *run == after),
        "the change is sent"
    );

    // Unchanged content keeps its version across rescans and across a host
    // restart, which indexes the same files from scratch.
    fixture.refresh(&[]);
    assert_eq!(fixture.summary("test-detail-only"), after);
    let restarted = test_app_state();
    create_test_project(&restarted, &fixture.root, "Restarted");
    restarted.refresh_test_runs_with(&|_, _| false, &|event| restarted.publish_delta(event));
    let reindexed = restarted
        .test_run_summaries(None)
        .into_iter()
        .find(|run| run.run_id == "test-detail-only")
        .expect("the restarted host indexes the run");
    assert_eq!(reindexed.detail_version, after.detail_version);
}

#[test]
fn the_rescan_publishes_only_changes_with_consecutive_revisions() {
    let fixture = RunFixture::new("deltas");
    let runs = fixture.runs_dir();
    let first = write_run(
        &runs,
        "test-a",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(9))),
    );
    write_run(
        &runs,
        "test-b",
        full_request(&fixture.root, "2026-09-25T09:00:00.000Z"),
        Some(passed_results()),
    );
    let mut receiver = fixture.state.subscribe_delta_events();
    let before = fixture.state.full_snapshot().revision;

    fixture.refresh(&[9]);
    let events = drain_test_run_deltas(&mut receiver);
    let revisions: Vec<u64> = events
        .iter()
        .map(|event| match event {
            DeltaEvent::TestRunChanged { revision, .. } => *revision,
            other => panic!("unexpected {}", serde_json::to_string(other).unwrap()),
        })
        .collect();
    assert_eq!(
        revisions,
        [before + 1, before + 2],
        "one bump per event, in list order"
    );
    assert_eq!(fixture.state.full_snapshot().revision, before + 2);
    assert_eq!(
        fixture.state.snapshot().test_runs.len(),
        2,
        "the snapshot carries the runs"
    );

    fixture.refresh(&[9]);
    assert!(
        drain_test_run_deltas(&mut receiver).is_empty(),
        "nothing changed, nothing is sent"
    );

    fs::write(first.join("results.json"), {
        let mut results = passed_results();
        results["runId"] = json!("test-a");
        results.to_string()
    })
    .expect("results should update");
    fixture.refresh(&[]);
    let events = drain_test_run_deltas(&mut receiver);
    assert_eq!(events.len(), 1);
    match &events[0] {
        DeltaEvent::TestRunChanged { run, .. } => {
            assert_eq!(run.run_id, "test-a");
            assert_eq!(run.state, TestRunState::Passed);
        }
        _ => panic!("expected testRunChanged"),
    }

    fs::remove_dir_all(&first).expect("run dir should be removed");
    fixture.refresh(&[]);
    let events = drain_test_run_deltas(&mut receiver);
    assert!(
        matches!(events.as_slice(), [DeltaEvent::TestRunRemoved { run_id, .. }] if run_id == "test-a"),
        "a removed run leaves the index"
    );
}

#[test]
fn every_delta_is_published_under_the_lock_that_allocated_its_revision() {
    // Published after the lock is released, a delta could be overtaken by
    // another thread's later revision, which clients answer with a full
    // resync. The probe records whether the publishing thread held the lock.
    let fixture = RunFixture::new("publish-under-lock");
    let runs = fixture.runs_dir();
    let first = write_run(
        &runs,
        "test-a",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(9))),
    );
    write_run(
        &runs,
        "test-b",
        full_request(&fixture.root, "2026-09-25T09:00:00.000Z"),
        Some(passed_results()),
    );
    let published = Mutex::new(Vec::new());
    let probe = |event: &DeltaEvent| {
        let revision = match event {
            DeltaEvent::TestRunChanged { revision, .. }
            | DeltaEvent::TestRunRemoved { revision, .. } => *revision,
            _ => panic!("the rescan publishes only test-run deltas"),
        };
        let held = !fixture.state.inner.is_not_held_by_current_thread_for_test();
        published.lock().unwrap().push((revision, held));
        fixture.state.publish_delta(event);
    };
    fixture
        .state
        .refresh_test_runs_with(&|pid, _| pid == 9, &probe);
    fs::remove_dir_all(&first).unwrap();
    fixture.state.refresh_test_runs_with(&|_, _| false, &probe);

    let published = published.into_inner().unwrap();
    assert_eq!(published.len(), 3, "two new runs, then one removed");
    assert!(
        published.iter().all(|(_, held)| *held),
        "every publication happens under the state lock: {published:?}"
    );
    assert!(
        published.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1),
        "{published:?}"
    );
}

#[test]
fn a_dead_worker_turns_a_running_run_unknown_by_delta() {
    let fixture = RunFixture::new("worker-exit");
    write_run(
        &fixture.runs_dir(),
        "test-live",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(11))),
    );
    fixture.refresh(&[11]);
    assert_eq!(fixture.summary("test-live").state, TestRunState::Running);
    let mut receiver = fixture.state.subscribe_delta_events();
    assert!(
        !fixture.refresh(&[]),
        "an unknown run does not hold the fast rescan"
    );
    let events = drain_test_run_deltas(&mut receiver);
    assert!(
        matches!(events.as_slice(), [DeltaEvent::TestRunChanged { run, .. }] if run.state == TestRunState::Unknown),
        "the host verdict changes without any file changing"
    );
}

#[test]
fn linked_worktree_runs_join_the_project_that_shares_the_common_directory() {
    let fixture = RunFixture::new("worktrees");
    let common = fixture.root.join(".git");
    let linked_git = common.join("worktrees").join("wt1");
    fs::create_dir_all(&linked_git).expect("linked git dir should exist");
    fs::write(linked_git.join("commondir"), "../..\n").expect("commondir should write");
    let linked_root = fixture.root.with_file_name("test-runs-worktrees-linked");
    fs::create_dir_all(&linked_root).expect("linked root should exist");
    fs::write(
        linked_root.join(".git"),
        format!("gitdir: {}\n", linked_git.display()),
    )
    .expect(".git file should write");
    write_run(
        &linked_git.join("review-runs"),
        "test-linked",
        full_request(&linked_root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    fixture.refresh(&[]);
    assert_eq!(
        fixture.summary("test-linked").project_id.as_deref(),
        Some(fixture.project_id.as_str()),
        "the only project sharing the common directory"
    );

    let linked_project = create_test_project(&fixture.state, &linked_root, "Linked");
    fixture.refresh(&[]);
    let runs = fixture.summaries();
    assert_eq!(
        runs.len(),
        1,
        "a common directory two projects share is indexed once"
    );
    assert_eq!(
        runs[0].project_id.as_deref(),
        Some(linked_project.as_str()),
        "the project rooted at the run's worktree"
    );
}

#[test]
fn terminal_runs_are_bounded_per_project_and_non_terminal_runs_always_stay() {
    let fixture = RunFixture::new("retention");
    let runs = fixture.runs_dir();
    for index in 0..52 {
        write_run(
            &runs,
            &format!("test-done-{index:02}"),
            full_request(&fixture.root, &format!("2026-09-25T10:{index:02}:00.000Z")),
            Some(passed_results()),
        );
    }
    write_run(
        &runs,
        "test-oldest-running",
        full_request(&fixture.root, "2026-09-01T00:00:00.000Z"),
        Some(running_results(Some(5))),
    );
    fixture.refresh(&[5]);
    let indexed: Vec<String> = fixture
        .summaries()
        .into_iter()
        .map(|run| run.run_id)
        .collect();
    assert_eq!(indexed.len(), 51);
    assert!(indexed.contains(&"test-oldest-running".to_owned()));
    assert!(
        !indexed.contains(&"test-done-00".to_owned()),
        "the oldest terminal runs age out"
    );
    assert!(!indexed.contains(&"test-done-01".to_owned()));
    assert!(indexed.contains(&"test-done-51".to_owned()));

    // Aged-out runs stay in the parse cache, so an unchanged run directory,
    // listed or not, is never parsed again.
    let parsed = |fixture: &RunFixture| {
        fixture
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .parsed
            .clone()
    };
    let first = parsed(&fixture);
    assert_eq!(first.len(), 53, "every run directory is cached");
    fixture.refresh(&[5]);
    let second = parsed(&fixture);
    assert_eq!(second.len(), 53);
    for (run_dir, disk) in &first {
        assert!(
            Arc::ptr_eq(disk, &second[run_dir]),
            "{} was parsed again",
            run_dir.display()
        );
    }

    // An aged-out run keeps its bounded extract, so when it comes back into
    // the list its summary is whole without reading anything again.
    let aged_out = runs.join("test-done-01");
    assert!(second[&aged_out].is_terminal());
    fs::remove_dir_all(runs.join("test-done-51")).unwrap();
    fixture.refresh(&[5]);
    let returned = fixture.summary("test-done-01");
    assert_eq!(returned.state, TestRunState::Passed);
    assert_eq!(returned.stages.len(), 2);
    assert!(returned.detail_version.is_some());
    assert!(
        Arc::ptr_eq(&second[&aged_out], &parsed(&fixture)[&aged_out]),
        "coming back into the list needs no new read"
    );
}

#[test]
fn the_index_keeps_a_bounded_extract_of_results_not_the_file() {
    // Memory per indexed run is bounded whatever results.json holds: the
    // index keeps summary fields only, each within its limit, and the detail
    // route reads the file afresh. Nothing that names something is ever cut:
    // an over-long stage name is left out, an over-long timestamp reads as
    // null, and only the display-only error text is shortened.
    let fixture = RunFixture::new("bounded-extract");
    let long = "n".repeat(2_000);
    let mut stages: Vec<Value> = (0..100)
        .map(|index| {
            json!({ "name": format!("stage-{index}"), "state": "passed", "code": 0,
                    "started": long, "ended": "2026-09-25T10:00:00.000Z" })
        })
        .collect();
    // Not a launcher stage name: the log route could never look it up.
    stages.insert(0, json!({ "name": format!("{long}"), "state": "passed" }));
    stages.insert(1, json!({ "name": "has space", "state": "passed" }));
    write_run(
        &fixture.runs_dir(),
        "test-bounded",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(json!({
            "state": "failed", "exitCode": 1, "ended": long, "error": long,
            "stages": stages, "diagnostics": long,
        })),
    );
    fixture.refresh(&[]);
    let run = fixture.summary("test-bounded");
    assert_eq!(run.state, TestRunState::Failed);
    assert_eq!(run.stages.len(), 64, "at most 64 stages are kept");
    assert_eq!(
        run.stages[0].name, "stage-0",
        "invalid names are left out, not cut"
    );
    assert!(run.stages.iter().all(|stage| stage.started_at.is_none()
        && stage.ended_at.as_deref() == Some("2026-09-25T10:00:00.000Z")));
    assert_eq!(run.ended_at, None, "an over-long timestamp reads as null");
    assert_eq!(run.error.as_ref().map(String::len), Some(512));
    // The detail still reads every stage from the file.
    let (_, dir) = fixture.state.test_run_entry("test-bounded").unwrap();
    assert_eq!(test_run_detail(&dir).unwrap().stages.len(), 102);
}

#[test]
fn the_current_stage_is_found_past_the_kept_stages() {
    let fixture = RunFixture::new("current-stage-past-cap");
    let stages: Vec<Value> = (0..70)
        .map(|index| {
            let state = match index {
                ..66 => "passed",
                66 => "running",
                _ => "unrun",
            };
            json!({ "name": format!("stage-{index}"), "state": state })
        })
        .collect();
    write_run(
        &fixture.runs_dir(),
        "test-long-plan",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(json!({ "state": "running", "pid": 8, "stages": stages })),
    );
    fixture.refresh(&[8]);
    let run = fixture.summary("test-long-plan");
    assert_eq!(run.stages.len(), 64);
    assert_eq!(run.current_stage.as_deref(), Some("stage-66"));
}

#[test]
fn over_long_identifiers_make_a_run_unsupported_rather_than_aliased() {
    // A cut run id, worktree or session reference could name another run,
    // tree or session, so such a run is not indexed at all.
    let fixture = RunFixture::new("over-long-identifiers");
    let runs = fixture.runs_dir();
    let long = "x".repeat(129);
    let request = |key: &str, value: Value| {
        let mut request = full_request(&fixture.root, "2026-09-25T10:00:00.000Z");
        request[key] = value;
        request
    };
    write_run(
        &runs,
        "test-owner",
        request("owner", json!(long)),
        Some(passed_results()),
    );
    write_run(
        &runs,
        "test-notify",
        request("notifyTo", json!(long)),
        Some(passed_results()),
    );
    write_run(
        &runs,
        "test-root",
        request("root", json!(format!("C:/{}", "d".repeat(4096)))),
        Some(passed_results()),
    );
    write_run(
        &runs,
        "test-fine",
        request("owner", json!("x".repeat(128))),
        Some(passed_results()),
    );
    // A run id over the limit, written directly: write_run sets its own.
    let dir = runs.join("test-long-id");
    fs::create_dir_all(&dir).unwrap();
    let mut long_id = full_request(&fixture.root, "2026-09-25T10:00:00.000Z");
    long_id["runId"] = json!(format!("test-{long}"));
    fs::write(dir.join("request.json"), long_id.to_string()).unwrap();
    fixture.refresh(&[]);
    let ids: Vec<String> = fixture
        .summaries()
        .into_iter()
        .map(|run| run.run_id)
        .collect();
    assert_eq!(
        ids,
        ["test-fine"],
        "only the run within every limit is indexed"
    );
}

#[test]
fn oversized_results_are_unknown_at_once_and_refused_as_not_retryable() {
    let fixture = RunFixture::new("oversized");
    let run_dir = write_run(
        &fixture.runs_dir(),
        "test-oversized",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    let mut results = passed_results();
    results["runId"] = json!("test-oversized");
    results["padding"] = json!("x".repeat(1024 * 1024 + 1));
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.refresh(&[]);
    // No grace: an oversized file is not a race.
    let run = fixture.summary("test-oversized");
    assert_eq!(run.state, TestRunState::Unknown);
    assert!(run.stages.is_empty());
    assert_eq!(run.detail_version, None);
    let (_, dir) = fixture.state.test_run_entry("test-oversized").unwrap();
    assert_eq!(
        test_run_detail(&dir).unwrap_err().status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        test_run_stage_log_tail(&dir, "rust-tests", None)
            .unwrap_err()
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[test]
fn unsent_deltas_leave_the_previous_entries_in_the_index() {
    // When a commit fails mid-rescan, runs whose delta was not sent keep their
    // previous entry, so the index claims nothing clients did not receive.
    let fixture = RunFixture::new("settle");
    let runs = fixture.runs_dir();
    for (run_id, started) in [
        ("test-a", "2026-09-25T10:00:00.000Z"),
        ("test-b", "2026-09-25T09:00:00.000Z"),
        ("test-c", "2026-09-25T08:00:00.000Z"),
    ] {
        write_run(
            &runs,
            run_id,
            full_request(&fixture.root, started),
            Some(running_results(Some(3))),
        );
    }
    fixture.refresh(&[3]);
    let entries = |fixture: &RunFixture| {
        fixture
            .state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .entries
            .clone()
    };
    let before: HashMap<String, TestRunEntry> = entries(&fixture)
        .into_iter()
        .map(|entry| (entry.summary.run_id.clone(), entry))
        .collect();
    // b finishes, c is removed, d is new.
    let mut passed = passed_results();
    passed["runId"] = json!("test-b");
    fs::write(runs.join("test-b").join("results.json"), passed.to_string()).unwrap();
    fs::remove_dir_all(runs.join("test-c")).unwrap();
    write_run(
        &runs,
        "test-d",
        full_request(&fixture.root, "2026-09-25T11:00:00.000Z"),
        Some(running_results(Some(3))),
    );
    fixture.refresh(&[3]);
    let after = entries(&fixture);

    let settled = test_run_settle_entries(
        after,
        &before,
        &HashSet::from(["test-b".to_owned(), "test-d".to_owned()]),
        &["test-c".to_owned()],
    );
    let ids: Vec<&str> = settled
        .iter()
        .map(|entry| entry.summary.run_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["test-a", "test-b", "test-c"],
        "d unsent: still absent"
    );
    assert_eq!(
        settled[1].summary, before["test-b"].summary,
        "b keeps its previous entry"
    );
    assert_eq!(
        settled[2].summary, before["test-c"].summary,
        "c stays listed"
    );
}

#[test]
fn a_duplicate_run_id_keeps_the_first_directory() {
    let fixture = RunFixture::new("duplicate");
    let runs = fixture.runs_dir();
    write_run(
        &runs,
        "test-dup-a",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(passed_results()),
    );
    let second = write_run(
        &runs,
        "test-dup-b",
        full_request(&fixture.root, "2026-09-25T11:00:00.000Z"),
        Some(passed_results()),
    );
    let mut request: Value =
        serde_json::from_str(&fs::read_to_string(second.join("request.json")).unwrap()).unwrap();
    request["runId"] = json!("test-dup-a");
    fs::write(second.join("request.json"), request.to_string()).unwrap();
    fixture.refresh(&[]);
    let runs = fixture.summaries();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].run_dir.ends_with("/test-dup-a"),
        "{}",
        runs[0].run_dir
    );
}

#[test]
fn run_detail_carries_stages_preflight_and_relative_logs() {
    let fixture = RunFixture::new("detail");
    let run_dir = fixture.runs_dir().join("test-detail");
    let log = run_dir.join("rust-tests.log");
    write_run(
        &fixture.runs_dir(),
        "test-detail",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(json!({
            "state": "failed",
            "exitCode": 101,
            "ended": "2026-09-25T10:10:00.000Z",
            "expectedFingerprint": "abc",
            "before": "abc",
            "after": "abc",
            "limitations": "Windows: untracked executable modes are unverified.",
            "preflight": [
                { "name": "cargo", "command": ["cargo", "--version"], "code": 0,
                  "log": run_dir.join("preflight-cargo.log").to_string_lossy() },
                // The launcher spells the directory as Git reports it, which
                // may differ in case from the index's spelling.
                { "name": "shell", "code": 0,
                  "log": run_dir.join("preflight-shell.log").to_string_lossy().to_uppercase() },
            ],
            "stages": [
                { "name": "cargo-check", "state": "passed", "code": 0 },
                { "name": "rust-tests", "state": "failed", "code": 101,
                  "command": ["sh", "scripts/test-rust.sh"], "cwd": fixture.root.to_string_lossy(),
                  "log": log.to_string_lossy(),
                  "diagnostics": { "text": "thread 'x' panicked", "truncated": false } },
            ],
        })),
    );
    fixture.refresh(&[]);
    let (_, dir) = fixture
        .state
        .test_run_entry("test-detail")
        .expect("indexed");
    let detail = test_run_detail(&dir).expect("readable results give a detail");
    assert_eq!(detail.stages.len(), 2);
    let failed = &detail.stages[1];
    assert_eq!(failed.summary.state, TestRunStageState::Failed);
    assert_eq!(failed.summary.exit_code, Some(101));
    assert_eq!(
        failed.log.as_deref(),
        Some("rust-tests.log"),
        "relative to the run directory"
    );
    assert_eq!(
        failed.command,
        Some(vec!["sh".to_owned(), "scripts/test-rust.sh".to_owned()])
    );
    assert_eq!(
        failed.diagnostics,
        Some(TestRunDiagnostics {
            text: "thread 'x' panicked".to_owned(),
            truncated: false
        })
    );
    assert_eq!(
        failed.cwd.as_deref(),
        Some(test_run_display_path(&fixture.root).as_str()),
        "forward slashes"
    );
    assert_eq!(
        detail.preflight[0].log.as_deref(),
        Some("preflight-cargo.log")
    );
    assert_eq!(detail.expected_fingerprint.as_deref(), Some("abc"));
    if cfg!(windows) {
        fs::write(run_dir.join("preflight-shell.log"), "ok").unwrap();
        let detail = test_run_detail(&dir).expect("readable results give a detail");
        assert_eq!(
            detail.preflight[1].log.as_deref(),
            Some("preflight-shell.log"),
            "a case-only difference is resolved on canonical paths"
        );
    }

    // Results that exist but cannot be read are refused rather than shown as
    // a detail with no stages; results not yet written give an empty detail.
    let results = run_dir.join("results.json");
    let good = fs::read(&results).unwrap();
    fs::write(&results, "{not json").unwrap();
    assert_eq!(
        test_run_detail(&dir).unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        test_run_stage_log_tail(&dir, "rust-tests", None)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    fs::remove_file(&results).unwrap();
    assert_eq!(
        test_run_detail(&dir).expect("no results yet is an empty detail"),
        TestRunDetail::default()
    );
    fs::write(&results, good).unwrap();
}

#[test]
fn the_log_tail_is_bounded_cut_at_a_character_and_confined_to_the_run() {
    let fixture = RunFixture::new("log-tail");
    let run_dir = fixture.runs_dir().join("test-log");
    fs::create_dir_all(&run_dir).unwrap();
    let log = run_dir.join("rust-tests.log");
    let outside = fixture.runs_dir().join("outside.log");
    fs::write(&outside, "secret").unwrap();
    write_run(
        &fixture.runs_dir(),
        "test-log",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(json!({
            "state": "running",
            "pid": 1,
            "stages": [
                { "name": "rust-tests", "state": "running", "log": log.to_string_lossy() },
                { "name": "escape", "state": "unrun", "log": run_dir.join("..").join("outside.log").to_string_lossy() },
                { "name": "no-log", "state": "unrun" },
            ],
        })),
    );
    // "ab" then "é" (two bytes) then eight more bytes: a nine-byte tail
    // starts inside the "é".
    fs::write(&log, "abé12345678").unwrap();

    let whole = test_run_stage_log_tail(&run_dir, "rust-tests", None).expect("tail");
    assert_eq!(whole.text, "abé12345678");
    assert!(!whole.truncated);
    assert_eq!(whole.size, 12);
    let cut = test_run_stage_log_tail(&run_dir, "rust-tests", Some(9)).expect("tail");
    assert_eq!(
        cut.text, "12345678",
        "the cut moves forward past the half character"
    );
    assert!(cut.truncated);

    let status = |name: &str| {
        test_run_stage_log_tail(&run_dir, name, None)
            .unwrap_err()
            .status
    };
    assert_eq!(status("../x"), StatusCode::BAD_REQUEST);
    assert_eq!(
        status("escape"),
        StatusCode::BAD_REQUEST,
        "a log outside the run directory"
    );
    assert_eq!(status("missing"), StatusCode::NOT_FOUND);
    assert_eq!(status("no-log"), StatusCode::NOT_FOUND);
}

#[test]
fn the_wire_shape_is_the_contracts() {
    let fixture = RunFixture::new("wire");
    write_run(
        &fixture.runs_dir(),
        "test-wire",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(3))),
    );
    fixture.refresh(&[3]);
    let run = serde_json::to_value(fixture.summary("test-wire")).unwrap();
    for key in [
        "runId",
        "projectId",
        "worktree",
        "runDir",
        "preset",
        "command",
        "commandTruncated",
        "detached",
        "state",
        "interrupted",
        "currentStage",
        "stages",
        "ownerSessionId",
        "notifyTo",
        "notifySessionId",
        "startedAt",
        "endedAt",
        "exitCode",
        "error",
        "detailVersion",
    ] {
        assert!(
            run.get(key).is_some(),
            "{key} is always present, null when unknown: {run}"
        );
    }
    assert!(run["detailVersion"].is_string(), "{run}");
    assert_eq!(run["state"], "running");
    assert_eq!(run["preset"], "full");
    assert_eq!(run["notifySessionId"], Value::Null);
    assert_eq!(
        run["stages"][1],
        json!({
            "name": "rust-tests", "state": "running", "exitCode": null,
            "startedAt": "2026-09-25T11:00:00.000Z", "endedAt": null,
        })
    );
    assert!(
        !run["runDir"].as_str().unwrap().contains('\\'),
        "forward slashes"
    );
    let snapshot = serde_json::to_value(fixture.state.snapshot()).unwrap();
    assert_eq!(snapshot["testRuns"][0]["runId"], "test-wire");

    let changed = serde_json::to_value(DeltaEvent::TestRunChanged {
        revision: 7,
        run: fixture.summary("test-wire"),
    })
    .unwrap();
    assert_eq!(changed["type"], "testRunChanged");
    assert_eq!(changed["revision"], 7);
    assert_eq!(changed["run"]["runId"], "test-wire");
    let removed = serde_json::to_value(DeltaEvent::TestRunRemoved {
        revision: 8,
        run_id: "test-wire".to_owned(),
    })
    .unwrap();
    assert_eq!(
        removed,
        json!({ "type": "testRunRemoved", "revision": 8, "runId": "test-wire" })
    );
}

#[tokio::test]
async fn the_routes_serve_the_list_the_detail_and_the_log_tail() {
    let fixture = RunFixture::new("routes");
    let other = RunFixture::new("routes-other");
    let run_dir = fixture.runs_dir().join("test-route");
    let log = run_dir.join("rust-tests.log");
    write_run(
        &fixture.runs_dir(),
        "test-route",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(json!({
            "state": "running",
            "pid": 21,
            "stages": [{ "name": "rust-tests", "state": "running", "log": log.to_string_lossy() }],
        })),
    );
    fs::write(&log, "0123456789").unwrap();
    fixture.refresh(&[21]);
    let app = app_router(fixture.state.clone());
    let get = |uri: String| Request::builder().uri(uri).body(Body::empty()).unwrap();

    let (status, list): (StatusCode, Value) =
        request_json(&app, get("/api/test-runs".to_owned())).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["runs"][0]["runId"], "test-route");
    let (_, scoped): (StatusCode, Value) = request_json(
        &app,
        get(format!("/api/test-runs?projectId={}", fixture.project_id)),
    )
    .await;
    assert_eq!(scoped["runs"].as_array().unwrap().len(), 1);
    let (_, foreign): (StatusCode, Value) = request_json(
        &app,
        get(format!("/api/test-runs?projectId={}", other.project_id)),
    )
    .await;
    assert_eq!(
        foreign["runs"],
        json!([]),
        "another project's filter hides the run"
    );

    let (status, detail): (StatusCode, Value) =
        request_json(&app, get("/api/test-runs/test-route".to_owned())).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["run"]["state"], "running");
    assert_eq!(detail["detail"]["stages"][0]["log"], "rust-tests.log");

    let (status, tail): (StatusCode, Value) = request_json(
        &app,
        get("/api/test-runs/test-route/stages/rust-tests/log?tail=4".to_owned()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tail}");
    assert_eq!(
        tail,
        json!({ "text": "6789", "truncated": true, "size": 10 })
    );

    for (uri, expected) in [
        ("/api/test-runs/test-missing", StatusCode::NOT_FOUND),
        (
            "/api/test-runs/test-missing/stages/rust-tests/log",
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/test-runs/test-route/stages/unknown/log",
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/test-runs/test-route/stages/rust-tests/log?tail=-1",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, body): (StatusCode, Value) = request_json(&app, get(uri.to_owned())).await;
        assert_eq!(status, expected, "{uri}: {body}");
    }
}

#[test]
fn a_live_process_may_be_alive() {
    assert!(test_run_process_may_be_alive(std::process::id()));
}

#[test]
fn an_exited_process_is_not_alive() {
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        // No console window: a test must not flash one on the desktop.
        std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("a short process should start")
    };
    #[cfg(not(windows))]
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("a short process should start");
    let pid = child.id();
    assert!(child.wait().expect("the process should exit").success());
    // Windows keeps the exited process object while `child` holds its
    // handle, and reports its exit code; elsewhere the pid is reaped.
    assert!(!test_run_process_may_be_alive(pid));
}

#[test]
fn a_result_is_terminal_exactly_when_the_launcher_says_so() {
    let base = json!({ "state": "passed", "ended": "2026-09-25T10:00:00.000Z", "exitCode": 0 });
    let with = |key: &str, value: Value| {
        let mut results = base.clone();
        results[key] = value;
        results
    };
    assert!(test_run_results_are_terminal(&base));
    // The launcher's isTerminal: Boolean(ended) and Number.isInteger(exitCode).
    for terminal in [
        with("state", json!("failed")),
        with("ended", json!(true)),
        with("ended", json!(1)),
        with("exitCode", json!(1.0)),
        with("exitCode", json!(-1)),
    ] {
        assert!(test_run_results_are_terminal(&terminal), "{terminal}");
    }
    let mut no_ended = base.clone();
    no_ended.as_object_mut().unwrap().remove("ended");
    for open in [
        no_ended,
        with("state", json!("running")),
        with("ended", json!("")),
        with("ended", json!(false)),
        with("ended", json!(0)),
        with("ended", Value::Null),
        with("exitCode", json!(1.5)),
        with("exitCode", json!("0")),
        with("exitCode", Value::Null),
    ] {
        assert!(!test_run_results_are_terminal(&open), "{open}");
    }
}

/// How a test makes a run file unreadable, and how it puts it back.
#[derive(Clone, Copy, Debug)]
enum RunFileFault {
    /// Replaced by a directory: the read fails but the name exists.
    Directory,
    /// Content that is not JSON.
    Malformed,
    /// Gone while its run directory is still listed.
    Missing,
}

impl RunFileFault {
    fn apply(self, path: &FsPath) {
        match self {
            Self::Directory => {
                fs::remove_file(path).unwrap();
                fs::create_dir(path).unwrap();
            }
            Self::Malformed => fs::write(path, "{not json").unwrap(),
            Self::Missing => fs::remove_file(path).unwrap(),
        }
    }

    fn restore(self, path: &FsPath, good: &[u8]) {
        if let Self::Directory = self {
            fs::remove_dir(path).unwrap();
        }
        fs::write(path, good).unwrap();
    }
}

#[test]
fn a_failed_read_gets_one_rescan_of_grace_and_then_the_run_is_unknown_or_dropped() {
    for fault in [
        RunFileFault::Directory,
        RunFileFault::Malformed,
        RunFileFault::Missing,
    ] {
        for file in ["results.json", "request.json"] {
            let label = format!("fault-{fault:?}-{file}").replace('.', "-");
            let fixture = RunFixture::new(&label);
            let run_dir = write_run(
                &fixture.runs_dir(),
                "test-fault",
                full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
                Some(running_results(Some(4))),
            );
            fixture.refresh(&[4]);
            let before = fixture.summary("test-fault");
            assert_eq!(before.state, TestRunState::Running, "{label}");
            let path = run_dir.join(file);
            let good = fs::read(&path).unwrap();
            let mut receiver = fixture.state.subscribe_delta_events();

            // The first failure is taken as a read racing a replacement:
            // nothing changes and nothing is sent.
            fault.apply(&path);
            fixture.refresh(&[4]);
            assert!(
                drain_test_run_deltas(&mut receiver).is_empty(),
                "{label}: a single failed read is absorbed"
            );
            assert_eq!(fixture.summary("test-fault"), before, "{label}");

            // The grace is one rescan: failing again, the stale evidence is
            // given up.
            fixture.refresh(&[4]);
            let events = drain_test_run_deltas(&mut receiver);
            if file == "results.json" {
                assert!(
                    matches!(
                        events.as_slice(),
                        [DeltaEvent::TestRunChanged { run, .. }]
                            if run.state == TestRunState::Unknown && run.stages.is_empty()
                    ),
                    "{label}: unreadable results leave the run unknown"
                );
            } else {
                assert!(
                    matches!(
                        events.as_slice(),
                        [DeltaEvent::TestRunRemoved { run_id, .. }] if run_id == "test-fault"
                    ),
                    "{label}: an unreadable request drops the run"
                );
            }

            // The failure persisting changes nothing more, but every rescan
            // still reads the files again rather than caching the failure.
            fixture.refresh(&[4]);
            assert!(
                drain_test_run_deltas(&mut receiver).is_empty(),
                "{label}: no churn"
            );
            let cached_failures = fixture
                .state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .test_runs
                .parsed
                .get(&run_dir)
                .map(|disk| disk.read_failures);
            if file == "results.json" {
                assert!(
                    cached_failures.is_some_and(|failures| failures > 0),
                    "{label}"
                );
            } else {
                assert_eq!(
                    cached_failures, None,
                    "{label}: a dropped run is not cached"
                );
            }

            // Readable again, the run reads as before.
            fault.restore(&path, &good);
            fixture.refresh(&[4]);
            assert_eq!(fixture.summary("test-fault"), before, "{label}");
        }
    }
}

#[test]
fn remote_test_run_deltas_are_consumed_without_a_local_record() {
    let state = test_app_state();
    let remote = super::remote_delta_replay::local_replay_test_remote();
    let run: TestRunSummary = serde_json::from_value(json!({
        "runId": "test-remote", "projectId": null, "worktree": "/remote/repo",
        "runDir": "/remote/repo/.git/review-runs/test-remote", "preset": "full",
        "command": null, "commandTruncated": false, "detached": false, "state": "running",
        "interrupted": false, "currentStage": null, "stages": [], "ownerSessionId": null,
        "notifyTo": null, "notifySessionId": null, "startedAt": null, "endedAt": null,
        "exitCode": null, "error": null, "detailVersion": null,
    }))
    .expect("a remote summary should decode");
    for event in [
        DeltaEvent::TestRunChanged { revision: 7, run },
        DeltaEvent::TestRunRemoved {
            revision: 8,
            run_id: "test-remote".to_owned(),
        },
    ] {
        assert!(
            AppState::remote_delta_replay_key(&remote.id, &event).is_none(),
            "remote test-run deltas never enter replay-key suppression"
        );
        state
            .apply_remote_delta_event(&remote.id, event)
            .expect("a remote test-run delta is consumed as a no-op");
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.test_runs.entries.is_empty(),
        "a remote's runs are not this host's"
    );
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&8));
}

#[test]
fn a_process_created_after_its_pid_was_recorded_is_not_the_runs_process() {
    let written = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let alive = |_: u32| true;
    let created_after = |seconds: u64| move |_: u32| Some(written + Duration::from_secs(seconds));
    let created_before = |_: u32| Some(written - Duration::from_secs(3_600));
    assert!(
        test_run_writer_may_be_alive(7, Some(written), &alive, &created_before),
        "created before the write: may be the writer"
    );
    assert!(
        test_run_writer_may_be_alive(7, Some(written), &alive, &created_after(1)),
        "within the clock margin: may be the writer"
    );
    assert!(
        !test_run_writer_may_be_alive(7, Some(written), &alive, &created_after(3)),
        "created after the write: a reused pid"
    );
    assert!(
        test_run_writer_may_be_alive(7, None, &alive, &created_after(3_600)),
        "an unknown write time proves nothing"
    );
    assert!(
        test_run_writer_may_be_alive(7, Some(written), &alive, &|_| None),
        "an unknown creation time proves nothing"
    );
    assert!(
        !test_run_writer_may_be_alive(7, Some(written), &|_| false, &created_before),
        "a dead process is not the writer"
    );
}

#[test]
fn a_live_process_has_a_creation_time_no_later_than_now() {
    let created = test_run_process_created_at(std::process::id())
        .expect("this process's creation time should be readable");
    assert!(created <= std::time::SystemTime::now());
}

#[test]
fn a_reused_pid_reads_unknown_while_the_real_writer_reads_running() {
    // The current test process stands in for both cases. A run whose files
    // were written after it started may be its run; a run whose files were
    // last written before it existed only shares the number, like a stopped
    // run whose pid the system later gave to another program.
    let fixture = RunFixture::new("pid-reuse");
    let runs = fixture.runs_dir();
    let me = std::process::id();
    let long_ago = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let backdate = |path: PathBuf| {
        fs::File::options()
            .write(true)
            .open(path)
            .expect("file should open")
            .set_modified(long_ago)
            .expect("modification time should be set");
    };
    write_run(
        &runs,
        "test-live-writer",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(Some(me))),
    );
    let reused = write_run(
        &runs,
        "test-reused",
        full_request(&fixture.root, "2026-09-25T09:00:00.000Z"),
        Some(running_results(Some(me))),
    );
    backdate(reused.join("results.json"));
    // With no pid in results.json, request.json's creatorPid stands in.
    let mut request = full_request(&fixture.root, "2026-09-25T08:00:00.000Z");
    request["creatorPid"] = json!(me);
    let reused_creator = write_run(
        &runs,
        "test-reused-creator",
        request,
        Some(running_results(None)),
    );
    backdate(reused_creator.join("request.json"));

    fixture
        .state
        .refresh_test_runs_with(&test_run_process_writer_may_be_alive, &|event| {
            fixture.state.publish_delta(event)
        });
    assert_eq!(
        fixture.summary("test-live-writer").state,
        TestRunState::Running
    );
    assert_eq!(fixture.summary("test-reused").state, TestRunState::Unknown);
    assert_eq!(
        fixture.summary("test-reused-creator").state,
        TestRunState::Unknown
    );
}

#[test]
fn a_pid_is_judged_against_the_results_version_it_was_read_from() {
    // The launcher writes results without a pid before it captures the input
    // fingerprint, and a detached worker later replaces them with its own pid.
    // A read stamped before that replacement must not judge the worker's pid
    // against the older version's modification time: the worker was created
    // after that version, so it would read as a reused pid.
    let fixture = RunFixture::new("pid-version");
    let me = std::process::id();
    let run_dir = write_run(
        &fixture.runs_dir(),
        "test-replaced",
        full_request(&fixture.root, "2026-09-25T10:00:00.000Z"),
        Some(running_results(None)),
    );
    let results_path = run_dir.join("results.json");
    // The pid-less version was written long before this process existed.
    fs::File::options()
        .write(true)
        .open(&results_path)
        .expect("results should open")
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000))
        .expect("modification time should be set");
    let replace_with_worker_results = || {
        let mut results = running_results(Some(me));
        results["runId"] = json!("test-replaced");
        let staged = run_dir.join("results.json.tmp");
        fs::write(&staged, results.to_string()).expect("staged results should write");
        fs::rename(&staged, &results_path).expect("results should be replaced");
    };

    let disk = TestRunDisk::read_pausing(&run_dir, None, &replace_with_worker_results)
        .expect("the run should read");

    assert_eq!(
        disk.results.as_ref().and_then(|results| results.pid),
        Some(me),
        "the read saw the worker's version"
    );
    assert_eq!(
        disk.state(&test_run_process_writer_may_be_alive),
        TestRunState::Running,
        "the worker's pid is judged against its own version's write"
    );
}

#[test]
fn a_write_time_too_late_for_the_margin_proves_nothing() {
    // A file's modification time can be set to anything. The latest time this
    // platform can represent leaves no room for the margin, which must neither
    // panic nor turn the run unknown.
    let representable = |seconds: u64| {
        std::time::UNIX_EPOCH
            .checked_add(Duration::from_secs(seconds))
            .is_some()
    };
    let (mut low, mut high) = (0_u64, u64::MAX);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if representable(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    let latest = std::time::UNIX_EPOCH + Duration::from_secs(low);
    assert!(
        latest.checked_add(TEST_RUN_PID_REUSE_MARGIN).is_none(),
        "the fixture needs a write time with no room for the margin"
    );
    let created = |_: u32| Some(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    assert!(test_run_writer_may_be_alive(
        7,
        Some(latest),
        &|_| true,
        &created
    ));
}
