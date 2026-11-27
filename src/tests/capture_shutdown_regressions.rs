use super::*;
use std::sync::atomic::Ordering;

fn lifetime_fixture() -> (
    Arc<CaptureLifetime>,
    std::path::PathBuf,
    mpsc::Receiver<std::result::Result<(), String>>,
) {
    let root = Arc::new(TestTempRoot::create("termal-capture-shutdown"));
    let directory = root.path().to_owned();
    let cleaned = root.observe_cleanup();
    (
        Arc::new(CaptureLifetime::with_test_root(Some(root))),
        directory,
        cleaned,
    )
}

#[test]
fn concurrent_shutdown_waiters_both_report_pending_cleanup() {
    let (lifetime, _root, cleaned) = lifetime_fixture();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    *lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
        started_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .unwrap();
    }));
    lifetime.spawn("two-waiters-stalled-read", || {}).unwrap();
    started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let seen = Arc::new(Mutex::new(HashSet::new()));
    *lifetime.before_shutdown_wait.lock().unwrap() = Some(Arc::new(move || {
        if seen.lock().unwrap().insert(std::thread::current().id()) {
            waiting_tx.send(()).unwrap();
        }
    }));
    let (result_tx, result_rx) = mpsc::channel();
    let waiters: Vec<_> = (0..2)
        .map(|_| {
            let owner = lifetime.clone();
            let result_tx = result_tx.clone();
            std::thread::spawn(move || result_tx.send(owner.stop_and_join()).unwrap())
        })
        .collect();
    for _ in 0..2 {
        waiting_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    }
    // Taking the registry proves both waiters released it into their condvar
    // wait. Expiry is injected only now, with both initial draining checks past.
    {
        let _registry = lifetime.state.lock().unwrap();
        lifetime.expire_shutdown.store(true, Ordering::Release);
    }
    lifetime.changed.notify_all();
    let results = [
        result_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(),
        result_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(),
    ];
    release_tx.send(()).unwrap();
    for waiter in waiters {
        waiter.join().unwrap();
    }
    drop(lifetime);
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert_eq!(
        results,
        [false, false],
        "neither waiter may claim pending cleanup completed"
    );
}

#[test]
fn shutdown_fences_observation_installs_before_cancelling_a_capture() {
    let (lifetime, _root, cleaned) = lifetime_fixture();
    let observation = Arc::new(EngramRuntimeObservationLifetime::default());
    let installing = observation.clone();
    let federation = FederationExecutorWakeSignal::default();
    let (shutdown, _) = tokio::sync::watch::channel(false);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (fenced_tx, fenced_rx) = mpsc::channel();
    lifetime
        .spawn("stop-order-control", move || {
            let _registration = register_capture_child(move || {
                let fenced = installing.enter("shutdown-cancel-control").is_none();
                fenced_tx.send(fenced).unwrap();
                release_tx.send(()).unwrap();
            });
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
        })
        .unwrap();
    started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    retire_capture_observation_workers(&lifetime, &observation, &federation, &shutdown);
    let fenced = fenced_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    drop(lifetime);
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert!(
        fenced,
        "a cancelled capture must not enter a persisted failure install"
    );
}

#[cfg(unix)]
struct UnresolvedCaptureGroup(Arc<SharedChild>);

#[cfg(unix)]
impl BoundedReadGroupIo for UnresolvedCaptureGroup {
    type Status = std::process::ExitStatus;
    fn signal_before_reap(&mut self) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EPERM))
    }
    fn observe_before_reap(&mut self) -> io::Result<bool> {
        Ok(false)
    }
    fn reap(&mut self) -> io::Result<Self::Status> {
        unreachable!("injected unresolved leader")
    }
    fn probe_after_reap(&mut self) -> io::Result<()> {
        unreachable!("leader still unresolved")
    }
    fn retry_probe(&mut self) -> bool {
        false
    }
    fn missing_errno(&self) -> i32 {
        libc::ESRCH
    }
    fn permission_errno(&self) -> i32 {
        libc::EPERM
    }
    fn process_id(&self) -> u32 {
        self.0.id()
    }
    fn retain_unresolved(&mut self, detail: String) {
        retain_unresolved_bounded_read(self.0.clone(), BoundedReadReapState::Unreaped, detail);
    }
}

