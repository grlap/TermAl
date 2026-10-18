// Owns one-machine federation channel/reconnect tests; no SSH or live stores.
// New fixtures for the federation modules, not split from executor-proxy tests.

use super::*;

pub(super) fn peer_snapshot(role: FederationRole) -> FederationPeerSnapshot {
    let central = FederationHostIdentity {
        host_id: "11111111-1111-4111-8111-111111111111".to_owned(),
        host_generation: 7,
    };
    let satellite = FederationHostIdentity {
        host_id: "22222222-2222-4222-8222-222222222222".to_owned(),
        host_generation: 3,
    };
    let (local, peer) = if role == FederationRole::Central {
        (central.clone(), satellite)
    } else {
        (satellite, central.clone())
    };
    FederationPeerSnapshot {
        local,
        local_role: role,
        peer,
        central,
        enrollment_generation: 5,
        record_generation: 9,
        transport_generation: 9,
        pair_capability_ref: "pair-secret-reference".to_owned(),
        endpoint: FederationSshEndpoint {
            ssh_host: "satellite.example".to_owned(),
            ssh_user: "operator".to_owned(),
            ssh_port: 22,
            known_hosts_ref: "pair-known-hosts".to_owned(),
            host_key_ref: "pair-host-key".to_owned(),
            satellite_federation_port: 47102,
            central_federation_port: 47104,
            satellite_reverse_forward_port: 47103,
        },
        membership: FederationMembershipState::Active,
    }
}

pub(super) fn channel(role: FederationRole, secret: char) -> FederationChannel {
    FederationChannel::new(
        peer_snapshot(role),
        FederationPairCapability::new(secret.to_string().repeat(64)).unwrap(),
    )
}

pub(super) fn allowed() -> FederationAuthorityCheck {
    Arc::new(|| Ok(()))
}

fn caller_authority_for(role: FederationRole) -> String {
    let mut snapshot = peer_snapshot(role);
    std::mem::swap(&mut snapshot.local, &mut snapshot.peer);
    serde_json::to_string(&FederationRequestAuthority::outgoing(&snapshot)).unwrap()
}

