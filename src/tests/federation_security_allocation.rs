// Owns real-client sender skew, authenticated settlement and counter-wrap controls.
// Does not implement the sender window or issue new application intents. Split from security requests.

use super::*;

/// An established pair whose central control link outlives the time a test
/// steps through, so only the request path decides the link's fate. Its
/// request deadlines run on the scripted clock: no test waits on host time.
async fn long_control_pair() -> Pair {
    Pair::with_scripted_central_io(Duration::from_secs(3600))
        .await
        .wait_established()
        .await
}

/// Makes the relay hold the request that carries counter `next`;
/// `wait_for_relayed` then waits until the relay has seen it.
async fn hold_next_request(pair: &Pair, next: u64) {
    pair.relay.hold_counter.store(next, Ordering::SeqCst);
}

async fn wait_for_relayed(pair: &Pair, counter: u64) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = pair.relay.changed.notified();
            if pair
                .relay
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.counter == Some(counter))
            {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
}

/// A cancelled caller gives its request an UNKNOWN outcome: its counter is
/// retired at once, so it never pins the allocation floor. Well past 64 later
/// requests are served by the same enrolled pair, with no re-pairing.
#[tokio::test]
async fn federation_security_cancelled_request_frees_its_counter_and_keeps_the_link() {
    let pair = long_control_pair().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    let next = context.sender.lock().unwrap().next;
    hold_next_request(&pair, next).await;
    let client = pair.central_peer.rpc.clone();
    let old = lease.clone();
    let request = tokio::spawn(async move { client.health(&old).await });
    wait_for_relayed(&pair, next).await;
    request.abort();
    // Tokio drops a cancelled task's future before its handle reports the
    // cancellation, so the caller's counter is already settled or not.
    assert!(request.await.unwrap_err().is_cancelled());
    let retired = !context
        .sender
        .lock()
        .unwrap()
        .outstanding
        .contains_key(&next);
    pair.relay.hold_counter.store(0, Ordering::SeqCst);
    let mut served = 0;
    if retired {
        for _ in 0..70 {
            if pair.central_peer.rpc.health(&lease).await.is_ok() {
                served += 1;
            }
        }
    }
    let kept = lease.with_incarnation(|| Ok(())).is_ok();
    let same_context = Arc::ptr_eq(
        &context,
        &pair.central_peer.channel.security_context().unwrap(),
    );
    pair.finish().await;
    assert!(
        retired,
        "the cancelled counter is retired, not left to pin allocation"
    );
    assert_eq!(
        served, 70,
        "the same pair serves well past 64 later requests"
    );
    assert!(kept, "no timeout or cancellation tears down the link");
    assert!(same_context, "no re-pairing or new incarnation");
}

/// The caller of a request unanswered past its unchanged 10 s limit gets an
/// UNKNOWN outcome, and the request leaves exactly one retirement record
/// naming the fixed operation. The link stays up and the same pair serves the
/// next request. (The watchdog's own retirement is shown separately.)
#[tokio::test]
async fn federation_security_unanswered_request_ends_unknown_and_keeps_the_link() {
    let pair = long_control_pair().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    let next = context.sender.lock().unwrap().next;
    hold_next_request(&pair, next).await;
    let client = pair.central_peer.rpc.clone();
    let old = lease.clone();
    let request = tokio::spawn(async move { client.health(&old).await });
    wait_for_relayed(&pair, next).await;
    pair.central_clock
        .advance(FEDERATION_SENDER_RETIREMENT + Duration::from_secs(1));
    let outcome = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await
        .unwrap()
        .unwrap();
    pair.relay.hold_counter.store(0, Ordering::SeqCst);
    let next_served = pair.central_peer.rpc.health(&lease).await;
    let retired = context
        .sender_events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.counter == next && event.kind == "retiredUnknown")
        .cloned()
        .collect::<Vec<_>>();
    let kept = lease.with_incarnation(|| Ok(())).is_ok();
    pair.finish().await;
    assert!(
        outcome.is_err_and(|error| error.status == StatusCode::SERVICE_UNAVAILABLE),
        "an unanswered request ends UNKNOWN"
    );
    assert_eq!(retired.len(), 1, "one retirement record: {retired:?}");
    assert_eq!(retired[0].operation, "health");
    assert!(kept, "the link survives the expired request");
    assert!(
        next_served.is_ok(),
        "the same pair serves the next request: {next_served:?}"
    );
}

