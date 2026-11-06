// Controlled shutdown scheduling and subprocess-isolated teardown failures.
// Uses real attested fixtures; no SSH, live stores or timing performance claim.
use super::*;

pub(super) struct ReceiveRelease(pub(super) Option<mpsc::Sender<()>>);

impl ReceiveRelease {
    pub(super) fn release(&mut self) {
        self.0.take().unwrap().send(()).unwrap();
    }
}

impl Drop for ReceiveRelease {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() { let _ = send.send(()); }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_shutdown_driver_sets_stop_while_both_workers_wait_for_state() {
    let spawned_app = test_app_state();
    let direct_app = test_app_state();
    let held = spawned_app.inner.lock().unwrap();
    let (entered_tx, entered) = mpsc::channel();
    let mut workers = Vec::new();
    for _ in 0..2 {
        let app = spawned_app.clone();
        let entered_tx = entered_tx.clone();
        workers.push(tokio::spawn(async move {
            entered_tx.send(std::thread::current().id()).unwrap();
            // No await between the notification and this synchronous lock:
            // this runtime worker cannot poll another task until it opens.
            let _held = app.inner.lock().unwrap();
        }));
    }
    let first = phase_sync::receive(&entered, "first worker waiting for state");
    let second = phase_sync::receive(&entered, "second worker waiting for state");
    assert_ne!(first, second, "both runtime workers must be occupied");

    let spawned = no_external_runtime(&spawned_app);
    let spawned_shutdown = tokio::spawn(spawned.shutdown());
    let direct = no_external_runtime(&direct_app);
    let mut direct_shutdown = Box::pin(direct.shutdown());
    assert_join_pending(direct_shutdown.as_mut()).await;
    assert!(direct_app.federation_executor_wake_signal.is_stopping());
    assert!(!spawned_app.federation_executor_wake_signal.is_stopping(),
        "spawning shutdown cannot establish stop while both workers are blocked");

    // Release before any join. The held MutexGuard also opens on assertion
    // unwind, so this diagnostic cannot leave its workers parked.
    drop(held);
    for worker in workers { worker.await.unwrap(); }
    spawned_shutdown.await.unwrap();
    direct_shutdown.await;
    assert!(spawned_app.federation_executor_wake_signal.is_stopping());
}

fn unwind_child(mode: &str) {
    let module = module_path!().split_once("::").unwrap().1;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", &format!("{module}::federation_shutdown_unwind_child"), "--nocapture"])
        .env("TERMAL_TEST_FEDERATION_UNWIND", mode);
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("federation teardown during original unwind");
    assert!(status.success(), "{mode}: {status}\n{stderr}");
}

#[test]
fn federation_shutdown_guard_unwinds_with_poisoned_link() { unwind_child("link"); }

#[test]
fn federation_shutdown_guard_normally_retires_exact_owner_on_unwind() { unwind_child("normal"); }

#[test]
fn federation_shutdown_guard_unwinds_with_poisoned_basis() { unwind_child("basis"); }

#[test]
fn federation_shutdown_guard_unwinds_with_poisoned_state() { unwind_child("state"); }

#[test]
fn federation_shutdown_old_guard_cannot_retire_current_owner_after_poison() { unwind_child("old-owner"); }

#[test]
fn federation_shutdown_wrong_incarnation_cannot_retire_current_owner_after_poison() { unwind_child("old-incarnation"); }

#[test]
fn federation_shutdown_terminal_observer_does_not_double_panic() { unwind_child("observer"); }

#[test]
fn federation_shutdown_receive_job_does_not_double_panic() { unwind_child("receive"); }

#[test]
fn federation_shutdown_unwind_child() {
    let Ok(mode) = std::env::var("TERMAL_TEST_FEDERATION_UNWIND") else { return; };
    let fixture = Fixture::new();
    let channel = &fixture.channel;
    let context = channel.security_context().unwrap();
    let signal = fixture.satellite.federation_executor_wake_signal.clone();
    let receive = (mode == "receive").then(|| signal.begin_receive().unwrap());
    // Poison each owner before the original failure, without changing its
    // contents. The child boundary makes the old double panic a test failure
    // instead of aborting the parent suite.
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match mode.as_str() {
            "link" | "old-owner" | "old-incarnation" => { let _held = channel.state.lock().unwrap(); panic!("injected link poison"); }
            "basis" => { let _held = channel.basis.write().unwrap(); panic!("injected basis poison"); }
            "state" | "observer" => { let _held = fixture.satellite.inner.lock().unwrap(); panic!("injected state poison"); }
            "receive" => { let _held = signal.receives.lock().unwrap(); panic!("injected receive-counter poison"); }
            "normal" => {},
            _ => panic!("unknown teardown mode"),
        }
    }));
    assert_eq!(poisoned.is_err(), mode != "normal");
    // The fixture's first heartbeat already ended its diagnostic receiver.
    // Reset only this observation flag so the observer case really attempts
    // its poisoned-state diagnostic if the unwind guard is removed.
    if mode == "observer" { context.terminal_logged.store(false, Ordering::SeqCst); }
    let guard = FederationControlGuard {
        channel: channel.clone(),
        owner: if mode == "old-owner" { context.owner - 1 } else { context.owner },
        incarnation: if mode == "old-incarnation" { Uuid::new_v4().to_string() } else { context.incarnation.clone() },
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let original = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = guard;
        let _receive = receive;
        let _observer = (mode == "observer").then(|| FederationControlEnd {
            channel, context: &context, reason: "fixtureFailure", io_kind: None, sequence: 0,
        });
        panic!("original controlled assertion failure");
    }));
    let payload = original.expect_err("the original failure must remain visible");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"original controlled assertion failure"));
    let state = channel.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if mode == "old-owner" || mode == "old-incarnation" {
        assert!(state.security.as_ref().is_some_and(|current| Arc::ptr_eq(current, &context)));
        assert_eq!(state.incarnation.as_deref(), Some(context.incarnation.as_str()));
        assert_eq!(state.recorded_incarnation.as_deref(), Some(context.incarnation.as_str()));
        assert!(!*context.retired.borrow());
        assert_eq!(fixture.satellite.inner.lock().unwrap().federation.links
            .get(&channel.snapshot.peer.host_id), Some(&context.incarnation));
    } else {
        assert!(state.security.is_none());
        assert!(state.incarnation.is_none());
        assert!(state.recorded_incarnation.is_none());
        assert!(*context.retired.borrow());
        let inner = fixture.satellite.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(!inner.federation.links.contains_key(&channel.snapshot.peer.host_id));
    }
    drop(state);
    if mode == "receive" {
        assert_eq!(*signal.receives.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 0);
        assert!(signal.is_stopping(), "poisoned accounting cannot admit more work");
    }
    if mode == "observer" { assert!(!context.terminal_logged.load(Ordering::SeqCst)); }
    match mode.as_str() {
        "link" | "old-owner" | "old-incarnation" => assert!(channel.state.is_poisoned()),
        "basis" => assert!(channel.basis.snapshot.is_poisoned()),
        "state" | "observer" => assert!(fixture.satellite.inner.lock().is_err()),
        "receive" => assert!(signal.receives.is_poisoned()),
        _ => {},
    }
}
