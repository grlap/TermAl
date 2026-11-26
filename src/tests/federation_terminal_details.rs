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
async fn federation_terminal_satellite_ages_from_completed_response_without_moving_deadline() {
    use super::super::federation_control_fixture::heartbeat_request;
    use tokio::io::AsyncReadExt;

    let mut satellite = Satellite::new().await;
    let channel = satellite.handle.channel.clone();
    let mut opposite = channel.snapshot.clone();
    std::mem::swap(&mut opposite.local, &mut opposite.peer);
    opposite.local_role = FederationRole::Central;
    let central = FederationChannel::new(
        opposite,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );
    // A response cannot fit in this pipe, so its write stalls until the peer
    // drains the body. The proof setup below keeps both sides running.
    let capacity = 8;
    let (central_io, satellite_io) = tokio::io::duplex(capacity);
    let mut central_io: FederationControlIo = Box::new(central_io);
    let mut satellite_io: FederationControlIo = Box::new(satellite_io);
    let central_proof = async {
        let hello = federation_read_frame(&mut central_io).await.unwrap();
        federation_prove_central(&central, &mut central_io, 1, hello).await
    };
    let (central_context, satellite_context) =
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            tokio::join!(
                central_proof,
                federation_attest_satellite(&channel, &mut satellite_io)
            )
        })
        .await
        .unwrap();
    let (context, guard) = satellite_context.unwrap();
    let central_context = central_context.unwrap();
    let delay = Duration::from_secs(2);
    let expected_age = channel.control.timeout - delay;
    let peer = async {
        federation_write_frame(&mut central_io, &heartbeat_request(&central_context, 1))
            .await
            .unwrap();
        let response_len = central_io.read_u32().await.unwrap() as usize;
        assert!((capacity + 1..=FEDERATION_FRAME_LIMIT).contains(&response_len));
        satellite.clock.advance(delay);
        let mut response = vec![0; response_len];
        central_io.read_exact(&mut response).await.unwrap();
        satellite.event("requestAccepted", 1).await;
        // Expire at the original deadline, measured from request receipt.
        satellite.clock.advance(expected_age);
    };
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::join!(
            peer,
            federation_control_satellite_established(&channel, &mut satellite_io, &context)
        );
    })
    .await
    .expect("the original heartbeat deadline must still expire");
    drop(guard);
    let event = terminal(&channel);
    satellite.finish().await;

    assert_eq!(event["reason"], "controlExpired");
    assert_eq!(event["lastAcceptedSequence"], 1);
    assert_eq!(
        event["lastAcceptedAgeMsAtTerminal"],
        expected_age.as_millis() as u64
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

#[tokio::test]
async fn federation_terminal_withdrawal_survives_reconnect_as_history() {
    let (satellite, old) = established().await;
    let channel = satellite.handle.channel.clone();
    let context = channel.security_context().unwrap();
    channel.fence_rpc_security_failure(&context, "rpcError", "/test", "reply", None);
    assert!(channel.current_lease().is_err());
    drop(old);
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let successor_lease = channel.current_lease().unwrap();
    let historical = terminal(&channel);
    successor.io.shutdown().await.unwrap();
    fenced(&successor_lease).await;
    let current = terminal(&channel);
    drop(successor);
    satellite.finish().await;

    assert_eq!(historical["incarnation"], context.incarnation);
    assert_eq!(historical["reason"], "rpcError");
    assert_eq!(historical["currentBasis"], false);
    assert_eq!(current["incarnation"], successor_lease.incarnation);
    assert_eq!(current["currentBasis"], true);
}

#[tokio::test]
async fn federation_terminal_claim_and_withdrawal_cannot_split_between_callers() {
    let (satellite, control) = established().await;
    let channel = satellite.handle.channel.clone();
    let context = channel.security_context().unwrap();
    let (claimed, claim) = tokio::sync::oneshot::channel();
    let (resume, withdrawal) = tokio::sync::oneshot::channel();
    let first = tokio::task::spawn_blocking({
        let channel = channel.clone();
        let context = context.clone();
        move || {
            channel.fence_terminal_event(&context, "rpc", "firstFailure", None, None, |state| {
                claimed.send(()).unwrap();
                let _ = withdrawal.blocking_recv();
                channel.fence_security_locked(state, context.owner, &context.incarnation);
            });
        }
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, claim)
        .await
        .unwrap()
        .unwrap();
    let (started, contender) = tokio::sync::oneshot::channel();
    let second = tokio::task::spawn_blocking({
        let channel = channel.clone();
        let context = context.clone();
        move || {
            // The first callback is still paused when the second caller tries
            // to claim and withdraw the same installed context.
            started.send(channel.state.try_lock().is_err()).unwrap();
            channel.fence_rpc_security_failure(&context, "secondFailure", "/test", "reply", None);
        }
    });
    let contested = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, contender)
        .await
        .unwrap()
        .unwrap();
    resume.send(()).unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        first.await.unwrap();
        second.await.unwrap();
    })
    .await
    .unwrap();
    let event = terminal(&channel);
    let live = channel.current_lease().is_ok();
    let recorded_link = satellite
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&channel.snapshot.peer.host_id);
    drop(control);
    satellite.finish().await;

    assert!(
        contested,
        "the first claim must hold the installation owner through withdrawal"
    );
    assert!(!live && !recorded_link);
    assert_eq!(event["incarnation"], context.incarnation);
    assert_eq!(event["reason"], "firstFailure");
}

