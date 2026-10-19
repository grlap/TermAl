// Owns delayed/misrouted reply tests for the real async federation client.
// Does not use SSH or live host state. New transport tests, not a code split.

use super::federation_control_fixture::{attest, authenticated_router, establish};
use super::federation_link::{allowed, channel};
use super::*;

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
