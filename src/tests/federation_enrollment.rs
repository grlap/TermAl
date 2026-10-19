// Owns L1 integration with M1's real current-enrollment/apply boundary.
// Does not duplicate registry fixtures or provisioning. New integration tests.

use super::*;

fn linked_enrollment() -> (AppState, FederationChannel, FederationLinkLease) {
    let app = federation_registry::central();
    let record = federation_registry::peer(48001);
    let id = record.identity.host_id.clone();
    app.enroll_federation_peer(record).unwrap();
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let incarnation = Uuid::new_v4().to_string();
    let post_commit = app
        .federation_peer_connected(&snapshot, incarnation.clone())
        .unwrap();
    let channel = FederationChannel::for_app(
        post_commit,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let lease = channel.activate(incarnation);
    (app, channel, lease)
}

#[test]
fn federation_reply_apply_rechecks_real_enrollment_under_state_lock() {
    let (app, channel, lease) = linked_enrollment();
    let applied = AtomicBool::new(false);
    lease
        .with_current(&app, |_, snapshot| {
            assert_eq!(snapshot, &channel.snapshot);
            applied.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    assert!(applied.swap(false, Ordering::SeqCst));
    app.transition_federation_peer(&channel.snapshot, FederationMembershipState::Revoked)
        .unwrap();
    let rejected = lease.with_current(&app, |_, _| {
        applied.store(true, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(rejected.unwrap_err().status, StatusCode::CONFLICT);
    assert!(!applied.load(Ordering::SeqCst));
}

#[test]
fn federation_reply_apply_rejects_real_reenrollment_with_same_endpoint() {
    let (app, channel, lease) = linked_enrollment();
    let record = app
        .inner
        .lock()
        .unwrap()
        .federation
        .peers
        .get(&channel.snapshot.peer.host_id)
        .unwrap()
        .clone();
    app.enroll_federation_peer(record).unwrap();
    let new = app
        .federation_peer_snapshot(&channel.snapshot.peer.host_id)
        .unwrap();
    assert_eq!(new.endpoint, channel.snapshot.endpoint);
    assert!(new.enrollment_generation > channel.snapshot.enrollment_generation);
    assert_eq!(
        lease.with_current(&app, |_, _| Ok(())).unwrap_err().status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn federation_real_state_lock_is_free_while_rpc_waits_for_wire_reply() {
    let (app, channel, _) = linked_enrollment();
    let (satellite, _proof) = super::federation_control_fixture::establish(&channel).await;
    let lease = channel.current_lease().unwrap();
    let health = FederationHealth {
        protocol_version: 1,
        host_id: channel.snapshot.peer.host_id.clone(),
        host_generation: channel.snapshot.peer.host_generation,
        enrollment_generation: channel.snapshot.enrollment_generation,
        link_incarnation: lease.incarnation.clone(),
    };
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_release = release.clone();
    let router = Router::new().route(
        "/federation/v1/health",
        get(move || {
            let entered = entered.clone();
            let release = handler_release.clone();
            let health = health.clone();
            async move {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                release.notified().await;
                Json(health)
            }
        }),
    );
    let router = super::federation_control_fixture::authenticated_router(satellite, router);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
            .unwrap();
    });
    let check_channel = channel.clone();
    let rpc = FederationRpcClient::new(
        port,
        channel,
        Arc::new(move || check_channel.current_authority().map(|_| ())),
    )
    .unwrap();
    let request = tokio::spawn(async move { rpc.health(&lease).await });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    {
        let _inner = app
            .inner
            .try_lock_now()
            .expect("global state must remain available during network I/O");
    }
    release.notify_one();
    request.await.unwrap().unwrap();
    stop.send_replace(true);
    server.await.unwrap();
}

#[test]
fn federation_membership_bookkeeping_keeps_link_and_refreshes_apply_basis() {
    let (app, channel, lease) = linked_enrollment();
    let basis = channel.transport_basis();
    app.verify_federation_peer(&basis, federation_registry::checks())
        .unwrap();
    let verified = app.federation_peer_snapshot(&basis.peer.host_id).unwrap();
    app.transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let active = app.federation_peer_snapshot(&basis.peer.host_id).unwrap();
    assert!(active.record_generation > basis.record_generation);
    assert!(basis.same_transport_as(&active));
    assert_eq!(channel.current_authority().unwrap(), active);
    lease
        .with_current(&app, |_, current| {
            assert_eq!(current.membership, FederationMembershipState::Active);
            Ok(())
        })
        .unwrap();
    app.transition_federation_peer(&active, FederationMembershipState::Draining)
        .unwrap();
    lease
        .with_current(&app, |_, current| {
            assert_eq!(current.membership, FederationMembershipState::Draining);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        channel.current_lease().unwrap().incarnation,
        lease.incarnation
    );
}

#[test]
fn federation_reenrollment_fences_even_reused_endpoint_and_incarnation_text() {
    let (app, channel, lease) = linked_enrollment();
    let record = app
        .inner
        .lock()
        .unwrap()
        .federation
        .peers
        .get(&channel.snapshot.peer.host_id)
        .unwrap()
        .clone();
    app.enroll_federation_peer(record).unwrap();
    let next = app
        .federation_peer_snapshot(&channel.snapshot.peer.host_id)
        .unwrap();
    app.federation_peer_connected(&next, lease.incarnation.clone())
        .unwrap();
    assert_eq!(
        channel.current_authority().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        lease.with_current(&app, |_, _| Ok(())).unwrap_err().status,
        StatusCode::CONFLICT
    );
}

#[test]
fn federation_durability_hold_refuses_authority_without_discarding_link() {
    let (app, channel, lease) = linked_enrollment();
    app.inner.lock().unwrap().federation_durable = false;
    assert_eq!(
        channel.current_authority().unwrap_err().status,
        StatusCode::LOCKED
    );
    assert_eq!(
        channel.current_lease().unwrap().incarnation,
        lease.incarnation
    );
    app.inner.lock().unwrap().federation_durable = true;
    channel.current_authority().unwrap();
    lease.with_current(&app, |_, _| Ok(())).unwrap();
}

#[test]
fn federation_local_disconnect_fences_current_placement_admission() {
    let (app, channel, _) = linked_enrollment();
    let basis = channel.current_authority().unwrap();
    app.verify_federation_peer(&basis, federation_registry::checks())
        .unwrap();
    let verified = app.federation_peer_snapshot(&basis.peer.host_id).unwrap();
    app.transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    channel.invalidate();
    let current = app.federation_peer_snapshot(&basis.peer.host_id).unwrap();
    let reservation = app.reserve_federation_placement(
        &current,
        "disconnected-admission".to_owned(),
        "placement".to_owned(),
        1,
        0,
    );
    assert!(
        reservation.is_err(),
        "disconnected ACTIVE peer must refuse fresh capacity"
    );
}

#[tokio::test]
async fn federation_durable_hold_is_retryable_on_wire_without_losing_incarnation() {
    let (app, channel, _) = linked_enrollment();
    let (caller, _proof) = super::federation_control_fixture::establish(&channel).await;
    let lease = channel.current_lease().unwrap();
    let checked = channel.clone();
    let router = federation_router(
        channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let request = |counter| {
        super::federation_control_fixture::signed(
            &caller,
            counter,
            "GET",
            "/federation/v1/health",
            Default::default(),
            &[],
        )
    };
    app.inner.lock().unwrap().federation_durable = false;
    let held = router.clone().oneshot(request(1)).await.unwrap().status();
    let retained = channel.current_lease().unwrap().incarnation;
    app.inner.lock().unwrap().federation_durable = true;
    let settled = router.oneshot(request(2)).await.unwrap().status();
    assert_eq!(
        held,
        StatusCode::LOCKED,
        "a persistence hold must be distinguishable from a stale link"
    );
    assert_eq!(retained, lease.incarnation);
    assert_eq!(settled, StatusCode::OK);
}
