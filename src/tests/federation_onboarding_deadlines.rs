// Owns deterministic expiry witnesses for onboarding's real transport failures.
// Does not change budgets or infer a bad key from an elapsed proof deadline.
use super::federation_onboarding_fixture::Pair;
use super::*;
use std::future::Future;
use std::task::Poll;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn federation_onboarding_proof_expiry_maps_to_authentication_refusal() {
    let pair = Pair::unattested(federation_registry::central()).await;
    let mut channel = pair.rpc.channel.clone();
    channel.control.clock = FederationControlClock::scripted_io();
    let clock = channel.control.clock.clone();
    let (central_io, mut satellite_io) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut central_io: FederationControlIo = Box::new(central_io);
    let mut attempt = Box::pin(federation_attest_central(&channel, &mut central_io));
    let mut peer = Box::pin(async {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        satellite_io.read_exact(&mut preface).await.unwrap();
        assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
        federation_write_frame(
            &mut satellite_io,
            &federation_encode(
                b"termal-hello-s-v1",
                &[
                    channel.snapshot.peer.host_id.as_bytes(),
                    &channel.snapshot.enrollment_generation.to_be_bytes(),
                    Uuid::new_v4().to_string().as_bytes(),
                    &[7; 32],
                ],
            ),
        )
        .await
        .unwrap();
        // Receipt of C's challenge proves startup completed and its proof
        // budget is running. Keep the stream open without sending tag_s.
        federation_read_frame(&mut satellite_io).await.unwrap();
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::select! {
            biased;
            () = &mut peer => {},
            _ = &mut attempt => panic!("proof completed before the withheld peer tag"),
        }
    })
    .await
    .unwrap();
    clock.advance(channel.control.attest_timeout);
    let result = std::future::poll_fn(|cx| Poll::Ready(attempt.as_mut().poll(cx))).await;
    let error = match result {
        Poll::Ready(Err(error)) => Some(error),
        _ => None,
    };
    drop(attempt);
    drop(peer);
    let observed = pair.central.inner.lock().unwrap().federation_attestation[&pair.id].project();
    pair.finish().await;
    let error = error.expect("logical proof expiry must be observed without waiting for wall time");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "federation message authentication refused");
    assert_eq!(observed["lastStage"], "attest");
    assert_eq!(observed["lastReason"], "attestExpired");
}

#[tokio::test]
async fn federation_onboarding_rpc_expiry_fences_the_link_before_the_next_request() {
    rpc_expiry(false).await;
}

#[tokio::test]
async fn federation_onboarding_receiver_expiry_also_maps_to_a_stale_next_link() {
    rpc_expiry(true).await;
}

async fn rpc_expiry(expire_receiver: bool) {
    let pair = Pair::new(federation_registry::central(), federation_onboarding_fixture::io()).await;
    let mut channel = pair.lease.channel.clone();
    channel.control.clock = FederationControlClock::scripted_io();
    let clock = channel.control.clock.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_entered = entered.clone();
    let handler_release = release.clone();
    let router = federation_control_fixture::authenticated_router(
        pair.satellite_channel.clone(),
        Router::new().route("/federation/v1/health", get(move || {
            let entered = handler_entered.clone();
            let release = handler_release.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                StatusCode::NO_CONTENT
            }
        })),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stopped).await })
            .await
            .unwrap();
    });
    let checked = channel.clone();
    let client = FederationRpcClient::new(
        port,
        channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let lease = channel.current_lease().unwrap();
    let mut request = Box::pin(client.authenticated_health(&lease));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::select! {
            biased;
            () = entered.notified() => {},
            _ = &mut request => panic!("request completed before the held authenticated handler"),
        }
    })
    .await
    .unwrap();
    let error = if expire_receiver {
        pair.satellite_channel.control.clock.advance(Duration::from_secs(5));
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, &mut request)
            .await
            .unwrap()
            .err()
    } else {
        clock.advance(Duration::from_secs(10));
        match std::future::poll_fn(|cx| Poll::Ready(request.as_mut().poll(cx))).await {
            Poll::Ready(Err(error)) => Some(error),
            _ => None,
        }
    };
    drop(request);
    let next_error = if error.is_some() {
        client.authenticated_health(&lease).await.err()
    } else {
        None
    };
    let receiver_fenced = pair.satellite_channel.current_lease().is_err();
    release.notify_waiters();
    stop.send_replace(true);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, server).await.unwrap().unwrap();
    pair.finish().await;
    let error = error.expect("logical request expiry must fence without waiting for wall time");
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    let next_error = next_error.expect("a fenced lease must refuse the next request locally");
    assert_eq!(next_error.status, StatusCode::CONFLICT);
    assert_eq!(receiver_fenced, expire_receiver);
    assert_eq!(
        next_error.message,
        "federation link changed; reconnect and reconcile the original operation"
    );
}
