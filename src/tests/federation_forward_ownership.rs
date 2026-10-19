// Owns squatted and relayed loopback-forward credential disclosure regressions.
// Does not run SSH or read secrets. Split from federation_runtime_fixture.rs.

use super::*;

async fn untrusted_forward(relay: bool) {
    let mut fixture = Fixture::new().await;
    let target = fixture
        .satellite
        .snapshot
        .endpoint
        .satellite_federation_port;
    let (observation, observation_rx) = tokio::sync::oneshot::channel();
    let observation = Arc::new(Mutex::new(Some(observation)));
    let attempt_observation = observation.clone();
    let previous = fixture.io.attempt_finished.clone();
    fixture.io.attempt_finished = Arc::new(move |result| {
        previous(result);
        if let Some(send) = attempt_observation.lock().unwrap().take() {
            let refused = result.as_ref().err().is_some_and(|error| {
                error.status == StatusCode::FORBIDDEN
                    && error.message == "unproven federation transport"
            });
            let _ = send.send((None, refused));
        }
    });
    let began = Arc::new(AtomicBool::new(false));
    let began_server = began.clone();
    let handle = tokio::runtime::Handle::current();
    let reserved = fixture.io.forward_listener.clone().unwrap();
    fixture.io.ssh = Arc::new(move |_, port, _| {
        // This endpoint is an unrelated local listener, even when it relays
        // valid responses from the genuine satellite. Child liveness alone
        // therefore cannot establish ownership of this TCP connection.
        let _entered = handle.enter();
        let socket = reserved
            .lock()
            .unwrap()
            .take()
            .expect("reserved forward socket");
        assert_eq!(socket.local_addr().unwrap().port(), port);
        socket.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(socket).unwrap();
        let observed = observation.clone();
        let router = Router::new().fallback(move |request: axum::extract::Request| {
            let observed = observed.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, 4096).await.unwrap();
                let secret = "a".repeat(64);
                let leaked = parts.headers.values().any(|value| {
                    value
                        .as_bytes()
                        .windows(64)
                        .any(|part| part == secret.as_bytes())
                }) || bytes.windows(64).any(|part| part == secret.as_bytes())
                    || parts.uri.to_string().contains(&secret);
                if let Some(send) = observed.lock().unwrap().take() {
                    let _ = send.send((Some(leaked), false));
                }
                if !relay {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let response = reqwest::Client::builder()
                    .no_proxy()
                    .build()
                    .unwrap()
                    .request(
                        parts.method,
                        format!("http://127.0.0.1:{target}{}", parts.uri),
                    )
                    .headers(parts.headers)
                    .body(bytes)
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                let body = response.bytes().await.unwrap();
                (status, body).into_response()
            }
        });
        let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
        let (exit_tx, exited) = mpsc::channel();
        handle.spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
                .await;
            let _ = exit_tx.send(());
        });
        began_server.store(true, Ordering::SeqCst);
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        Ok(Box::new(FakeSsh {
            control: None,
            stop,
            exited,
            events,
            cleanup_unknown: Arc::new(AtomicBool::new(false)),
        }))
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    let observed = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, observation_rx)
        .await
        .unwrap()
        .unwrap();
    fixture.finish(runtime).await;
    assert!(
        began.load(Ordering::SeqCst),
        "exercise the selected local forward endpoint"
    );
    assert!(
        observed == (Some(false), false) || observed == (None, true),
        "an unowned forward, including a transparent relay, must never receive the pair capability"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_forward_squatter_never_receives_pair_capability() {
    untrusted_forward(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_forward_relay_never_receives_pair_capability() {
    untrusted_forward(true).await;
}
