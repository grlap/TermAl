// Owns control heartbeat freshness, symmetric expiry and owner-fenced teardown.
// Does not use elapsed wall time as proof of success. Split from security attestation.

use super::*;

async fn wait_for_fence(lease: &FederationLinkLease) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while lease.with_incarnation(|| Ok(())).is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expired control must apply its exact-owner fence");
}

#[tokio::test]
async fn federation_security_owned_stream_eof_records_reason_before_withdrawing_runtime_link() {
    let (satellite, control) = established().await;
    let lease = satellite.handle.channel.current_lease().unwrap();
    drop(control);
    let fenced = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while lease.with_incarnation(|| Ok(())).is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let (available, observed) = {
        let inner = satellite.app.inner.lock().unwrap();
        (
            inner.federation.links.len(),
            inner.federation_attestation[&satellite.handle.channel.snapshot.peer.host_id].project(),
        )
    };
    satellite.finish().await;
    assert!(
        fenced && available == 0,
        "EOF cannot leave verified availability behind"
    );
    assert_eq!(observed["lastStage"], "control");
    assert_eq!(observed["lastReason"], "controlEof");
    assert_eq!(observed["consecutiveFailures"], 0);
}

#[tokio::test]
async fn federation_security_satellite_replayed_requests_do_not_keep_context_live() {
    let (mut satellite, mut control) = established().await;
    let lease = satellite.handle.channel.current_lease().unwrap();
    let replay = control.session.heartbeat(1, &[7; 16]);
    for _ in 0..4 {
        if lease.with_incarnation(|| Ok(())).is_err() {
            break;
        }
        write_frame(&mut control.io, &replay).await.unwrap();
        satellite.event("requestRefused", 1).await;
        satellite.clock.advance(Duration::from_secs(12));
    }
    wait_for_fence(&lease).await;
    let fenced_while_open = lease.with_incarnation(|| Ok(())).is_err();
    drop(control);
    satellite.finish().await;
    assert!(
        fenced_while_open,
        "old valid requests cannot refresh the satellite's freshness deadline"
    );
}

#[tokio::test]
async fn federation_security_invalid_control_tag_cannot_refresh_satellite_liveness() {
    let (mut satellite, mut control) = established().await;
    let lease = satellite.handle.channel.current_lease().unwrap();
    let mut invalid = control.session.heartbeat(2, &[8; 16]);
    *invalid.last_mut().unwrap() ^= 1;
    for _ in 0..4 {
        if lease.with_incarnation(|| Ok(())).is_err() {
            break;
        }
        write_frame(&mut control.io, &invalid).await.unwrap();
        satellite.event("requestRefused", 2).await;
        satellite.clock.advance(Duration::from_secs(12));
    }
    wait_for_fence(&lease).await;
    let fenced = lease.with_incarnation(|| Ok(())).is_err();
    drop(control);
    satellite.finish().await;
    assert!(fenced);
}

#[tokio::test]
async fn federation_security_old_expiry_callback_cannot_fence_successor_context() {
    let (satellite, old) = established().await;
    satellite.clock.advance(Duration::from_secs(20));
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let incarnation = successor.session.incarnation.clone();
    satellite.clock.advance(Duration::from_secs(26));
    successor.heartbeat(2, &[10; 16]).await.unwrap();
    let retained = satellite
        .handle
        .channel
        .current_lease()
        .ok()
        .is_some_and(|lease| lease.incarnation == incarnation);
    drop(old);
    drop(successor);
    satellite.finish().await;
    assert!(
        retained,
        "the old owner deadline expires before the successor deadline"
    );
}

