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

#[tokio::test]
async fn federation_incomplete_authenticated_body_has_a_request_deadline() {
    let central = channel(FederationRole::Central, 'a');
    let satellite = channel(FederationRole::Satellite, 'a');
    let bound = satellite.bound.clone();
    let stream = async_stream::stream! {
        yield Ok::<_, std::io::Error>("{");
        std::future::pending::<()>().await;
    };
    let request = Request::builder()
        .method("POST")
        .uri("/federation/v1/link")
        .header("content-type", "application/json")
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .header(
            FEDERATION_AUTHORITY_HEADER,
            serde_json::to_string(&FederationRequestAuthority::outgoing(&central.snapshot))
                .unwrap(),
        )
        .header(FEDERATION_INCARNATION_HEADER, Uuid::new_v4().to_string())
        .body(Body::from_stream(stream))
        .unwrap();
    let response = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        federation_router(satellite, allowed()).oneshot(request),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(response.headers()["connection"], "close");
    assert_eq!(bound.permits.available_permits(), 4);
}