#[cfg(unix)]
#[test]
fn unresolved_unix_child_keeps_capture_shutdown_incomplete_and_retains_root() {
    let (lifetime, root, cleaned) = lifetime_fixture();
    let directory = root.clone();
    let (child_tx, child_rx) = mpsc::channel();
    lifetime
        .spawn("unresolved-unix-cleanup-control", move || {
            let mut command = git_command();
            command
                .arg("--version")
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
            let killing = child.clone();
            let _registration = register_capture_child(move || {
                let _ = killing.kill();
            });
            let error =
                finish_bounded_read_group(&mut UnresolvedCaptureGroup(child.clone())).unwrap_err();
            assert!(error.to_string().contains("leader_alive"));
            child_tx.send(child).unwrap();
        })
        .unwrap();
    let child = child_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let registry = lifetime.state.lock().unwrap();
    let (registry, timeout) = lifetime
        .changed
        .wait_timeout_while(registry, TEST_PHASE_DEADLOCK_GUARD, |registry| {
            !registry.workers[0].done.load(Ordering::Acquire)
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "capture thread completed before shutdown"
    );
    drop(registry);
    let (later_tx, later_rx) = mpsc::channel();
    let observer_tx = later_tx.clone();
    let later_capture = lifetime
        .spawn("later-capture", move || {
            later_tx.send(()).unwrap();
        })
        .is_ok();
    let later_observer = lifetime
        .spawn_observer("later-observer", move || {
            observer_tx.send(()).unwrap();
        })
        .is_ok();
    for admitted in [later_capture, later_observer] {
        if admitted {
            later_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
        }
    }
    lifetime.expire_shutdown.store(true, Ordering::Release);
    let completed = lifetime.stop_and_join();
    drop(lifetime);
    let root_retained = root.exists();
    let early_cleanup = cleaned.try_recv().ok();
    // Clean up the real harmless child even on the RED implementation. The
    // injected group has no descendants; this exact reap is its certificate.
    child.wait().unwrap();
    remove_reaped_bounded_read(&child);
    drop(child);
    if let Some(outcome) = &early_cleanup {
        assert!(outcome.is_ok());
    } else {
        cleaned
            .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .unwrap()
            .unwrap();
    }
    assert!(
        !completed,
        "thread completion is not a child-cleanup certificate"
    );
    assert!(
        root_retained && early_cleanup.is_none(),
        "unresolved child must retain root custody"
    );
    assert!(
        !root.exists(),
        "certified cleanup eventually releases the root"
    );
    assert!(
        later_capture && later_observer,
        "one unresolved child must not refuse later captures or observation workers"
    );
}

#[cfg(windows)]
#[test]
fn uncontained_capture_is_refused_before_it_can_spawn_a_descendant() {
    let (lifetime, root, cleaned) = lifetime_fixture();
    let marker = root.join("descendant-ran");
    let marker_arg = marker.clone();
    let directory = root.clone();
    let (result_tx, result_rx) = mpsc::channel();
    lifetime.spawn("unavailable-containment-control", move || {
        let mut command = windows_launch::LaunchSpec::new("node");
        // The descendant completes before its leader. RED therefore proves
        // real descendant execution without leaking an uncontained process.
        command.args(["-e", r#"const r=require('node:child_process').spawnSync(process.execPath,['-e',"require('node:fs').writeFileSync(process.argv[1], 'descendant-ran')",process.argv[1]],{stdio:'ignore'}); if(r.error) throw r.error; process.exit(r.status);"#]);
        command.arg(marker_arg).current_dir(directory);
        command.force_unavailable_containment();
        let result = run_bounded_read_windows_with_setup(&mut command,
            std::time::Instant::now() + SOURCE_READ_TIMEOUT, 4096, |_| Ok(()));
        result_tx.send(result.map(|_| ()).map_err(|error| format!("{error:#}"))).unwrap();
    }).unwrap();
    let result = result_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    assert!(lifetime.stop_and_join());
    let descendant_ran = marker.exists();
    drop(lifetime);
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert!(
        result
            .as_ref()
            .is_err_and(|error| error.contains("capture containment unavailable")),
        "capture must fail before resume: {result:?}"
    );
    assert!(
        !descendant_ran,
        "the uncontained leader must never execute its descendant spawn"
    );
}

#[cfg(unix)]
#[test]
fn unix_shutdown_cancels_live_git_before_releasing_the_capture_root() {
    let (lifetime, root, cleaned) = lifetime_fixture();
    run_git_test_command(&root, &["init"]);
    let git = PinnedGitRead::new(&root).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    lifetime
        .spawn("unix-live-git-cancellation", move || {
            let mut command = git.command(&["cat-file", "--batch"]);
            // Ordinary Git reads request false; capture ownership must supply the
            // process group and finish its cleanup on cancellation.
            let result = run_bounded_read_process_input(
                &mut command,
                git.deadline,
                4096,
                false,
                |child| {
                    started_tx.send(child.clone()).unwrap();
                    release_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
                    Ok(())
                },
                Some(Vec::new()),
            );
            let refused_next_read = git.run(&["status", "--porcelain"], false).is_err();
            finished_tx
                .send((result.is_err(), refused_next_read))
                .unwrap();
        })
        .unwrap();
    let child = started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    lifetime.stop();
    release_tx.send(()).unwrap();
    assert!(lifetime.stop_and_join(), "real Git cleanup is certified");
    assert_eq!(
        finished_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(),
        (true, true)
    );
    assert!(child.wait().unwrap().code() != Some(0));
    assert!(root.exists(), "host still owns its root");
    drop(child);
    drop(lifetime);
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert!(!root.exists());
}

/// Builds an observation request on a capture worker, as the closing capture
/// builds its observations. When `stop_first`, the worker's capture is stopped
/// first, as host retirement stops the captures before it joins the workers.
fn build_on_a_capture_worker(
    stop_first: bool,
    build: impl FnOnce() -> std::result::Result<EngramControlRequest, EngramTransportError>
    + Send
    + 'static,
) -> std::result::Result<EngramControlRequest, EngramTransportError> {
    let lifetime = Arc::new(CaptureLifetime::default());
    let (built_tx, built_rx) = mpsc::channel();
    let worker_lifetime = lifetime.clone();
    lifetime
        .spawn_observer("observation-on-a-stopped-capture", move || {
            if stop_first {
                worker_lifetime.stop();
            }
            built_tx.send(build()).unwrap();
        })
        .unwrap();
    let built = built_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    assert!(lifetime.stop_and_join(), "the capture worker ends");
    built
}

fn observation_basis(key: &str) -> EngramUnadmittedObservationBasis {
    EngramUnadmittedObservationBasis {
        idempotency_key: key.into(),
        binding: EngramControlWorkBinding {
            root_execution_id: "fixture-root".into(),
            work_id: "fixture-work".into(),
            run_id: "fixture-run".into(),
            work_revision: 1,
            claim_id: "fixture-claim".into(),
            claim_fence: 1,
        },
        root_basis: EngramObservationRootBasis {
            capture_run_cut: 1,
            latest_event: None,
            state: EngramObservationRootState::None {},
        },
        observed_interval: EngramObservationInterval {
            from: "2026-01-01T00:00:00Z".into(),
            through: "2026-01-01T00:00:02Z".into(),
        },
        policy_basis: EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 1,
            policy: "a".repeat(32),
            obligation_rule_set: "b".repeat(32),
        },
    }
}

fn observation_built_at() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:03Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// A source basis with a complete SourceId whose tree is `tree` repeated.
fn complete_source(tree: char) -> Value {
    json!({"workspace_id": "fixture-workspace", "source_id": {
        "commit": "a".repeat(40), "tree": tree.to_string().repeat(40),
        "submodules": [], "changes": []}})
}

fn source_basis(source: Value) -> EngramExecutionSourceBasis {
    serde_json::from_value(source).unwrap()
}

fn sighting_of(
    source: Value,
) -> impl FnOnce() -> std::result::Result<EngramControlRequest, EngramTransportError> + Send + 'static
{
    move || {
        engram_source_sighting_request(
            observation_basis("fixture-sighting"),
            EngramMeasuredSighting {
                source_basis: source_basis(source),
                observed_at: "2026-01-01T00:00:01Z".into(),
            },
            observation_built_at(),
        )
    }
}

/// An unadmitted turn whose measured change starts at `baseline` and ends at
/// a complete sighting of another tree.
fn change_from(
    baseline: Value,
) -> impl FnOnce() -> std::result::Result<EngramControlRequest, EngramTransportError> + Send + 'static
{
    move || {
        let baseline = source_basis(baseline);
        engram_unadmitted_turn_request(
            observation_basis("fixture-turn"),
            "fixture-turn".into(),
            Some(EngramObservedSourceChange::ContentComparison {
                workspace_id: "fixture-workspace".into(),
                baseline: EngramMeasuredBaseline {
                    workspace_id: baseline.workspace_id.clone(),
                    source_revision: baseline.source_revision.clone(),
                    source_id: baseline.source_id.clone(),
                    observed_at: "2026-01-01T00:00:00Z".into(),
                },
                sighting: EngramMeasuredSighting {
                    source_basis: source_basis(complete_source('e')),
                    observed_at: "2026-01-01T00:00:02Z".into(),
                },
            }),
            vec![],
            observation_built_at(),
        )
    }
}

#[test]
fn a_complete_observation_identity_stays_valid_after_host_shutdown() {
    for stop_first in [false, true] {
        let when = if stop_first {
            "after host shutdown"
        } else {
            "on a running capture"
        };
        if let Err(error) = build_on_a_capture_worker(stop_first, sighting_of(complete_source('b')))
        {
            panic!("a complete sighting is valid {when}: {}", error.message);
        }
        if let Err(error) = build_on_a_capture_worker(stop_first, change_from(complete_source('b')))
        {
            panic!(
                "a complete source change is valid {when}: {}",
                error.message
            );
        }
    }
}

#[test]
fn a_malformed_observation_identity_stays_malformed_after_host_shutdown() {
    let no_source_id =
        json!({"workspace_id": "fixture-workspace", "source_revision": "content-v1:fixture"});
    let mut also_a_revision = complete_source('b');
    also_a_revision["source_revision"] = json!("content-v1:fixture");
    for stop_first in [false, true] {
        for (shape, malformed) in [
            ("no source_id", &no_source_id),
            ("a source_revision", &also_a_revision),
        ] {
            let case = format!("{shape}, stopped first: {stop_first}");
            let sighting = build_on_a_capture_worker(stop_first, sighting_of(malformed.clone()))
                .expect_err(&format!("{case}: a malformed sighting is refused"));
            assert!(
                sighting
                    .message
                    .contains("source_sighting.source_basis needs one complete bounded source_id"),
                "{case}: the sighting is refused as malformed: {}",
                sighting.message
            );
            let change = build_on_a_capture_worker(stop_first, change_from(malformed.clone()))
                .expect_err(&format!("{case}: a malformed baseline is refused"));
            assert!(
                change
                    .message
                    .contains("source_change.baseline needs one complete bounded source identity"),
                "{case}: the baseline is refused as malformed: {}",
                change.message
            );
        }
    }
}
