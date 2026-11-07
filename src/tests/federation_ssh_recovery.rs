// Exercises owned late SSH results with controlled IO, without launching SSH.
// Completion channels synchronize the test; no real reporting timeout is awaited.
use super::*;

struct LateChild {
    cleanups: Arc<AtomicUsize>,
    dropped: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FederationSshChild for LateChild {
    fn child_exited(&mut self) -> Result<bool, ApiError> { Ok(false) }

    fn shutdown(&mut self) -> Result<(), ApiError> {
        self.cleanups.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Drop for LateChild {
    fn drop(&mut self) {
        if let Some(dropped) = self.dropped.take() { let _ = dropped.send(()); }
    }
}

#[tokio::test]
async fn late_start_result_is_explicitly_cleaned_once_after_reporting_deadline() {
    let cleanups = Arc::new(AtomicUsize::new(0));
    let child_cleanups = cleanups.clone();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (released, release_rx) = std::sync::mpsc::channel();
    let (dropped, dropped_rx) = tokio::sync::oneshot::channel();
    let unresolved = Arc::new(FederationOsAccount::default());
    // The operation cannot return before explicit release. Zero selects the
    // reporting deadline immediately, independent of OS thread scheduling.
    let result = federation_os_operation(
        move |owned| {
            let _ = entered.send(());
            if release_rx.recv().is_err() { return Err(federation_transport_unavailable()); }
            *owned = Some(Box::new(LateChild {
                cleanups: child_cleanups, dropped: Some(dropped),
            }));
            Ok(())
        },
        None,
        unresolved.clone(),
        Duration::ZERO,
    ).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx).await.unwrap().unwrap();
    assert!(result.is_err());
    assert!(unresolved.pending());
    assert_eq!(cleanups.load(Ordering::SeqCst), 0);
    released.send(()).unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, dropped_rx).await.unwrap().unwrap();
    assert_eq!(cleanups.load(Ordering::SeqCst), 1,
        "the original operation must explicitly reap its late child exactly once");
}

async fn cleanup_returned(account: &FederationOsAccount) {
    let flight = account.flight.lock().unwrap().clone().unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = flight.changed.notified();
            if !flight.state.lock().unwrap().running { return; }
            changed.await;
        }
    }).await.unwrap();
}

#[derive(Clone, Copy)]
enum CleanupReply { KillFailed, WaitFailed, Reaped, Panic }

struct ControlledChild {
    alive: bool,
    calls: Arc<AtomicUsize>,
    kills: Arc<AtomicUsize>,
    waits: Arc<AtomicUsize>,
    reaps: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    entered: tokio::sync::mpsc::UnboundedSender<std::sync::mpsc::Sender<CleanupReply>>,
}

impl FederationSshChild for ControlledChild {
    fn child_exited(&mut self) -> Result<bool, ApiError> { Ok(!self.alive) }
    fn shutdown(&mut self) -> Result<(), ApiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (reply, receive) = std::sync::mpsc::channel();
        self.entered.send(reply).map_err(|_| federation_transport_unavailable())?;
        let reply = receive.recv().map_err(|_| federation_transport_unavailable())?;
        if matches!(reply, CleanupReply::Panic) { panic!("injected cleanup panic while still owning child"); }
        if self.alive {
            if matches!(reply, CleanupReply::KillFailed) { return Err(federation_transport_unavailable()); }
            self.kills.fetch_add(1, Ordering::SeqCst);
            self.alive = false;
        }
        self.waits.fetch_add(1, Ordering::SeqCst);
        if matches!(reply, CleanupReply::WaitFailed) { return Err(federation_transport_unavailable()); }
        self.reaps.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl Drop for ControlledChild {
    fn drop(&mut self) { self.drops.fetch_add(1, Ordering::SeqCst); }
}

async fn cleanup_case(alive: bool, first: CleanupReply) {
    let account = Arc::new(FederationOsAccount::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let kills = Arc::new(AtomicUsize::new(0));
    let waits = Arc::new(AtomicUsize::new(0));
    let reaps = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let (entered, mut steps) = tokio::sync::mpsc::unbounded_channel();
    let child = Box::new(ControlledChild {
        alive, calls: calls.clone(), kills: kills.clone(), waits: waits.clone(),
        reaps: reaps.clone(), drops: drops.clone(), entered,
    });
    assert!(federation_os_cleanup(child, account.clone(), Duration::ZERO).await.is_err());
    let reply = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, steps.recv()).await.unwrap().unwrap();
    for _ in 0..8 { account.tick(); }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "ticks must not duplicate the outstanding call");
    assert!(account.pending());
    assert_eq!(reaps.load(Ordering::SeqCst), 0, "exit or a kill request is not reap proof");
    reply.send(first).unwrap();
    cleanup_returned(&account).await;
    if !matches!(first, CleanupReply::Reaped) {
        assert_eq!(drops.load(Ordering::SeqCst), 0, "an error must retain the exact child");
        assert!(account.pending());
        account.tick();
        let reply = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, steps.recv()).await.unwrap().unwrap();
        for _ in 0..8 { account.tick(); }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(account.pending());
        reply.send(CleanupReply::Reaped).unwrap();
        cleanup_returned(&account).await;
    }
    account.tick();
    assert!(!account.pending(), "proven reap must release the automatic recovery fence");
    assert_eq!(reaps.load(Ordering::SeqCst), 1);
    assert_eq!(kills.load(Ordering::SeqCst), usize::from(alive));
    assert_eq!(waits.load(Ordering::SeqCst), if matches!(first, CleanupReply::WaitFailed) { 2 } else { 1 });
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(account.stage(), "sshRecovered");
}

