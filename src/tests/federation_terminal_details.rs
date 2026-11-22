// Pins the read-only terminal snapshot to accepted control frames, not raw
// parsed counters, context age or the time at which a later GET is made.
use super::*;

fn terminal(channel: &FederationChannel) -> Value {
    let view = channel
        .app
        .as_ref()
        .unwrap()
        .federation_registry_view()
        .unwrap();
    view["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| {
            peer["host"]["hostId"].as_str() == Some(channel.snapshot.peer.host_id.as_str())
        })
        .expect("enrolled peer in existing registry projection")["attestation"]["lastTerminal"]
        .clone()
}

async fn fenced(lease: &FederationLinkLease) {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while lease.with_incarnation(|| Ok(())).is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned control teardown applied its fence");
}

#[tokio::test]
async fn federation_terminal_satellite_uses_last_accepted_frame_and_freezes_first_event() {
    let (mut satellite, mut control) = established().await;
    satellite.event("requestAccepted", 1).await;
    let channel = satellite.handle.channel.clone();
    let context = channel.security_context().unwrap();
    let lease = channel.current_lease().unwrap();
    satellite.clock.advance(Duration::from_secs(20));
    control.heartbeat(2, &[8; 16]).await.unwrap();
    satellite.event("requestAccepted", 2).await;
    satellite.clock.advance(Duration::from_secs(3));
    let mut invalid = control.session.heartbeat(99, &[9; 16]);
    *invalid.last_mut().unwrap() ^= 1;
    write_frame(&mut control.io, &invalid).await.unwrap();
    satellite.event("requestRefused", 99).await;
    satellite.clock.advance(Duration::from_secs(4));
    control.io.shutdown().await.unwrap();
    fenced(&lease).await;
    let first = terminal(&channel);
    channel.observe_terminal(&context, "monitor", "sshChildExited");
    satellite.clock.advance(Duration::from_secs(10));
    let later = terminal(&channel);
    drop(control);
    satellite.finish().await;
    assert_eq!(first["role"], "satellite");
    assert_eq!(first["stage"], "control");
    assert_eq!(first["reason"], "controlEof");
    assert_eq!(first["incarnation"], lease.incarnation);
    assert_eq!(first["lastAcceptedSequence"], 2);
    assert_eq!(first["lastAcceptedAgeMsAtTerminal"], 7_000);
    assert_eq!(first["currentBasis"], true);
    assert_eq!(
        later, first,
        "cleanup and later reads cannot rewrite the first event or its age"
    );
}

#[tokio::test]
async fn federation_terminal_central_replayed_reply_cannot_refresh_accepted_age() {
    let mut pair = Pair::with_central_control_timeout(Duration::from_secs(5))
        .await
        .wait_established()
        .await;
    pair.central_event("replyAccepted", 1).await;
    let channel = pair.central_peer.channel.clone();
    let lease = channel.current_lease().unwrap();
    let replay = pair.relay.last_heartbeat.lock().unwrap()[1]
        .clone()
        .unwrap();
    let injection = pair.relay.inject.lock().unwrap()[1].clone().unwrap();
    pair.relay.mute[1].store(true, Ordering::SeqCst);
    pair.central_clock.advance(Duration::from_secs(2));
    injection.send(replay).unwrap();
    pair.central_event("replyRefused", 1).await;
    pair.central_clock.advance(Duration::from_secs(3));
    fenced(&lease).await;
    let event = terminal(&channel);
    pair.finish().await;
    assert_eq!(event["role"], "central");
    assert_eq!(event["stage"], "control");
    assert_eq!(event["reason"], "controlExpired");
    assert_eq!(event["lastAcceptedSequence"], 1);
    assert_eq!(event["lastAcceptedAgeMsAtTerminal"], 5_000);
}

#[tokio::test]
async fn federation_terminal_without_accepted_heartbeat_reports_null_measurements() {
    let satellite = Satellite::new().await;
    let mut control = Control::open_unready(&satellite.info(), 'a', |_| {}, None)
        .await
        .unwrap();
    let channel = satellite.handle.channel.clone();
    let lease = channel.current_lease().unwrap();
    satellite.clock.advance(Duration::from_secs(9));
    control.io.shutdown().await.unwrap();
    fenced(&lease).await;
    let event = terminal(&channel);
    drop(control);
    satellite.finish().await;
    assert!(
        event.is_object(),
        "first terminal event is retained even before readiness"
    );
    assert_eq!(event["lastAcceptedSequence"], Value::Null);
    assert_eq!(event["lastAcceptedAgeMsAtTerminal"], Value::Null);
}

#[tokio::test]
async fn federation_terminal_survives_new_attempt_as_its_own_historical_basis() {
    let (satellite, mut control) = established().await;
    let channel = satellite.handle.channel.clone();
    let lease = channel.current_lease().unwrap();
    control.io.shutdown().await.unwrap();
    fenced(&lease).await;
    let old_event = terminal(&channel);
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let historical = terminal(&channel);
    let new_lease = channel.current_lease().unwrap();
    successor.io.shutdown().await.unwrap();
    fenced(&new_lease).await;
    let new_event = terminal(&channel);
    drop(control);
    drop(successor);
    satellite.finish().await;
    assert!(old_event.is_object());
    assert_eq!(historical["incarnation"], lease.incarnation);
    assert_eq!(
        historical["currentBasis"], false,
        "a new current attempt cannot relabel its predecessor event"
    );
    assert_eq!(
        historical["lastAcceptedAgeMsAtTerminal"],
        old_event["lastAcceptedAgeMsAtTerminal"]
    );
    assert_eq!(new_event["incarnation"], new_lease.incarnation);
    assert_eq!(new_event["currentBasis"], true);
}

#[tokio::test]
async fn federation_terminal_delayed_old_context_cannot_replace_successor_details() {
    let (mut satellite, old) = established().await;
    let channel = satellite.handle.channel.clone();
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let successor_lease = channel.current_lease().unwrap();
    drop(old);
    satellite.event("controlContextChanged", 1).await;
    let after_old = terminal(&channel);
    successor.io.shutdown().await.unwrap();
    fenced(&successor_lease).await;
    let current = terminal(&channel);
    satellite.finish().await;
    assert_eq!(
        after_old,
        Value::Null,
        "a stale context's first terminal event cannot fill its successor observation"
    );
    assert_eq!(current["incarnation"], successor_lease.incarnation);
    assert_eq!(current["currentBasis"], true);
}
