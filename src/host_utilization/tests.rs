// Snapshot, freshness and read-only endpoint witnesses. Native collectors have
// platform smoke coverage here and arithmetic/parser controls beside their owner.
use super::*;
use tower::ServiceExt;

fn identity() -> FederationHostIdentity {
    FederationHostIdentity {
        host_id: "4fc1417b-a6db-4584-8cd2-61bff273f6b2".to_owned(),
        host_generation: 1,
    }
}
fn snapshot() -> Snapshot {
    let mut memory = Memory::unavailable();
    memory.total_bytes = Metric::available(1024);
    memory.in_use_bytes = Metric::available(512);
    memory.reclaimable_cache_bytes = Metric::available(256);
    memory.free_bytes = Metric::available(256);
    memory.available_bytes = Metric::available(500);
    memory.cache_definition = "fixture native cache".to_owned();
    Snapshot {
        schema_version: 1,
        host: Some(identity()),
        os: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
        measured_at: "2099-01-01T00:00:00.000Z".to_owned(),
        source_age_ms: 0,
        stale_after_ms: 30_000,
        cpu: Cpu {
            logical_count: Metric::available(8),
            busy_percent: Metric::available(25.0),
            window_ms: Metric::available(5000),
            load_average_1_5_15: Metric::available([1.0, 2.0, 3.0]),
        },
        memory,
        volumes: VolumeCollection {
            data: Metric::available(vec![Volume {
                identity: "/".to_owned(),
                total_bytes: Metric::available(1_000_000),
                free_bytes: Metric::available(200_000),
                available_to_caller_bytes: Metric::available(190_000),
            }]),
            complete: true,
            failed_path_count: 0,
            unavailable_required_path_count: 0,
            omitted_path_count_at_least: 0,
            failure_reasons: Vec::new(),
        },
        network: Metric::available(vec![Interface {
            identity: "1:lo".to_owned(),
            received_bytes: Metric::available(2048),
            sent_bytes: Metric::available(1024),
            counter_measured_at: "2099-01-01T00:00:00.000Z".to_owned(),
            received_bytes_per_second: Metric::available(100.0),
            sent_bytes_per_second: Metric::available(50.0),
            window_ms: Metric::available(5000),
        }]),
    }
}
fn peer() -> Peer {
    Peer {
        identity: identity(),
        os: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
        enrollment_generation: 1,
        transport_generation: 1,
        connected: true,
    }
}
fn peer_cache(now: std::time::Instant) -> Cache {
    let row = peer();
    Cache {
        image: RwLock::new(Image {
            central: Some(true),
            roster_observed: Some(now),
            roster_total: 2,
            roster_complete: true,
            peers: BTreeMap::from([(
                row.identity.host_id.clone(),
                PeerSample {
                    peer: row,
                    sample: None,
                    reason: None,
                },
            )]),
            ..Image::default()
        }),
        sampler_running: AtomicBool::new(false),
    }
}

#[test]
fn utilization_shape_units_and_validation_are_explicit() {
    let snapshot = snapshot();
    assert!(snapshot.validate(&identity()));
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["memory"]["totalBytes"]["value"], 1024);
    assert_eq!(wire["cpu"]["busyPercent"]["value"], 25.0);
    assert_eq!(wire["cpu"]["windowMs"]["value"], 5000);
    assert_eq!(
        wire["network"]["value"][0]["receivedBytesPerSecond"]["value"],
        100.0
    );
    assert_eq!(wire["memory"]["commitUsedBytes"]["state"], "unsupported");
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < RESPONSE_BYTES);
    let mut bad = snapshot.clone();
    bad.cpu.busy_percent.value = None;
    assert!(
        !bad.validate(&identity()),
        "available without a value must be refused"
    );
    bad = snapshot.clone();
    bad.network.value.as_mut().unwrap()[0]
        .sent_bytes_per_second
        .value = Some(-1.0);
    assert!(!bad.validate(&identity()));
    bad = snapshot;
    bad.volumes
        .data
        .value
        .as_mut()
        .unwrap()
        .extend((0..MAX_VOLUMES).map(|_| Volume {
            identity: "/other".to_owned(),
            total_bytes: Metric::available(1),
            free_bytes: Metric::available(0),
            available_to_caller_bytes: Metric::available(0),
        }));
    assert!(!bad.validate(&identity()));
}