/// A verified reply that arrives after its counter was already retired as
/// UNKNOWN keeps that UNKNOWN outcome, so the caller's result and the one
/// retirement record agree. The link stays up.
#[tokio::test]
async fn federation_security_reply_after_retirement_stays_unknown() {
    let pair = long_control_pair().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    let next = context.sender.lock().unwrap().next;
    hold_next_request(&pair, next).await;
    let client = pair.central_peer.rpc.clone();
    let old = lease.clone();
    let request = tokio::spawn(async move { client.health(&old).await });
    wait_for_relayed(&pair, next).await;
    assert!(context.retire_unknown(next, "unsettled", pair.central_clock.now()));
    pair.relay.hold_counter.store(0, Ordering::SeqCst);
    // A stored permit: the held handler may not be waiting yet.
    pair.relay.release_http.notify_one();
    let outcome = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, request)
        .await
        .unwrap()
        .unwrap();
    let retired = context
        .sender_events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.counter == next && event.kind == "retiredUnknown")
        .count();
    let kept = lease.with_incarnation(|| Ok(())).is_ok();
    pair.finish().await;
    assert!(
        outcome.is_err_and(|error| error.status == StatusCode::SERVICE_UNAVAILABLE),
        "the retired request stays UNKNOWN"
    );
    assert_eq!(retired, 1, "exactly the one retirement record");
    assert!(kept, "the link stays up");
}

/// The watchdog alone retires a counter left unsettled past the unchanged
/// sender window, with one record naming its operation and no fence.
#[tokio::test]
async fn federation_security_watchdog_retires_an_abandoned_counter_without_fencing() {
    let pair = long_control_pair().await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    // One request starts the watchdog.
    pair.central_peer.rpc.health(&lease).await.unwrap();
    let counter = pair
        .central_peer
        .rpc
        .allocate_authenticated_counter(&context)
        .await
        .unwrap();
    pair.central_clock
        .advance(FEDERATION_SENDER_RETIREMENT + Duration::from_secs(1));
    let retired = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = context.changed.notified();
            let retired = context
                .sender_events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event.counter == counter && event.kind == "retiredUnknown")
                .cloned()
                .collect::<Vec<_>>();
            if !retired.is_empty() {
                break retired;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    let outstanding = context
        .sender
        .lock()
        .unwrap()
        .outstanding
        .contains_key(&counter);
    let kept = lease.with_incarnation(|| Ok(())).is_ok();
    pair.finish().await;
    assert_eq!(retired.len(), 1, "{retired:?}");
    assert_eq!(retired[0].operation, "test");
    assert!(
        !outstanding,
        "the abandoned counter no longer pins allocation"
    );
    assert!(kept, "the watchdog never fences");
}

/// A transport failure after the request left, a dropped connection or a
/// body that fails after its authentic headers, is an UNKNOWN outcome for that
/// request only: the link stays and the same pair serves the next request.
#[tokio::test]
async fn federation_security_transport_failures_keep_the_link() {
    for fault in [9, 10] {
        let pair = Pair::established().await;
        let lease = pair.central_peer.channel.current_lease().unwrap();
        pair.relay.response_fault.store(fault, Ordering::SeqCst);
        let failed = pair.central_peer.rpc.health(&lease).await;
        let exercised = pair.relay.response_fault.load(Ordering::SeqCst) == 0;
        let kept = lease.with_incarnation(|| Ok(())).is_ok();
        let next = pair.central_peer.rpc.health(&lease).await;
        pair.finish().await;
        assert!(exercised, "fault {fault} reached the reply");
        assert_eq!(
            failed.err().map(|error| error.status),
            Some(StatusCode::SERVICE_UNAVAILABLE),
            "fault {fault}"
        );
        assert!(kept, "fault {fault}: the link survives");
        assert!(
            next.is_ok(),
            "fault {fault}: the next request is served: {next:?}"
        );
    }
}

/// Only a 408 with no MAC header, the receiver's own deadline answer, is an
/// unanswered request: UNKNOWN, and the link stays. A 408 that still carries
/// a malformed MAC header is a security event and fences.
#[tokio::test]
async fn federation_security_only_an_unsigned_408_is_unanswered() {
    for (fault, expect_fenced) in [(7, false), (8, true)] {
        let pair = Pair::established().await;
        let lease = pair.central_peer.channel.current_lease().unwrap();
        pair.relay.response_fault.store(fault, Ordering::SeqCst);
        let response = pair.central_peer.rpc.health(&lease).await;
        let exercised = pair.relay.response_fault.load(Ordering::SeqCst) == 0;
        let fenced = lease.with_incarnation(|| Ok(())).is_err();
        pair.finish().await;
        assert!(exercised, "fault {fault} reached the reply");
        assert_eq!(
            response.err().map(|error| error.status),
            Some(StatusCode::SERVICE_UNAVAILABLE),
            "fault {fault}"
        );
        assert_eq!(fenced, expect_fenced, "fault {fault}");
    }
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