#[tokio::test]
async fn federation_security_central_old_unsolicited_replies_do_not_keep_context_live() {
    let mut pair = Pair::with_central_control_timeout(Duration::from_secs(5))
        .await
        .wait_established()
        .await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let response = pair.relay.last_heartbeat.lock().unwrap()[1]
        .clone()
        .expect("real authenticated readiness heartbeat");
    let injection = pair.relay.inject.lock().unwrap()[1].clone().unwrap();
    pair.relay.mute[1].store(true, Ordering::SeqCst);
    pair.central_event("replyAccepted", 1).await;
    let origin = pair.central_clock.now();
    let mut refusals = Vec::new();
    let mut terminal = None;
    // A1 section 3: old or unsolicited replies do not refresh liveness;
    // the bounded response deadline takes this exact context down. Every
    // required refusal observation therefore belongs BEFORE expiry.
    for iteration in 0..5 {
        eprintln!(
            "unsolicited replay iteration={iteration}, elapsed={:?}, lease_current={}",
            pair.central_clock.now().duration_since(origin),
            lease.with_incarnation(|| Ok(())).is_ok()
        );
        let _ = injection.send(response.clone());
        let event = pair.central_next_event().await;
        if (event.0, event.1) != ("replyRefused", 1) {
            terminal = Some(event);
            break;
        }
        refusals.push(event.2.duration_since(origin));
        pair.central_clock.advance(Duration::from_secs(1));
    }
    // This final probe discriminates specified expiry from a replay-refresh
    // defect. It requires no further refusal on the expired incarnation.
    let terminal = match terminal {
        Some(event) => event,
        None => {
            eprintln!(
                "unsolicited replay expiry-probe iteration=5, elapsed={:?}, lease_current={}",
                pair.central_clock.now().duration_since(origin),
                lease.with_incarnation(|| Ok(())).is_ok()
            );
            let _ = injection.send(response);
            pair.central_next_event().await
        }
    };
    if terminal.0 == "controlExpired" {
        wait_for_fence(&lease).await;
    }
    let fenced = lease.with_incarnation(|| Ok(())).is_err();
    // Satellite's production 45-second deadline cannot have expired here;
    // only the central's configured 5-second response bound is under test.
    pair.finish().await;
    assert_eq!(
        refusals,
        (0..5).map(Duration::from_secs).collect::<Vec<_>>(),
        "all five refusals must be observed strictly before expiry; terminal={terminal:?}"
    );
    assert_eq!(
        terminal.0,
        "controlExpired",
        "old unsolicited replies kept the context live or control terminated unexpectedly; terminal={terminal:?}, elapsed={:?}",
        terminal.2.duration_since(origin)
    );
    assert_eq!(terminal.2.duration_since(origin), Duration::from_secs(5));
    assert!(
        fenced,
        "duplicate/old/unsolicited replies never refresh central liveness"
    );
}

#[tokio::test]
async fn federation_security_central_future_and_invalid_replies_never_refresh_deadline() {
    let mut pair = Pair::with_central_control_timeout(Duration::from_secs(5))
        .await
        .wait_established()
        .await;
    let lease = pair.central_peer.channel.current_lease().unwrap();
    let context = pair.central_peer.channel.security_context().unwrap();
    let request = pair.relay.last_heartbeat.lock().unwrap()[0]
        .clone()
        .unwrap();
    let fields = decode(&request[4..], b"termal-hb-req-v1", 3).unwrap();
    let future = 100_u64.to_be_bytes();
    let valid_future_tag = mac(
        &context.keys[5],
        &joined(&[b"hb-resp", &context.transcript, &future, &fields[1]]),
    );
    let valid_future = encode(b"termal-hb-resp-v1", &[future.to_vec(), valid_future_tag]);
    let mut invalid = pair.relay.last_heartbeat.lock().unwrap()[1]
        .clone()
        .unwrap();
    *invalid.last_mut().unwrap() ^= 1;
    let injection = pair.relay.inject.lock().unwrap()[1].clone().unwrap();
    pair.relay.mute[1].store(true, Ordering::SeqCst);
    for _ in 0..5 {
        if lease.with_incarnation(|| Ok(())).is_err() {
            break;
        }
        let mut frame = (valid_future.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&valid_future);
        injection.send(frame).unwrap();
        pair.central_event("replyRefused", 100).await;
        injection.send(invalid.clone()).unwrap();
        pair.central_event("replyRefused", 1).await;
        pair.central_clock.advance(Duration::from_secs(1));
    }
    wait_for_fence(&lease).await;
    pair.finish().await;
}

#[tokio::test]
async fn federation_security_fresh_heartbeats_extend_the_current_deadline() {
    let (satellite, mut control) = established().await;
    for sequence in [2, 3] {
        satellite.clock.advance(Duration::from_secs(20));
        control
            .heartbeat(sequence, &[sequence as u8; 16])
            .await
            .unwrap();
    }
    satellite.clock.advance(Duration::from_secs(10));
    control.heartbeat(4, &[4; 16]).await.unwrap();
    let alive = satellite.handle.channel.current_lease().is_ok();
    drop(control);
    satellite.finish().await;
    assert!(
        alive,
        "fresh proofs keep the context alive beyond its first deadline"
    );
}

#[tokio::test]
async fn federation_security_delayed_handshake_completion_cannot_replace_new_owner() {
    let satellite = Satellite::new().await;
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let old = Control::start_paused(&satellite, 'a', |_| {}, Some((entered, release.clone())));
    let next = async {
        entered_rx.await.unwrap();
        let successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
        release.notify_one();
        successor
    };
    let (old, successor) = tokio::join!(old, next);
    let actual = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    let expected = successor.session.incarnation.clone();
    drop(successor);
    satellite.finish().await;
    assert!(
        old.is_err(),
        "completion with the old owner generation must be refused"
    );
    assert_eq!(actual, expected);
}
