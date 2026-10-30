// Owns delayed/misrouted reply tests for the real async federation client.
// Does not use SSH or live host state. New transport tests, not a code split.

use super::federation_control_fixture::{attest, authenticated_router, establish};
use super::federation_link::{allowed, channel};
use super::*;

#[tokio::test]
async fn host_utilization_authenticated_read_failures_do_not_retire_peer_health() {
    let central = channel(FederationRole::Central, 'a');
    let (satellite, _proof) = establish(&central).await;
    let lease = central.current_lease().unwrap();
    let mode = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_mode = mode.clone();
    let handler_release = release.clone();
    let router = Router::new().route("/federation/v1/health", get(federation_health))
        .route("/federation/v1/host-utilization", get(move || {
            let mode = handler_mode.clone();
            let release = handler_release.clone();
            async move {
                match mode.load(Ordering::SeqCst) {
                    0 => Json(json!({"counterBytes": 1234})).into_response(),
                    1 => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    2 => { release.notified().await; Json(json!({"late": true})).into_response() },
                    3 => "malformed snapshot".into_response(),
                    _ => "x".repeat(host_utilization::RESPONSE_BYTES + 1).into_response(),
                }
            }
        }));
    let router = authenticated_router(satellite, router);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await;
        }).await.unwrap();
    });
    let rpc = FederationRpcClient::new(port, central.clone(), allowed()).unwrap();
    let value: Value = rpc.request(FederationRpcRoute::HostUtilization(lease.clone())).await.unwrap();
    assert_eq!(value["counterBytes"], 1234);
    for failure_mode in 1..=4 {
        mode.store(failure_mode, Ordering::SeqCst);
        assert!(rpc.request::<Value>(FederationRpcRoute::HostUtilization(lease.clone())).await.is_err());
        assert!(lease.with_incarnation(|| Ok(())).is_ok());
        rpc.health(&lease).await.expect("telemetry failure must not retire the authenticated pair");
        let context = central.security_context().unwrap();
        assert!(context.sender.lock().unwrap().outstanding.is_empty(), "read deadline must settle its counter");
        if failure_mode == 2 { release.notify_one(); }
    }
    stop.send_replace(true);
    server.await.unwrap();
}

