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
            let mut command = Command::new("git");
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
            std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, 4096, |_| Ok(()));
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
    let git = ReviewFreezeGit::new(&root).unwrap();
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
