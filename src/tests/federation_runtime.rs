// Owns production worker regressions for liveness, authority and cleanup.
// Does not access OS credentials or launch SSH. New tests beside federation_rpc.

include!("federation_runtime_fixture.rs");

#[path = "federation_worker_io.rs"]
mod worker_io;

#[path = "federation_connect_durability.rs"]
mod connect_durability;

#[path = "federation_settlement.rs"]
mod settlement;

#[path = "federation_mailbox_pressure.rs"]
mod mailbox_pressure;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_production_monitor_recovers_remote_incarnation_loss() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    let old = runtime
        .peer(&fixture.id)
        .unwrap()
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    let handle = runtime.peer(&fixture.id).unwrap();
    handle
        .rpc
        .health(&handle.channel.current_lease().unwrap())
        .await
        .unwrap();
    // Models the satellite monitor's failed reverse probe or satellite-only
    // restart while the central's owned SSH child remains alive.
    fixture.satellite.invalidate();
    let reopened = fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("link") && status.is_success())).await;
    let successor = runtime
        .peer(&fixture.id)
        .and_then(|peer| peer.channel.current_lease().ok())
        .map(|lease| lease.incarnation);
    fixture.finish(runtime).await;
    assert!(
        reopened,
        "central must re-handshake without waiting for SSH exit"
    );
    assert_ne!(successor.as_deref(), Some(old.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_initial_durability_hold_retries_after_settlement() {
    let mut fixture = Fixture::new().await;
    fixture.hold.store(true, Ordering::SeqCst);
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Credentials)).await);
    assert!(
        fixture
            .event(|e| matches!(e, Event::Attempt(StatusCode::CONFLICT)))
            .await
    );
    fixture.app.inner.lock().unwrap().federation_durable = true;
    let connected = fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await;
    fixture.finish(runtime).await;
    assert!(
        connected,
        "a settled hold must not require configuration change"
    );
}

#[tokio::test]
async fn federation_disabled_and_standalone_start_without_os_effects() {
    let app = federation_registry::domain_state();
    let touched = Arc::new(AtomicBool::new(false));
    let reader_touched = touched.clone();
    let ssh_touched = touched.clone();
    let (checked, mut checked_rx) = tokio::sync::mpsc::unbounded_channel();
    let io = FederationRuntimeIo {
        credentials: Arc::new(move |_| {
            reader_touched.store(true, Ordering::SeqCst);
            Err(federation_credential_error())
        }),
        ssh: Arc::new(move |_, _, _| {
            ssh_touched.store(true, Ordering::SeqCst);
            Err(federation_transport_unavailable())
        }),
        reconcile_interval: Duration::from_millis(10),
        probe_interval: Duration::from_millis(10),
        operation_timeout: Duration::from_secs(5),
        attempt_finished: Arc::new(|_| {}),
        reconciled: Arc::new(move |count| {
            let _ = checked.send(count);
        }),
        connection_published: Arc::new(|| {}),
    };
    for state in [app, {
        let disabled = federation_registry::central();
        let mut settings = disabled
            .inner
            .lock()
            .unwrap()
            .preferences
            .federation
            .clone();
        settings.enabled = false;
        disabled.configure_federation(settings).unwrap();
        disabled
    }] {
        let runtime = start_federation_runtime_with_io(state, io.clone());
        assert_eq!(
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, checked_rx.recv())
                .await
                .unwrap(),
            Some(0)
        );
        assert!(runtime.peers.lock().unwrap().is_empty());
        runtime.shutdown().await;
    }
    assert!(!touched.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_shutdown_closes_an_authenticated_incomplete_body() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    let peer = runtime.peer(&fixture.id).unwrap();
    let mut stream = std::net::TcpStream::connect((
        "127.0.0.1",
        peer.channel.snapshot.endpoint.central_federation_port,
    ))
    .unwrap();
    let authority = serde_json::to_string(&FederationRequestAuthority::outgoing(
        &fixture.satellite.snapshot,
    ))
    .unwrap();
    write!(stream,
        "POST /federation/v1/link HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n{FEDERATION_CAPABILITY_HEADER}: {}\r\n{FEDERATION_AUTHORITY_HEADER}: {authority}\r\n{FEDERATION_INCARNATION_HEADER}: {}\r\n\r\n{{",
        "a".repeat(64), Uuid::new_v4()).unwrap();
    stream.flush().unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while peer.channel.bound.permits.available_permits() == 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut shutdown = tokio::spawn(runtime.shutdown());
    // A deadlock guard, not a throughput assertion. Closing the caller below
    // also lets the RED implementation unwind without leaving a server alive.
    let settled = tokio::time::timeout(Duration::from_secs(10), &mut shutdown)
        .await
        .is_ok();
    drop(stream);
    if !settled {
        shutdown.await.unwrap();
    }
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(
        settled,
        "an incomplete body must not hold host shutdown indefinitely"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_established_failure_keeps_observed_credential_fence() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    fixture.fingerprint.store(2, Ordering::SeqCst);
    fixture.fail_health.store(true, Ordering::SeqCst);
    assert!(fixture.event(|e| matches!(e, Event::Reaped)).await);
    let refused_material = fixture
        .event(|e| matches!(e, Event::Attempt(StatusCode::CONFLICT)))
        .await;
    fixture.finish(runtime).await;
    assert!(
        refused_material,
        "a retry must retain material observed before its own connect committed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_transport_replacement_reaps_old_child_first() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    let mut record = fixture.app.inner.lock().unwrap().federation.peers[&fixture.id].clone();
    record.endpoint.ssh_user = "replacement".to_owned();
    fixture.app.enroll_federation_peer(record).unwrap();
    assert!(fixture.event(|e| matches!(e, Event::Reaped)).await);
    let replaced = fixture.event(|e| matches!(e, Event::Started)).await;
    fixture.finish(runtime).await;
    assert!(
        replaced,
        "replacement may start only after exact old child reap"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_unknown_cleanup_quarantines_instead_of_replacing() {
    let mut fixture = Fixture::new().await;
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(fixture.event(|e| matches!(e, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await);
    fixture.cleanup_unknown.store(true, Ordering::SeqCst);
    fixture.fail_health.store(true, Ordering::SeqCst);
    let classified = fixture
        .event(|e| matches!(e, Event::Attempt(StatusCode::INTERNAL_SERVER_ERROR)))
        .await;
    // The test observer runs after the worker installs quarantine. Completed
    // shutdown joins that worker and makes the final state stable.
    let quarantined = runtime.quarantined.clone();
    let id = fixture.id.clone();
    fixture.finish(runtime).await;
    assert!(classified);
    assert!(quarantined.lock().unwrap().contains(&id));
}
