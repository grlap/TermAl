use super::*;

#[test]
fn state_mutex_wait_diagnostic_is_reported_before_the_waiting_guard_drops() {
    let (diagnostic_sender, diagnostic_receiver) = std::sync::mpsc::channel();
    let reporter: StateMutexDiagnosticReporter = Arc::new(move |diagnostic| {
        diagnostic_sender
            .send(diagnostic)
            .expect("diagnostic receiver should remain connected");
    });
    let state = Arc::new(StateMutex::new_with_diagnostic_reporter(
        (),
        Duration::ZERO,
        reporter,
    ));
    let first_guard = state.lock().expect("first state lock should succeed");
    let initial_wait = diagnostic_receiver
        .recv()
        .expect("initial wait diagnostic should be emitted");
    assert_wait_diagnostic(initial_wait);

    let (acquired_sender, acquired_receiver) = std::sync::mpsc::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let waiting_state = Arc::clone(&state);
    let waiter = std::thread::spawn(move || {
        let waiting_guard = waiting_state
            .lock()
            .expect("waiting state lock should succeed");
        acquired_sender
            .send(())
            .expect("acquisition receiver should remain connected");
        release_receiver
            .recv()
            .expect("release sender should remain connected");
        drop(waiting_guard);
    });

    drop(first_guard);
    acquired_receiver
        .recv()
        .expect("waiting guard should acquire the state lock");
    let wait_diagnostic = loop {
        let diagnostic = diagnostic_receiver
            .recv()
            .expect("waiting diagnostic should be emitted");
        if matches!(diagnostic, StateMutexDiagnostic::Waited { .. }) {
            break diagnostic;
        }
    };
    assert_wait_diagnostic(wait_diagnostic);
    assert!(
        state.inner.try_lock().is_err(),
        "waiter must still own the guard"
    );

    release_sender
        .send(())
        .expect("waiting thread should remain connected");
    waiter.join().expect("waiting thread should finish");
}

#[test]
fn state_mutex_hold_diagnostic_runs_after_the_underlying_guard_is_released() {
    let state_slot = Arc::new(Mutex::new(None::<std::sync::Weak<StateMutex<()>>>));
    let released_before_report = Arc::new(AtomicBool::new(false));
    let reporter_state_slot = Arc::clone(&state_slot);
    let reporter_released_before_report = Arc::clone(&released_before_report);
    let reporter: StateMutexDiagnosticReporter = Arc::new(move |diagnostic| {
        let StateMutexDiagnostic::Held {
            held,
            waited,
            file,
            line,
            column,
        } = diagnostic
        else {
            return;
        };
        assert!(held >= Duration::ZERO);
        assert!(waited >= Duration::ZERO);
        assert!(file.ends_with("state_mutex.rs"));
        assert!(line > 0);
        assert!(column > 0);
        let state = reporter_state_slot
            .lock()
            .expect("test state slot mutex should not be poisoned")
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .expect("state should remain alive");
        reporter_released_before_report.store(state.inner.try_lock().is_ok(), Ordering::SeqCst);
    });
    let state = Arc::new(StateMutex::new_with_diagnostic_reporter(
        (),
        Duration::ZERO,
        reporter,
    ));
    *state_slot
        .lock()
        .expect("test state slot mutex should not be poisoned") = Some(Arc::downgrade(&state));

    let guard = state.lock().expect("state lock should succeed");
    drop(guard);

    assert!(released_before_report.load(Ordering::SeqCst));
}

#[test]
fn state_mutex_diagnostic_queue_drops_when_full_without_waiting() {
    let (sender, _receiver) = mpsc::sync_channel(STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY);
    let diagnostic = || StateMutexDiagnostic::Waited {
        waited: Duration::from_millis(1),
        file: "test.rs",
        line: 1,
        column: 1,
    };

    for _ in 0..STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY {
        assert!(try_queue_state_mutex_diagnostic(&sender, diagnostic()));
    }
    assert!(!try_queue_state_mutex_diagnostic(&sender, diagnostic()));
}

fn assert_wait_diagnostic(diagnostic: StateMutexDiagnostic) {
    let StateMutexDiagnostic::Waited {
        waited,
        file,
        line,
        column,
    } = diagnostic
    else {
        panic!("expected wait diagnostic");
    };
    assert!(waited >= Duration::ZERO);
    assert!(file.ends_with("state_mutex.rs"));
    assert!(line > 0);
    assert!(column > 0);
}