#[test]
fn utilization_receipt_never_revives_source_stale_or_uses_remote_wall_time() {
    let now = std::time::Instant::now();
    let cache = peer_cache(now);
    let mut old = snapshot();
    old.source_age_ms = 31_000;
    runtime::publish_peer(&cache, &peer(), Ok(old), now);
    let view = cache.aggregate(now).unwrap();
    let peer = &view.hosts[1];
    assert_eq!(peer.state, Status::Stale);
    let sample = peer.snapshot.as_ref().unwrap();
    assert_eq!(sample.cpu.busy_percent.state, Status::Stale);
    assert_eq!(sample.source_age_ms, 31_000);
    assert_eq!(sample.measured_at, "2099-01-01T00:00:00.000Z");
    assert!(peer.received_at.is_some());
    let mut source_stale = snapshot();
    source_stale.cpu.busy_percent.state = Status::Stale;
    runtime::publish_peer(&cache, &self::peer(), Ok(source_stale), now);
    assert_eq!(cache.aggregate(now).unwrap().hosts[1].state, Status::Stale);
}

#[test]
fn utilization_disconnected_host_keeps_last_known_stale_and_unknown_stays_missing() {
    let now = std::time::Instant::now();
    let cache = peer_cache(now);
    let initial = cache.aggregate(now).unwrap();
    assert_eq!(
        initial.hosts.len(),
        2,
        "include unsampled local and peer identities"
    );
    assert_eq!(initial.hosts[1].state, Status::Unavailable);
    assert!(initial.hosts[1].snapshot.is_none());
    runtime::publish_peer(&cache, &peer(), Ok(snapshot()), now);
    {
        let mut image = cache.image.write().unwrap();
        image
            .peers
            .get_mut(&identity().host_id)
            .unwrap()
            .peer
            .connected = false;
    }
    let disconnected = cache.aggregate(now).unwrap();
    assert_eq!(disconnected.hosts[1].state, Status::Unavailable);
    assert_eq!(disconnected.hosts[1].reason, Some(Reason::PeerDisconnected));
    let last = disconnected.hosts[1].snapshot.as_ref().unwrap();
    assert_eq!(last.memory.total_bytes.value, Some(1024));
    assert_eq!(last.memory.total_bytes.state, Status::Stale);
    assert_eq!(
        last.network.value.as_ref().unwrap()[0].received_bytes.state,
        Status::Stale
    );
    assert_eq!(last.memory.commit_used_bytes.state, Status::Unsupported);
    assert_eq!(
        cache
            .aggregate(now + STALE_AFTER + Duration::from_secs(1))
            .unwrap()
            .roster_state,
        Status::Stale
    );
}

#[test]
fn utilization_bad_peer_result_retains_stale_sample_and_replaced_identity_cannot_publish() {
    let now = std::time::Instant::now();
    let cache = peer_cache(now);
    runtime::publish_peer(&cache, &peer(), Ok(snapshot()), now);
    runtime::publish_peer(&cache, &peer(), Err(Reason::PeerUnavailable), now);
    let view = cache.aggregate(now).unwrap();
    assert_eq!(view.hosts[1].state, Status::Unavailable);
    assert_eq!(
        view.hosts[1]
            .snapshot
            .as_ref()
            .unwrap()
            .cpu
            .logical_count
            .state,
        Status::Stale
    );
    let mut replacement = peer();
    replacement.transport_generation += 1;
    runtime::publish_peer(&cache, &replacement, Ok(snapshot()), now);
    assert_eq!(
        cache.aggregate(now).unwrap().hosts[1].reason,
        Some(Reason::PeerUnavailable)
    );
}