#[tokio::test]
async fn federation_listener_rejects_missing_wrong_pair_and_duplicate_capabilities() {
    let router = federation_router(channel(FederationRole::Satellite, 'a'), allowed());
    for supplied in [
        None,
        Some("b".repeat(64)),
        Some("pair-secret-reference".to_owned()),
    ] {
        let mut request = Request::builder().uri("/federation/v1/hello");
        if let Some(value) = supplied {
            request = request.header(FEDERATION_CAPABILITY_HEADER, value);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("pair-secret-reference"));
    }
    let request = Request::builder()
        .uri("/federation/v1/hello")
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .header(FEDERATION_CAPABILITY_HEADER, "b".repeat(64))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn federation_listener_never_mounts_general_api_or_payload_selected_identity() {
    let peer = channel(FederationRole::Satellite, 'a');
    let incarnation = Uuid::new_v4().to_string();
    peer.activate(incarnation.clone());
    let router = federation_router(peer, allowed());
    for path in [
        "/api/health",
        "/api/state",
        "/api/sessions",
        "/",
        "/api/mailboxes",
    ] {
        let request = Request::builder()
            .uri(path)
            .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let request = Request::builder()
        .uri("/federation/v1/health?hostId=attacker")
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .header(
            FEDERATION_AUTHORITY_HEADER,
            caller_authority_for(FederationRole::Satellite),
        )
        .header(FEDERATION_INCARNATION_HEADER, incarnation)
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let health: FederationHealth =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(
        health.host_id,
        peer_snapshot(FederationRole::Satellite).local.host_id
    );
}

#[test]
fn federation_reconnect_and_aba_fence_old_reply_at_apply() {
    let channel = channel(FederationRole::Central, 'a');
    let old = channel.activate(Uuid::new_v4().to_string());
    let mut applied = false;
    channel.invalidate();
    let current = channel.activate(Uuid::new_v4().to_string());
    assert!(
        old.with_incarnation(|| {
            applied = true;
            Ok(())
        })
        .is_err()
    );
    assert!(!applied);
    current
        .with_incarnation(|| {
            applied = true;
            Ok(())
        })
        .unwrap();
    assert!(applied);
}

#[test]
fn federation_delayed_failed_probe_cannot_invalidate_successor_link() {
    let peer = channel(FederationRole::Satellite, 'a');
    let old = peer.activate(Uuid::new_v4().to_string());
    let current = peer.activate(Uuid::new_v4().to_string());
    peer.invalidate_if_current(&old);
    assert_eq!(
        peer.current_lease().unwrap().incarnation,
        current.incarnation
    );
    peer.invalidate_if_current(&current);
    assert!(peer.current_lease().is_err());
}

#[test]
fn federation_handshake_is_receiver_boot_bound_and_one_use() {
    let satellite = channel(FederationRole::Satellite, 'a');
    let hello = satellite.challenge();
    let mut open = FederationOpenLink {
        protocol_version: 1,
        receiver_instance_id: hello.server_instance_id,
        challenge: hello.challenge,
        incarnation: Uuid::new_v4().to_string(),
        central: satellite.snapshot.central.clone(),
        enrollment_generation: 5,
    };
    let old = satellite.accept_link(&open).unwrap();
    assert!(satellite.accept_link(&open).is_err());
    let next = satellite.challenge();
    open.challenge = next.challenge;
    open.incarnation = Uuid::new_v4().to_string();
    let current = satellite.accept_link(&open).unwrap();
    assert!(old.with_incarnation(|| Ok(())).is_err());
    current.with_incarnation(|| Ok(())).unwrap();
    let rebooted = channel(FederationRole::Satellite, 'a');
    assert!(rebooted.accept_link(&open).is_err());
}

#[tokio::test]
async fn federation_cross_channel_capability_cannot_authenticate_another_pair() {
    let router = federation_router(channel(FederationRole::Satellite, 'b'), allowed());
    let request = Request::builder()
        .uri("/federation/v1/hello")
        .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn federation_listener_rechecks_enrollment_on_every_request() {
    let current = Arc::new(AtomicBool::new(true));
    let check_current = current.clone();
    let check: FederationAuthorityCheck = Arc::new(move || {
        if check_current.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY))
        }
    });
    let router = federation_router(channel(FederationRole::Satellite, 'a'), check);
    let request = || {
        Request::builder()
            .uri("/federation/v1/hello")
            .header(
                FEDERATION_AUTHORITY_HEADER,
                caller_authority_for(FederationRole::Satellite),
            )
            .header(FEDERATION_INCARNATION_HEADER, Uuid::new_v4().to_string())
            .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    current.store(false, Ordering::SeqCst);
    assert_eq!(
        router.oneshot(request()).await.unwrap().status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn federation_listener_rejects_asserted_alias_and_stale_epochs() {
    let router = federation_router(channel(FederationRole::Satellite, 'a'), allowed());
    for mismatch in 0..4 {
        let snapshot = peer_snapshot(FederationRole::Central);
        let mut authority = FederationRequestAuthority::outgoing(&snapshot);
        match mismatch {
            0 => authority.caller.host_id = "Central::Alias".to_owned(),
            1 => authority.caller.host_generation += 1,
            2 => authority.central.host_generation += 1,
            _ => authority.enrollment_generation += 1,
        }
        let request = Request::builder()
            .uri("/federation/v1/hello")
            .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
            .header(
                FEDERATION_AUTHORITY_HEADER,
                serde_json::to_string(&authority).unwrap(),
            )
            .header(FEDERATION_INCARNATION_HEADER, Uuid::new_v4().to_string())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(request).await.unwrap().status(),
            StatusCode::CONFLICT
        );
    }
}