async fn replace_terminal_fixture_context(
    channel: &FederationChannel,
    previous: &FederationSecurityContext,
) -> (Arc<FederationSecurityContext>, FederationControlGuard) {
    // Use the real local installation boundary with an already authenticated
    // fixture context. The test only exercises owner replacement and projection.
    let owner = channel.next_security_owner().unwrap();
    let context = Arc::new(
        FederationSecurityContext::derive(
            owner,
            previous.incarnation.clone(),
            previous.transcript.clone(),
            &[1; 32],
            &[2; 32],
            &[3; 32],
        )
        .unwrap(),
    );
    let guard = channel.install_security(context.clone()).await.unwrap();
    (context, guard)
}

#[tokio::test]
async fn federation_terminal_same_transport_orders_by_installed_owner() {
    let (satellite, old_control) = established().await;
    let channel = satellite.handle.channel.clone();
    let old = channel.security_context().unwrap();
    let generation = channel.transport_basis().transport_generation;
    let (second, second_guard) = replace_terminal_fixture_context(&channel, &old).await;
    assert_eq!(channel.transport_basis().transport_generation, generation);
    channel.observe_terminal(&old, "control", "controlContextChanged");
    assert_eq!(terminal(&channel), Value::Null);

    channel.observe_terminal(&second, "control", "secondOwnerEnded");
    assert_eq!(terminal(&channel)["ownerGeneration"], second.owner);
    let (third, third_guard) = replace_terminal_fixture_context(&channel, &second).await;
    assert_eq!(channel.transport_basis().transport_generation, generation);
    let historical = terminal(&channel);
    channel.fence_rpc_security_failure(&third, "rpcError", "/test", "reply", None);
    let current = terminal(&channel);
    drop((second_guard, third_guard, old_control));
    satellite.finish().await;

    assert_eq!(historical["ownerGeneration"], second.owner);
    assert_eq!(historical["currentBasis"], false);
    assert_eq!(current["ownerGeneration"], third.owner);
    assert_eq!(current["transportGeneration"], generation);
    assert_eq!(current["currentBasis"], true);
}

#[tokio::test]
async fn federation_terminal_after_invalidation_keeps_the_installed_basis() {
    let (satellite, control) = established().await;
    let channel = satellite.handle.channel.clone();
    let context = channel.security_context().unwrap();
    channel.invalidate();
    // Both synchronous calls run before the control task can observe retirement.
    channel.fence_rpc_security_failure(&context, "rpcError", "/test", "reply", None);
    let event = terminal(&channel);
    drop(control);
    satellite.finish().await;

    assert_eq!(event["ownerGeneration"], context.owner);
    assert_eq!(event["reason"], "rpcError");
    assert_eq!(event["currentBasis"], true);
}

#[tokio::test]
async fn federation_terminal_failed_replacement_keeps_installed_context_observable() {
    let (mut satellite, mut control) = established().await;
    let channel = satellite.handle.channel.clone();
    let lease = channel.current_lease().unwrap();
    assert!(Control::start(&satellite, 'b', |_| {}).await.is_err());
    control.heartbeat(2, &[8; 16]).await.unwrap();
    satellite.event("requestAccepted", 2).await;
    satellite.clock.advance(Duration::from_secs(3));
    control.io.shutdown().await.unwrap();
    fenced(&lease).await;
    let event = terminal(&channel);
    drop(control);
    satellite.finish().await;

    assert_eq!(event["incarnation"], lease.incarnation);
    assert_eq!(event["reason"], "controlEof");
    assert_eq!(event["lastAcceptedSequence"], 2);
    assert_eq!(event["lastAcceptedAgeMsAtTerminal"], 3_000);
    assert_eq!(event["currentBasis"], true);
}