#[tokio::test]
async fn utilization_endpoints_are_cached_read_only_even_while_state_is_held() {
    let app = crate::tests::test_app_state();
    {
        let mut image = app.host_utilization.image.write().unwrap();
        image.central = Some(true);
        image.roster_observed = Some(std::time::Instant::now());
        image.roster_complete = true;
        image.roster_total = 1;
        image.local = Some(Stored {
            snapshot: snapshot(),
            observed: std::time::Instant::now(),
            received_at: utc_now(),
        });
    }
    let router = crate::app_router(app.clone());
    let guard = app.inner.lock().unwrap();
    let revision = guard.revision;
    for uri in ["/api/host-utilization", "/api/federation/host-utilization"] {
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            router.clone().oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("cached GET must not wait for StateInner")
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(!bytes.is_empty());
    }
    assert_eq!(guard.revision, revision);
    drop(guard);
    for uri in ["/api/host-utilization", "/api/federation/host-utilization"] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .method("POST")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    let locked = app.host_utilization.image.write().unwrap();
    assert_eq!(
        app.host_utilization
            .local(std::time::Instant::now())
            .unwrap_err()
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(locked);
}

#[test]
fn utilization_native_first_sample_and_platform_unsupported_markers() {
    let app = crate::tests::test_app_state();
    let mut collector = native::Collector::new();
    let snapshot = collector.sample(
        Some(identity()),
        &native::VolumePaths {
            required: vec![app.persistence_path.parent().unwrap().to_path_buf()],
            additional: Vec::new(),
            omitted_at_least: 0,
        },
    );
    assert!(snapshot.validate(&identity()));
    assert_eq!(snapshot.cpu.busy_percent.state, Status::Unavailable);
    assert_eq!(snapshot.cpu.window_ms.state, Status::Unavailable);
    assert_eq!(snapshot.cpu.logical_count.state, Status::Available);
    assert!(snapshot.cpu.logical_count.value.unwrap() > 0);
    assert_eq!(snapshot.memory.total_bytes.state, Status::Available);
    assert!(snapshot.memory.total_bytes.value.unwrap() > 0);
    assert_eq!(snapshot.volumes.data.state, Status::Available);
    assert_eq!(
        snapshot.network.state,
        Status::Available,
        "native interface probe: {:?}",
        snapshot.network.reason
    );
    if let Some(interfaces) = snapshot.network.value {
        #[cfg(target_os = "linux")]
        {
            assert!(
                !interfaces.is_empty(),
                "Linux native probe must observe an active interface"
            );
            println!("Linux native active interfaces: {}", interfaces.len());
        }
        for interface in interfaces {
            assert_eq!(
                interface.received_bytes_per_second.reason,
                Some(Reason::FirstSample)
            );
            assert_eq!(interface.sent_bytes_per_second.value, None);
        }
    }
    #[cfg(windows)]
    {
        assert_eq!(snapshot.cpu.load_average_1_5_15.state, Status::Unsupported);
        assert_eq!(snapshot.memory.commit_limit_bytes.state, Status::Available);
        assert_eq!(snapshot.memory.compressed_bytes.state, Status::Unsupported);
    }
    #[cfg(unix)]
    {
        assert_eq!(snapshot.memory.commit_used_bytes.state, Status::Unsupported);
        assert_eq!(snapshot.cpu.load_average_1_5_15.state, Status::Available);
    }
    #[cfg(target_os = "macos")]
    assert_eq!(snapshot.memory.available_bytes.state, Status::Unsupported);
}

async fn wait_for_sampler_exit(app: &AppState) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while app.host_utilization.sampler_running.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("only the original sampler exit releases occupancy");
}

#[tokio::test]
async fn utilization_owns_only_one_sampler_until_its_stop_aware_thread_exits() {
    let app = crate::tests::test_app_state();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let runtime = Runtime::start_with_sampler(app.clone(), move |_, stopped| {
        let _ = observed_tx.send(stopped.recv().is_ok());
    })
    .unwrap();
    assert!(app.host_utilization.sampler_running.load(Ordering::SeqCst));
    assert!(
        matches!(Runtime::start(app.clone()), Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists)
    );
    runtime.shutdown().await;
    assert!(
        observed_rx
            .await
            .expect("sampler reports its stop observation")
    );
    wait_for_sampler_exit(&app).await;
    assert!(!app.host_utilization.sampler_running.load(Ordering::SeqCst));
}

