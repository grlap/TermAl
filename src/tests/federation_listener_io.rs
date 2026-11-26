// Owns listener connection pressure, request deadlines and shutdown controls.
// Does not use SSH or OS credentials. New transport lifecycle coverage.

use super::federation_link::{allowed, channel};
use super::*;

#[tokio::test]
async fn federation_shutdown_cancels_four_incomplete_header_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let connections = Arc::new(tokio::sync::Semaphore::new(4));
    let listener = FederationSocketListener {
        control: None,
        listener,
        connections: connections.clone(),
        stop: stop_rx.clone(),
    };
    let router = federation_router(channel(FederationRole::Satellite, 'a'), allowed());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
    });
    let mut clients = Vec::new();
    for _ in 0..5 {
        let mut client = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"POST /federation/v1/link HTTP/1.1\r\n")
            .unwrap();
        clients.push(client);
    }
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while connections.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send_replace(true);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(clients);
    assert_eq!(connections.available_permits(), 4);
}

// Waits until the listener is waiting on the clock for the next connection's
// preface: its deadline is then the only clock subscriber.
async fn preface_waits_on(clock: &FederationControlClock) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while clock.changed.receiver_count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn federation_preface_deadline_follows_the_scripted_control_clock() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut satellite = channel(FederationRole::Satellite, 'a');
    satellite.control.clock = FederationControlClock::scripted();
    let clock = satellite.control.clock.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let listener = FederationSocketListener {
        control: Some(satellite.clone()),
        listener,
        connections: Arc::new(tokio::sync::Semaphore::new(4)),
        stop: stop_rx.clone(),
    };
    let router = federation_router(satellite, allowed());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
    });

    // An accepted connection that has sent nothing ends, without a response,
    // once the scripted clock reaches the preface deadline.
    let mut silent = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    preface_waits_on(&clock).await;
    clock.advance(FEDERATION_PREFACE_DEADLINE);
    let mut buffer = [0_u8; 64];
    let ended = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, silent.read(&mut buffer))
        .await
        .unwrap();
    assert!(
        matches!(ended, Ok(0) | Err(_)),
        "a connection past its preface deadline must end without a response: {ended:?}"
    );

    // A connection whose preface arrives before the scripted deadline is
    // served: only the control clock's time counts against it.
    let mut late = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    preface_waits_on(&clock).await;
    clock.advance(FEDERATION_PREFACE_DEADLINE - Duration::from_millis(1));
    late.write_all(
        b"GET /federation/v1/health HTTP/1.1\r\nHost: satellite\r\nConnection: close\r\n\r\n",
    )
    .await
    .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, late.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 "),
        "a connection classified before its deadline must get an HTTP response: {:?}",
        String::from_utf8_lossy(&response)
    );

    stop.send_replace(true);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn federation_incomplete_authenticated_body_has_a_request_deadline() {
    let central = channel(FederationRole::Central, 'a');
    let mut satellite = channel(FederationRole::Satellite, 'a');
    let _proof = super::federation_control_fixture::attest(&central, &satellite).await;
    // The request deadline runs on logical time: the test steps past it
    // instead of waiting in real time.
    satellite.control.clock = FederationControlClock::scripted_io();
    let clock = satellite.control.clock.clone();
    let bound = satellite.bound.clone();
    let (stalled, stalled_rx) = tokio::sync::oneshot::channel::<()>();
    let stream = async_stream::stream! {
        yield Ok::<_, std::io::Error>("{");
        let _ = stalled.send(());
        std::future::pending::<()>().await;
    };
    let mut request = super::federation_control_fixture::signed(
        &central,
        1,
        "GET",
        "/federation/v1/health",
        Default::default(),
        b"{}",
    );
    *request.body_mut() = Body::from_stream(stream);
    let running = tokio::spawn(federation_router(satellite, allowed()).oneshot(request));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, stalled_rx)
        .await
        .unwrap()
        .unwrap();
    clock.advance(FEDERATION_HANDLER_DEADLINE);
    let response = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response.headers()["connection"], "close");
    assert_eq!(bound.permits.available_permits(), 4);
}
