// Owns real-client sender skew, authenticated settlement and counter-wrap controls.
// Does not implement the sender window or issue new application intents. Split from security requests.

use super::*;

#[tokio::test]
async fn federation_security_cancelled_request_retains_floor_until_bounded_fence() {
    let pair = Pair::established().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    let next = context.sender.lock().unwrap().next;
    pair.relay.hold_counter.store(next, Ordering::SeqCst);
    let client = pair.central_peer.rpc.clone();
    let old = lease.clone();
    let request = tokio::spawn(async move { client.health(&old).await });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = pair.relay.changed.notified();
            if pair
                .relay
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.counter == Some(next))
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(
        context
            .sender
            .lock()
            .unwrap()
            .outstanding
            .contains_key(&next),
        "dropping the caller is not authenticated settlement"
    );
    pair.central_clock.advance(Duration::from_secs(11));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while lease.with_incarnation(|| Ok(())).is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("an abandoned allocation independently fences within its unchanged request deadline");
    pair.finish().await;
}

#[tokio::test]
async fn federation_security_sender_exhaustion_fences_before_counter_wrap() {
    let pair = Pair::established().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    context.sender.lock().unwrap().next = u64::MAX;
    let observed = pair.relay.requests.lock().unwrap().len();
    let refused = pair.central_peer.rpc.health(&lease).await;
    let sent = pair.relay.requests.lock().unwrap().len();
    let fenced = lease.with_incarnation(|| Ok(())).is_err();
    pair.finish().await;
    assert!(refused.is_err() && fenced);
    assert_eq!(
        observed, sent,
        "an exhausted sender never transmits a wrapped counter"
    );
}

#[tokio::test]
async fn federation_security_sender_waits_at_64_until_oldest_authenticated_response() {
    let pair = Pair::established().await;
    let oldest = pair
        .relay
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| request.counter)
        .max()
        .unwrap()
        + 1;
    pair.relay.hold_counter.store(oldest, Ordering::SeqCst);
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let old_client = pair.central_peer.rpc.clone();
    let old_lease = lease.clone();
    let held = tokio::spawn(async move { old_client.health(&old_lease).await });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = pair.relay.changed.notified();
            if pair
                .relay
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| request.counter == Some(oldest))
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    // The other three slots can complete repeatedly, but cannot overtake
    // the still outstanding oldest request by 64 counter positions.
    for _ in 0..63 {
        pair.central_peer.rpc.health(&lease).await.unwrap();
    }
    let (blocked, blocked_rx) = tokio::sync::oneshot::channel();
    let blocked = Arc::new(Mutex::new(Some(blocked)));
    let mut client = pair.central_peer.rpc.clone();
    client.allocation_blocked = Arc::new(move |floor| {
        if let Some(send) = blocked.lock().unwrap().take() {
            let _ = send.send(floor);
        }
    });
    let next_lease = lease.clone();
    let next = tokio::spawn(async move { client.health(&next_lease).await });
    let observed = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, blocked_rx)
        .await
        .unwrap()
        .unwrap();
    let reserved_too_far = pair.relay.requests.lock().unwrap().iter().any(|request| {
        request
            .counter
            .is_some_and(|counter| counter >= oldest + 64)
    });
    pair.relay.release_http.notify_waiters();
    held.await.unwrap().unwrap();
    next.await.unwrap().unwrap();
    let resumed = pair
        .relay
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.counter == Some(oldest + 64));
    pair.finish().await;
    assert_eq!(observed, oldest);
    assert!(!reserved_too_far && resumed);
}

#[tokio::test]
async fn federation_security_unsigned_refusal_fences_incarnation_not_application_outcome() {
    let pair = Pair::established().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    pair.relay.response_fault.store(6, Ordering::SeqCst);
    let response = pair.central_peer.rpc.health(&lease).await;
    let exercised = pair.relay.response_fault.load(Ordering::SeqCst) == 0;
    let unknown = response
        .as_ref()
        .err()
        .is_some_and(|error| error.status == StatusCode::SERVICE_UNAVAILABLE);
    let fenced = lease.with_incarnation(|| Ok(())).is_err();
    pair.finish().await;
    assert!(
        exercised && unknown,
        "unsigned 409 is not an authenticated negative result"
    );
    assert!(
        fenced,
        "a lost/verifiably absent reply cannot hold a sender floor forever"
    );
}

#[tokio::test]
async fn federation_security_receiver_counter_wrap_cannot_reopen_a_namespace() {
    let (satellite, control) = established().await;
    let mut statuses = Vec::new();
    for counter in [u64::MAX, 0, u64::MAX] {
        let (headers, _) = sign_request(
            &control.session,
            counter,
            "GET",
            "/federation/v1/health",
            satellite.authority_headers(&control.session.incarnation),
            &[],
        );
        statuses.push(
            wire(satellite.port, "GET", "/federation/v1/health", headers, &[])
                .await
                .status,
        );
    }
    drop(control);
    satellite.finish().await;
    assert_eq!(statuses[0], StatusCode::OK);
    assert_eq!(statuses[1..], [StatusCode::CONFLICT, StatusCode::CONFLICT]);
}