#[tokio::test]
async fn utilization_shutdown_leaves_a_blocked_sampler_owned_until_its_actual_exit() {
    let app = crate::tests::test_app_state();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let runtime = Runtime::start_with_sampler(app.clone(), move |app, stopped| {
        let cache = app.host_utilization.clone();
        let mut entered_tx = Some(entered_tx);
        let mut observed_tx = Some(observed_tx);
        // Run the production stop/publication boundary with a blocked probe.
        runtime::sample_loop_with_probe(app, stopped, move |host, _| {
            entered_tx.take().unwrap().send(()).unwrap();
            release_rx.recv().unwrap();
            let _ = observed_tx
                .take()
                .unwrap()
                .send(cache.sampler_running.load(Ordering::SeqCst));
            let mut sample = snapshot();
            // Keep publication eligible so only the pending stop prevents it.
            sample.host = host;
            sample
        });
    })
    .unwrap();
    entered_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runtime.shutdown())
        .await
        .expect("observational shutdown must return while the probe is blocked");
    assert!(app.host_utilization.sampler_running.load(Ordering::SeqCst));
    assert!(matches!(Runtime::start(app.clone()), Err(error)
        if error.kind() == std::io::ErrorKind::AlreadyExists));
    // This is the host's next shutdown operation. It remains reachable while
    // the observation thread is still blocked, with no Tokio blocking join.
    app.shutdown_persist_blocking();
    release_tx.send(()).unwrap();
    assert!(
        observed_rx
            .await
            .expect("probe observations must reach the test")
    );
    wait_for_sampler_exit(&app).await;
    assert!(
        app.host_utilization.image.read().unwrap().local.is_none(),
        "a probe returning after stop must not publish its snapshot"
    );
    let replacement = Runtime::start_with_sampler(app.clone(), |_, _| {}).unwrap();
    replacement.shutdown().await;
    wait_for_sampler_exit(&app).await;
    assert!(!app.host_utilization.sampler_running.load(Ordering::SeqCst));
}

#[test]
fn utilization_partial_volumes_keep_metadata_through_wire_cache_and_freshness() {
    let now = std::time::Instant::now();
    let mut sample = snapshot();
    sample.volumes.complete = false;
    sample.volumes.failed_path_count = 1;
    sample.volumes.failure_reasons = vec![Reason::ProbeFailed];
    let wire = serde_json::to_vec(&sample).unwrap();
    let round_trip: Snapshot = serde_json::from_slice(&wire).unwrap();
    assert!(round_trip.validate(&identity()));
    let cache = peer_cache(now);
    runtime::publish_peer(&cache, &peer(), Ok(round_trip.clone()), now);
    let aggregate = cache.aggregate(now).unwrap();
    let retained = aggregate.hosts[1].snapshot.as_ref().unwrap();
    assert_eq!(
        retained.volumes.data.value.as_ref().unwrap()[0]
            .total_bytes
            .value,
        Some(1_000_000)
    );
    assert!(!retained.volumes.complete);
    assert_eq!(retained.volumes.failed_path_count, 1);
    let mut local = round_trip;
    local.age(Duration::from_secs(31), false);
    assert!(local.validate(&identity()));
    assert_eq!(local.volumes.data.state, Status::Stale);
    assert_eq!(
        local.volumes.data.value.as_ref().unwrap()[0]
            .total_bytes
            .state,
        Status::Stale
    );
    assert!(!local.volumes.complete);
    assert_eq!(local.volumes.failure_reasons, [Reason::ProbeFailed]);
    local.volumes.complete = true;
    assert!(
        !local.validate(&identity()),
        "a partial peer result cannot claim completeness"
    );
}
