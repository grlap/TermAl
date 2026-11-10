use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[test]
fn expired_capture_backstop_retains_root_until_the_pending_worker_is_joined() {
    let state = test_app_state();
    let cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    let root = state.test_temp_root.as_ref().unwrap().path().to_owned();
    let lifetime = state.capture_lifetime.clone();
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
    // A deterministic uninterruptible-read stand-in with an already expired
    // existing deadline: no added production timeout, sleep or retry.
    lifetime
        .spawn_owned("expired-capture", Some(std::time::Instant::now()), || {
            panic!("a cancelled capture must not start another read");
        })
        .unwrap();
    started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    assert!(!lifetime.stop_and_join(), "the backstop returns incomplete");
    assert!(
        !lifetime.stop_and_join(),
        "a pending drainer is never success"
    );
    assert!(lifetime.state.lock().unwrap().pending_logged);
    state.trigger_shutdown_signal();
    drop(state);
    drop(lifetime);
    assert!(root.exists());
    assert!(
        cleaned.try_recv().is_err(),
        "pending cleanup retains root custody"
    );
    release_tx.send(()).unwrap();
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert!(!root.exists(), "the drainer joins then releases custody");
}

// The read-only Git waits for EOF on stdin. The setup boundary keeps stdin
// open until shutdown has killed it; no sleep or timeout expiry releases it.
#[test]
fn shutdown_kills_inflight_git_and_joins_an_abandoned_capture_before_root_cleanup() {
    let state = test_app_state();
    let cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
    let root = state.test_temp_root.as_ref().unwrap().path().to_owned();
    run_git_test_command(&root, &["init"]);
    let git = ReviewFreezeGit::new(&root).unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let attempted_later_git = Arc::new(AtomicBool::new(false));
    let attempted = attempted_later_git.clone();
    let revision: Option<String> = bounded_source_capture(
        &live,
        &state.capture_lifetime,
        1,
        Duration::ZERO,
        move || {
            let mut command = git.command(&["cat-file", "--batch"]);
            #[cfg(windows)]
            let result = {
                command.piped_stdin();
                run_bounded_read_windows_with_setup(&mut command, git.deadline, 4096, |child| {
                    ready_tx.send(child.clone()).unwrap();
                    release_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
                    Ok(())
                })
            };
            #[cfg(not(windows))]
            let result = run_bounded_read_process_input(
                &mut command,
                git.deadline,
                4096,
                true,
                |child| {
                    ready_tx.send(child.clone()).unwrap();
                    release_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
                    Ok(())
                },
                Some(Vec::new()),
            );
            assert!(result.is_err(), "shutdown cancels the in-flight read");
            assert!(git.run(&["status", "--porcelain"], false).is_err());
            attempted.store(true, Ordering::Release);
            None
        },
    );
    assert_eq!(
        revision, None,
        "the caller has already abandoned its capture"
    );
    let child = ready_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    let retiring = state.clone();
    let shutdown = std::thread::spawn(move || retiring.trigger_shutdown_signal());
    #[cfg(windows)]
    assert!(
        child
            .wait_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .unwrap()
            .is_some(),
        "shutdown kills Git while its stdin remains open, before the 40-second budget"
    );
    #[cfg(unix)]
    {
        let (exit_tx, exit_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            exit_tx.send(result).unwrap();
        });
        assert_eq!(exit_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap(), 0);
    }
    // The worker is still in setup and shutdown must join it. It can acquire
    // StateInner while shutdown waits, proving no state lock crosses the join.
    drop(state.inner.lock().unwrap());
    release_tx.send(()).unwrap();
    shutdown.join().unwrap();
    assert!(attempted_later_git.load(Ordering::Acquire));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert!(
        state
            .capture_lifetime
            .state
            .lock()
            .unwrap()
            .workers
            .is_empty()
    );
    assert!(cleaned.try_recv().is_err(), "the host still owns its root");
    drop(state);
    cleaned
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    assert!(!root.exists());
}

#[test]
fn acceptance_source_capture_is_cancelled_by_the_same_host_lifetime() {
    let turn = super::engram_host_adapter::turn_checks::EOCheckedTurn::idle(
        "acceptance-capture-lifetime",
        true,
    );
    let state = turn.state.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    *state.capture_lifetime.before_capture.lock().unwrap() = Some(Arc::new(move || {
        let release_tx = release_tx.clone();
        let _registration = register_capture_child(move || {
            let _ = release_tx.send(());
        });
        started_tx.send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .unwrap();
    }));
    let measuring = state.clone();
    let owner = turn.session_id.clone();
    let place = EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned());
    let source =
        std::thread::spawn(move || measuring.acceptance_shared_source_capture(&owner, &place));
    started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    state.trigger_shutdown_signal();
    assert!(source.join().unwrap().is_none());
    assert_eq!(
        state
            .inner
            .lock()
            .unwrap()
            .acceptance_source_captures_live
            .load(Ordering::SeqCst),
        0
    );
    assert!(
        state
            .capture_lifetime
            .state
            .lock()
            .unwrap()
            .workers
            .is_empty()
    );
}

#[test]
fn discarded_snapshot_is_still_joined_and_shutdown_closes_capture_admission() {
    let state = test_app_state();
    let workers = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let completed = Arc::new(AtomicBool::new(false));
    let done = completed.clone();
    let discarded = EngramCapture::spawn(&workers, &state.capture_lifetime, move || {
        let _registration = register_capture_child(move || {
            let _ = release_tx.send(());
        });
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
        done.store(true, Ordering::Release);
    });
    drop(discarded);
    started_rx.recv_timeout(TEST_PHASE_DEADLOCK_GUARD).unwrap();
    state.trigger_shutdown_signal();
    assert!(completed.load(Ordering::Acquire));
    assert_eq!(workers.load(Ordering::SeqCst), 0);
    let ran = Arc::new(AtomicBool::new(false));
    let rejected = ran.clone();
    assert!(
        state
            .capture_lifetime
            .spawn("must-not-start", move || {
                rejected.store(true, Ordering::SeqCst);
            })
            .is_err()
    );
    assert!(!ran.load(Ordering::SeqCst));
}