#[tokio::test]
async fn federation_terminal_proof_write_failure_is_retained_after_install() {
    let satellite = Satellite::new().await;
    let channel = satellite.handle.channel.clone();
    let installed_from = channel.transport_basis().transport_generation;
    let mut opposite = channel.snapshot.clone();
    std::mem::swap(&mut opposite.local, &mut opposite.peer);
    opposite.local_role = FederationRole::Central;
    let central = FederationChannel::new(
        opposite,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );
    let (mut central_io, satellite_io) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut satellite_io: FederationControlIo = Box::new(satellite_io);
    let send_challenge = async move {
        let hello = federation_read_frame(&mut central_io).await.unwrap();
        // Deliver the real challenge and close the stream without reading the proof.
        let mut challenge_only: FederationControlIo =
            Box::new(tokio::io::join(tokio::io::empty(), central_io));
        assert!(
            federation_prove_central(&central, &mut challenge_only, 1, hello)
                .await
                .is_err()
        );
    };
    let (satellite_result, ()) = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::join!(
            federation_attest_satellite(&channel, &mut satellite_io),
            send_challenge
        )
    })
    .await
    .unwrap();
    let error = satellite_result.err().expect("the proof write must fail");
    let installed_transport = channel.transport_basis().transport_generation;
    let live = channel.current_lease().is_ok();
    let event = terminal(&channel);
    satellite.finish().await;

    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(installed_transport > installed_from);
    assert!(
        !live,
        "the failed proof must withdraw the installed context"
    );
    assert_eq!(event["role"], "satellite");
    assert_eq!(event["stage"], "attest");
    assert_eq!(event["reason"], "linkDownBeforeTag");
    assert_eq!(event["ioKind"], "brokenPipe");
    assert_eq!(event["transportGeneration"], installed_transport);
    assert_eq!(event["lastAcceptedSequence"], Value::Null);
    assert_eq!(event["lastAcceptedAgeMsAtTerminal"], Value::Null);
}

#[tokio::test]
async fn federation_terminal_cancelled_proof_write_withdraws_and_records_installed_context() {
    use tokio::io::AsyncReadExt;

    let satellite = Satellite::new().await;
    let channel = satellite.handle.channel.clone();
    let mut opposite = channel.snapshot.clone();
    std::mem::swap(&mut opposite.local, &mut opposite.peer);
    opposite.local_role = FederationRole::Central;
    let central = FederationChannel::new(
        opposite,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );
    let capacity = 8;
    let (mut central_io, satellite_io) = tokio::io::duplex(capacity);
    let task = tokio::spawn({
        let channel = channel.clone();
        async move {
            let mut io: FederationControlIo = Box::new(satellite_io);
            federation_attest_satellite(&channel, &mut io).await
        }
    });
    let (_reader, _writer) = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        let hello = federation_read_frame(&mut central_io).await.unwrap();
        let (mut reader, writer) = tokio::io::split(central_io);
        let mut challenge_only: FederationControlIo =
            Box::new(tokio::io::join(tokio::io::empty(), writer));
        assert!(
            federation_prove_central(&central, &mut challenge_only, 1, hello)
                .await
                .is_err()
        );
        // Consume only the proof prefix and keep the peer open. Its body
        // exceeds pipe capacity, so the installed proof write stays pending.
        let proof_len = reader.read_u32().await.unwrap() as usize;
        assert!((capacity + 1..=FEDERATION_FRAME_LIMIT).contains(&proof_len));
        (reader, challenge_only)
    })
    .await
    .expect("the installed context must begin its proof write");
    let lease = channel.current_lease().unwrap();
    assert_eq!(terminal(&channel), Value::Null);

    task.abort();
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
        .await
        .expect("cancellation must finish");
    assert!(
        result
            .err()
            .expect("pending proof was cancelled")
            .is_cancelled()
    );
    let withdrawn = channel.current_lease().is_err();
    let recorded_link = satellite
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&channel.snapshot.peer.host_id);
    let event = terminal(&channel);
    satellite.finish().await;

    assert!(withdrawn && !recorded_link);
    assert_eq!(event["incarnation"], lease.incarnation);
    assert_eq!(event["stage"], "attest");
    assert_eq!(event["reason"], "tagWriteCancelled");
    assert_eq!(event["ioKind"], Value::Null);
    assert_eq!(event["lastAcceptedSequence"], Value::Null);
    assert_eq!(event["lastAcceptedAgeMsAtTerminal"], Value::Null);
}