#[tokio::test]
async fn late_successful_reap_clears_the_recovery_fence() {
    cleanup_case(true, CleanupReply::Reaped).await;
}
#[tokio::test]
async fn exited_but_unreaped_child_stays_owned_until_wait_succeeds() {
    cleanup_case(false, CleanupReply::Reaped).await;
}
#[tokio::test]
async fn failed_kill_retries_the_same_live_child_without_overlap() {
    cleanup_case(true, CleanupReply::KillFailed).await;
}
#[tokio::test]
async fn failed_wait_retains_the_exited_child_and_never_repeats_kill() {
    cleanup_case(true, CleanupReply::WaitFailed).await;
}

#[tokio::test]
async fn cleanup_panic_retains_the_same_child_for_eventual_reap() {
    cleanup_case(true, CleanupReply::Panic).await;
}

#[tokio::test]
async fn start_panic_after_publication_still_reaps_the_owned_child() {
    let account = Arc::new(FederationOsAccount::default());
    let cleanups = Arc::new(AtomicUsize::new(0));
    let child_cleanups = cleanups.clone();
    let (dropped, dropped_rx) = tokio::sync::oneshot::channel();
    let result = federation_os_operation(move |owned| {
        *owned = Some(Box::new(LateChild { cleanups: child_cleanups, dropped: Some(dropped) }));
        panic!("injected post-spawn setup panic");
    }, None, account.clone(), phase_sync::DEADLOCK_GUARD).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, dropped_rx).await.unwrap().unwrap();
    assert!(result.is_err());
    assert_eq!(cleanups.load(Ordering::SeqCst), 1);
    account.tick();
    assert!(!account.pending());
}

#[tokio::test]
async fn start_panic_before_child_creation_settles_as_no_child() {
    let account = Arc::new(FederationOsAccount::default());
    let result = federation_os_operation(|_owned| panic!("injected pre-spawn panic"),
        None, account.clone(), phase_sync::DEADLOCK_GUARD).await;
    assert!(result.is_err());
    account.tick();
    assert!(!account.pending());
}

#[tokio::test]
async fn late_start_error_proves_no_child_and_clears_only_on_a_tick() {
    let account = Arc::new(FederationOsAccount::default());
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let result = federation_os_operation(move |_owned| {
        let _ = entered.send(());
        let _ = released.recv();
        Err(federation_transport_unavailable())
    }, None, account.clone(), Duration::ZERO).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx).await.unwrap().unwrap();
    assert!(result.is_err());
    for _ in 0..8 { account.tick(); }
    assert!(account.pending());
    release.send(()).unwrap();
    cleanup_returned(&account).await;
    assert!(account.pending());
    account.tick();
    assert!(!account.pending());
}
