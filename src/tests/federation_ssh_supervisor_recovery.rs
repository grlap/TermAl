// Exercises the real supervisor across reconfiguration and a late owned start.
// IO gates and cycle notifications drive ordering; no reporting deadline is awaited.
use super::*;

struct ReleaseStart(Option<std::sync::mpsc::Sender<()>>);
impl ReleaseStart {
    fn release(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}
impl Drop for ReleaseStart {
    fn drop(&mut self) {
        self.release();
    }
}

struct LateOwnedChild {
    child: Box<dyn FederationSshChild>,
    reaps: Arc<AtomicUsize>,
    dropped: Option<tokio::sync::oneshot::Sender<()>>,
}
impl FederationSshChild for LateOwnedChild {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        self.child.take_control_stream()
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        self.child.child_exited()
    }
    fn shutdown(&mut self) -> Result<(), ApiError> {
        self.child.shutdown()?;
        self.reaps.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl Drop for LateOwnedChild {
    fn drop(&mut self) {
        if let Some(send) = self.dropped.take() {
            let _ = send.send(());
        }
    }
}

async fn next_cycle(cycles: &mut tokio::sync::mpsc::UnboundedReceiver<()>) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, cycles.recv())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supervisor_reconfiguration_waits_for_the_late_child_reap_then_reconnects() {
    let mut fixture = Fixture::new().await;
    let starts = Arc::new(AtomicUsize::new(0));
    let reaps = Arc::new(AtomicUsize::new(0));
    let start_count = starts.clone();
    let reap_count = reaps.clone();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let mut release = ReleaseStart(Some(release));
    let (dropped, dropped_rx) = tokio::sync::oneshot::channel();
    let dropped = Mutex::new(Some(dropped));
    let start = fixture.io.ssh.clone();
    fixture.io.ssh = Arc::new(move |snapshot, port, pin, owned| {
        let first = start_count.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            if let Some(entered) = entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            released
                .lock()
                .unwrap()
                .recv()
                .map_err(|_| federation_transport_unavailable())?;
        }
        start(snapshot, port, pin, owned)?;
        if first {
            *owned = Some(Box::new(LateOwnedChild {
                child: owned.take().unwrap(),
                reaps: reap_count.clone(),
                dropped: dropped.lock().unwrap().take(),
            }));
        }
        Ok(())
    });
    let (cycle, mut cycles) = tokio::sync::mpsc::unbounded_channel();
    fixture.io.reconciled = Arc::new(move |_| {
        let _ = cycle.send(());
    });
    let (held, held_rx) = tokio::sync::oneshot::channel();
    let held = Mutex::new(Some(held));
    let finished = fixture.io.attempt_finished.clone();
    fixture.io.attempt_finished = Arc::new(move |result| {
        finished(result);
        if let Some(held) = held.lock().unwrap().take() {
            let _ = held.send(());
        }
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    let old = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let version = fixture.app.inner.lock().unwrap().federation.version;
    // A locator update changes the transport basis, preserving the existing
    // pair enrollment shared with this fixture's static satellite channel.
    fixture
        .app
        .update_federation_ssh_host(&fixture.id, version, "changed.example".to_owned())
        .unwrap();
    // Reconfiguration cancels the old await; the actual OS start stays held.
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, held_rx)
        .await
        .unwrap()
        .unwrap();
    while cycles.try_recv().is_ok() {}
    next_cycle(&mut cycles).await;
    next_cycle(&mut cycles).await;
    let fenced = starts.load(Ordering::SeqCst) == 1
        && runtime.quarantined.lock().unwrap().contains(&fixture.id)
        && reaps.load(Ordering::SeqCst) == 0;
    release.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, dropped_rx)
        .await
        .unwrap()
        .unwrap();
    // Drop notification occurs while the owned flight publishes reap proof.
    // The next supervisor cycle takes that same lock before clearing its fence.
    while cycles.try_recv().is_ok() {}
    next_cycle(&mut cycles).await;
    next_cycle(&mut cycles).await;
    let released_fence = !runtime.quarantined.lock().unwrap().contains(&fixture.id);
    let healthy = if released_fence {
        fixture
            .event(|event| {
                matches!(event, Event::Response(path, status)
            if path.ends_with("health") && status.is_success())
            })
            .await
    } else {
        false
    };
    let snapshot = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let lease = runtime
        .peer(&fixture.id)
        .and_then(|peer| peer.channel.current_lease().ok());
    let view = fixture.app.federation_registry_view().unwrap();
    let worker = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["host"]["hostId"] == fixture.id)
        .unwrap()["attestation"]["worker"]
        .clone();
    let observed_starts = starts.load(Ordering::SeqCst);
    let observed_reaps = reaps.load(Ordering::SeqCst);
    fixture.finish(runtime).await;
    assert!(
        fenced,
        "configuration and cycles must retain the exact outstanding start"
    );
    assert!(
        released_fence,
        "a late proven reap must release quarantine without restart"
    );
    assert!(
        healthy && lease.is_some(),
        "the supervisor must reconnect using current authority"
    );
    assert!(snapshot.transport_generation > old.transport_generation);
    assert_eq!(snapshot.enrollment_generation, old.enrollment_generation);
    assert_eq!(snapshot.endpoint.ssh_host, "changed.example");
    assert_eq!(observed_starts, 2);
    assert_eq!(
        observed_reaps, 1,
        "the late old child must be reaped exactly once"
    );
    assert_eq!(worker["stage"], "authenticated");
    assert_eq!(worker["currentBasis"], true);
    assert_eq!(worker["quarantinedForRuntime"], false);
    assert!(worker["recoveryReason"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returned_cleanup_error_recovers_on_the_same_configuration() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(
        fixture
            .event(|event| matches!(event, Event::Response(path, status)
        if path.ends_with("health") && status.is_success()))
            .await
    );
    let before = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let old_incarnation = runtime
        .peer(&fixture.id)
        .unwrap()
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    fixture.cleanup_unknown.store(true, Ordering::SeqCst);
    fixture.fail_health.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .event(|event| matches!(event, Event::Attempt(StatusCode::INTERNAL_SERVER_ERROR)))
            .await
    );
    let held = runtime.quarantined.lock().unwrap().contains(&fixture.id);
    let view = fixture.app.federation_registry_view().unwrap();
    let recovery = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["host"]["hostId"] == fixture.id)
        .unwrap()["attestation"]["worker"]
        .clone();
    // Explicitly settle the fake OS result, with enrollment and settings left
    // alone. The real worker's existing backoff and handshake then run normally.
    fixture.fail_health.store(false, Ordering::SeqCst);
    fixture.cleanup_unknown.store(false, Ordering::SeqCst);
    let restarted = fixture.event(|event| matches!(event, Event::Started)).await;
    let healthy = restarted
        && fixture
            .event(|event| {
                matches!(event, Event::Response(path, status)
        if path.ends_with("health") && status.is_success())
            })
            .await;
    let after = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let successor = runtime
        .peer(&fixture.id)
        .and_then(|peer| peer.channel.current_lease().ok());
    let clear = !runtime.quarantined.lock().unwrap().contains(&fixture.id);
    let view = fixture.app.federation_registry_view().unwrap();
    let worker = view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["host"]["hostId"] == fixture.id)
        .unwrap()["attestation"]["worker"]
        .clone();
    fixture.finish(runtime).await;
    assert!(held, "a returned reap error must retain quarantine");
    assert_eq!(recovery["currentBasis"], true);
    assert_eq!(recovery["quarantinedForRuntime"], true);
    assert!(matches!(
        recovery["recoveryReason"].as_str(),
        Some("ownedSshOperationPending" | "ownedSshCleanupPending" | "ownedSshCleanupRetry")
    ));
    assert!(
        healthy && clear,
        "settled cleanup must reconnect without a configuration change or restart"
    );
    assert_ne!(successor.unwrap().incarnation, old_incarnation);
    assert_eq!(before.enrollment_generation, after.enrollment_generation);
    assert_eq!(before.endpoint, after.endpoint);
    assert_eq!(worker["stage"], "authenticated");
    assert_eq!(worker["quarantinedForRuntime"], false);
    assert!(worker["recoveryReason"].is_null());
}