#[tokio::test]
async fn host_utilization_refresh_cap_keeps_six_enrolled_peers_and_does_not_overlap() {
    let app = super::federation_registry::central();
    let (released, release_rx) = tokio::sync::watch::channel(false);
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut servers = Vec::new();
    let mut proofs = Vec::new();
    let mut peer_ids = HashSet::new();
    // Enrolled channels require distinct central forward ports. The actual
    // loopback listeners below independently bind ephemeral OS ports.
    for central_port in 47102..47108 {
        let record = super::federation_registry::peer(central_port);
        let snapshot = super::federation_registry::connected(&app, record);
        let peer_id = snapshot.peer.host_id.clone();
        peer_ids.insert(peer_id.clone());
        let channel = FederationChannel::new(snapshot, FederationPairCapability::new("a".repeat(64)).unwrap());
        let (satellite, proof) = establish(&channel).await;
        proofs.push(proof);
        let send = entered.clone();
        let active_requests = active.clone();
        let maximum_requests = maximum.clone();
        let release = release_rx.clone();
        let route_id = peer_id.clone();
        let router = Router::new().route("/federation/v1/host-utilization", get(move || {
            let send = send.clone();
            let active = active_requests.clone();
            let maximum = maximum_requests.clone();
            let mut release = release.clone();
            let id = route_id.clone();
            async move {
                struct Active(Arc<std::sync::atomic::AtomicUsize>);
                impl Drop for Active {
                    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); }
                }
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                let _active = Active(active);
                maximum.fetch_max(count, Ordering::SeqCst);
                let _ = send.send(id);
                if !*release.borrow_and_update() { let _ = release.changed().await; }
                // A signed application refusal still completes the observation
                // attempt; no executable or test-run action is involved.
                StatusCode::SERVICE_UNAVAILABLE
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut stopped = stop_rx.clone();
        servers.push(tokio::spawn(async move {
            axum::serve(listener, authenticated_router(satellite, router))
                .with_graceful_shutdown(async move { federation_wait_stop(&mut stopped).await })
                .await.unwrap();
        }));
        let rpc = FederationRpcClient::new(port, channel.clone(), allowed()).unwrap();
        app.federation_peer_handles.lock().unwrap().insert(peer_id, Arc::new(FederationPeerHandle { channel, rpc }));
    }
    let runtime = host_utilization::Runtime::start(app.clone()).unwrap();
    let first = tokio::time::timeout(Duration::from_secs(5), async {
        let mut ids = HashSet::new();
        for _ in 0..4 { ids.insert(entries.recv().await.unwrap()); }
        ids
    }).await;
    let before_release = active.load(Ordering::SeqCst);
    released.send_replace(true);
    let remaining = if first.is_ok() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut ids = HashSet::new();
            for _ in 0..2 { ids.insert(entries.recv().await.unwrap()); }
            ids
        }).await.ok()
    } else { None };
    runtime.shutdown().await;
    stop.send_replace(true);
    for server in servers { server.await.unwrap(); }
    drop(proofs);
    let first = first.expect("first bounded refresh batch must enter");
    assert_eq!(first.len(), 4, "one request per peer");
    assert_eq!(before_release, 4);
    assert_eq!(maximum.load(Ordering::SeqCst), 4);
    let all: HashSet<_> = first.into_iter().chain(remaining.expect("remaining peers must receive service")).collect();
    assert_eq!(all, peer_ids);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn federation_delayed_wire_reply_is_refused_after_reconnect() {
    let central = channel(FederationRole::Central, 'a');
    let (satellite, _proof) = establish(&central).await;
    let old = central.current_lease().unwrap();
    let old_health = FederationHealth {
        protocol_version: 1,
        host_id: central.snapshot.peer.host_id.clone(),
        host_generation: central.snapshot.peer.host_generation,
        enrollment_generation: central.snapshot.enrollment_generation,
        link_incarnation: old.incarnation.clone(),
    };
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_release = release.clone();
    let router = Router::new().route(
        "/federation/v1/health",
        get(move |headers: axum::http::HeaderMap| {
            let entered = entered.clone();
            let release = handler_release.clone();
            let health = old_health.clone();
            async move {
                assert!(headers.contains_key(FEDERATION_AUTHORITY_HEADER));
                assert!(headers.contains_key(FEDERATION_INCARNATION_HEADER));
                assert!(!headers.contains_key(FEDERATION_CAPABILITY_HEADER));
                assert!(headers.contains_key(FEDERATION_MAC_HEADER));
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                release.notified().await;
                Json(health)
            }
        }),
    );
    let router = authenticated_router(satellite.clone(), router);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
            .unwrap();
    });
    let rpc = FederationRpcClient::new(port, central.clone(), allowed()).unwrap();
    let request = tokio::spawn(async move { rpc.health(&old).await });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    central.invalidate();
    let _successor = attest(&central, &satellite).await;
    release.notify_one();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    stop.send_replace(true);
    server.await.unwrap();
}

#[tokio::test]
async fn federation_redirect_does_not_forward_pair_credentials() {
    let central = channel(FederationRole::Central, 'a');
    let (_satellite, _proof) = establish(&central).await;
    let lease = central.current_lease().unwrap();
    let received = Arc::new(AtomicBool::new(false));
    let received_handler = received.clone();
    let health = FederationHealth {
        protocol_version: 1,
        host_id: central.snapshot.peer.host_id.clone(),
        host_generation: central.snapshot.peer.host_generation,
        enrollment_generation: central.snapshot.enrollment_generation,
        link_incarnation: lease.incarnation.clone(),
    };
    let sink = Router::new().route(
        "/api/state",
        get(move || {
            let received = received_handler.clone();
            let health = health.clone();
            async move {
                received.store(true, Ordering::SeqCst);
                Json(health)
            }
        }),
    );
    let sink_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = format!("http://{}/api/state", sink_listener.local_addr().unwrap());
    let (sink_stop, mut sink_stop_rx) = tokio::sync::watch::channel(false);
    let sink_server = tokio::spawn(async move {
        axum::serve(sink_listener, sink)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut sink_stop_rx).await })
            .await
            .unwrap();
    });
    let router = Router::new().route(
        "/federation/v1/health",
        get(move || {
            let target = target.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [("location", target)]) }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
            .unwrap();
    });
    let rpc = FederationRpcClient::new(port, central, allowed()).unwrap();
    assert_eq!(
        rpc.health(&lease).await.unwrap_err().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!received.load(Ordering::SeqCst));
    stop.send_replace(true);
    server.await.unwrap();
    sink_stop.send_replace(true);
    sink_server.await.unwrap();
}
