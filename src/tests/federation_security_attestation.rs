// Owns wrong-key, transcript, malformed-frame and context replacement controls.
// Does not fabricate a successful handshake. Split from the security wire tests.

use super::*;

#[tokio::test]
async fn federation_security_real_attestation_agrees_on_keys_and_signs_both_sides() {
    let (satellite, control) = established().await;
    let headers = satellite.authority_headers(&control.session.incarnation);
    let (headers, representation) = sign_request(
        &control.session,
        1,
        "GET",
        "/federation/v1/health",
        headers,
        &[],
    );
    let response = wire(satellite.port, "GET", "/federation/v1/health", headers, &[]).await;
    let verified = response.verified(&control.session, 1, &representation);
    let current = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    drop(control);
    satellite.finish().await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(
        verified,
        "response must prove the independently derived key"
    );
    let health: FederationHealth = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(health.link_incarnation, current);
}

#[tokio::test]
async fn federation_security_legacy_bearer_never_grants_authority() {
    let channel = super::super::federation_link::channel(FederationRole::Satellite, 'a');
    let incarnation = Uuid::new_v4().to_string();
    channel.activate(incarnation.clone());
    let mut outgoing = channel.snapshot.clone();
    std::mem::swap(&mut outgoing.local, &mut outgoing.peer);
    let authority =
        serde_json::to_string(&FederationRequestAuthority::outgoing(&outgoing)).unwrap();
    let response = federation_router(channel, super::super::federation_link::allowed())
        .oneshot(
            Request::builder()
                .uri("/federation/v1/health")
                .header(FEDERATION_CAPABILITY_HEADER, "a".repeat(64))
                .header(FEDERATION_AUTHORITY_HEADER, authority)
                .header(FEDERATION_INCARNATION_HEADER, incarnation)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !response.status().is_success(),
        "a bearer is never a control attestation or request MAC"
    );
}

#[tokio::test]
async fn federation_security_wrong_pair_key_refused_without_installation() {
    let satellite = Satellite::new().await;
    let result = Control::start(&satellite, 'b', |_| {}).await;
    let active = satellite.handle.channel.current_lease().is_ok();
    satellite.finish().await;
    assert!(result.is_err() && !active);
}

#[tokio::test]
async fn federation_security_changed_transcript_fields_refused() {
    let satellite = Satellite::new().await;
    for index in [0, 1, 2, 3, 5, 7] {
        let result = Control::start(&satellite, 'a', move |fields| {
            fields[index][0] ^= 1;
        })
        .await;
        assert!(
            result.is_err(),
            "transcript field {index} must match enrollment and this boot"
        );
        assert!(satellite.handle.channel.current_lease().is_err());
    }
    satellite.finish().await;
}

#[tokio::test]
async fn federation_security_failed_replacement_keeps_current_context_and_window() {
    let (satellite, mut control) = established().await;
    let old = satellite.handle.channel.current_lease().unwrap();
    assert!(Control::start(&satellite, 'b', |_| {}).await.is_err());
    control.heartbeat(2, &[8; 16]).await.unwrap();
    let retained = old.with_incarnation(|| Ok(())).is_ok();
    drop(control);
    satellite.finish().await;
    assert!(
        retained,
        "an unauthenticated replacement cannot fence a healthy context"
    );
}

#[tokio::test]
async fn federation_security_captured_handshake_cannot_answer_a_fresh_nonce() {
    let (satellite, mut valid) = established().await;
    let incarnation = valid.session.incarnation.clone();
    let mut replay = tokio::net::TcpStream::connect(("127.0.0.1", satellite.port))
        .await
        .unwrap();
    replay.write_all(CONTROL_PREFACE).await.unwrap();
    let hello = read_frame(&mut replay).await.unwrap();
    assert!(decode(&hello, b"termal-hello-s-v1", 4).is_ok());
    write_frame(&mut replay, &valid.hello_c).await.unwrap();
    let refused = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut replay))
        .await
        .unwrap();
    assert!(
        refused.is_err(),
        "a captured HMAC does not prove a new receiver nonce"
    );
    valid.heartbeat(2, &[12; 16]).await.unwrap();
    let retained = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    drop(replay);
    drop(valid);
    satellite.finish().await;
    assert_eq!(retained, incarnation);
}

#[tokio::test]
async fn federation_security_old_control_eof_cannot_retire_successor() {
    let (satellite, old) = established().await;
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let newer = successor.session.incarnation.clone();
    drop(old);
    successor.heartbeat(2, &[9; 16]).await.unwrap();
    let actual = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    drop(successor);
    satellite.finish().await;
    assert_eq!(
        actual, newer,
        "delayed EOF carries the old owner generation"
    );
}

#[tokio::test]
async fn federation_security_delayed_exact_owner_fence_cannot_retire_successor() {
    let (satellite, old) = established().await;
    let old_context = satellite.handle.channel.security_context().unwrap();
    let mut successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    satellite
        .handle
        .channel
        .fence_security(old_context.owner, &old_context.incarnation);
    successor.heartbeat(2, &[11; 16]).await.unwrap();
    let retained = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation;
    let expected = successor.session.incarnation.clone();
    drop(old);
    drop(successor);
    satellite.finish().await;
    assert_eq!(retained, expected);
}

#[tokio::test]
async fn federation_security_replayed_old_request_is_fenced_after_new_attestation() {
    let (satellite, old) = established().await;
    let headers = satellite.authority_headers(&old.session.incarnation);
    let (headers, _) = sign_request(
        &old.session,
        1,
        "GET",
        "/federation/v1/health",
        headers,
        &[],
    );
    let successor = Control::start(&satellite, 'a', |_| {}).await.unwrap();
    let response = wire(satellite.port, "GET", "/federation/v1/health", headers, &[]).await;
    let unchanged = satellite
        .handle
        .channel
        .current_lease()
        .unwrap()
        .incarnation
        == successor.session.incarnation;
    drop(old);
    drop(successor);
    satellite.finish().await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert!(unchanged);
}

#[tokio::test]
async fn federation_security_malformed_control_frames_are_bounded_and_install_nothing() {
    let satellite = Satellite::new().await;
    let valid = encode(
        b"termal-hello-c-v1",
        &[vec![0; 16], vec![0; 32], vec![0; 32]],
    );
    let mut duplicate = valid.clone();
    duplicate.extend_from_slice(&[2, 0, 0, 0, 0]);
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut unknown = valid.clone();
    unknown[1] = 250;
    let mut wrong_version = valid;
    wrong_version[0] = 2;
    for frame in [duplicate, trailing, unknown, wrong_version] {
        let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", satellite.port))
            .await
            .unwrap();
        socket.write_all(CONTROL_PREFACE).await.unwrap();
        let _ = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut socket)).await;
        if write_frame(&mut socket, &frame).await.is_ok() {
            let result =
                tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut socket)).await;
            assert!(
                !matches!(result, Ok(Ok(ref value)) if decode(value, b"termal-hello-proof-v1", 1).is_ok())
            );
        }
        assert!(satellite.handle.channel.current_lease().is_err());
    }
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", satellite.port))
        .await
        .unwrap();
    socket.write_all(CONTROL_PREFACE).await.unwrap();
    let _ = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut socket)).await;
    let _ = socket.write_all(&65537_u32.to_be_bytes()).await;
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut socket)).await;
    let refused = !matches!(result, Ok(Ok(_)));
    drop(socket);
    satellite.finish().await;
    assert!(
        refused,
        "length is rejected before allocation or waiting for a body"
    );
}

#[tokio::test]
async fn federation_security_http_and_control_routes_cannot_substitute_for_each_other() {
    let (satellite, mut control) = established().await;
    let incarnation = control.session.incarnation.clone();
    for (counter, target) in [(1, "/federation/v1/hello"), (2, "/federation/v1/link")] {
        let (headers, repr) = sign_request(
            &control.session,
            counter,
            "POST",
            target,
            satellite.authority_headers(&incarnation),
            CONTROL_PREFACE,
        );
        let response = wire(satellite.port, "POST", target, headers, CONTROL_PREFACE).await;
        assert_eq!(response.status, StatusCode::NOT_FOUND);
        assert!(response.verified(&control.session, counter, &repr));
    }
    let mut wrong_path = tokio::net::TcpStream::connect(("127.0.0.1", satellite.port))
        .await
        .unwrap();
    wrong_path.write_all(CONTROL_PREFACE).await.unwrap();
    read_frame(&mut wrong_path).await.unwrap();
    wrong_path
        .write_all(b"GET /federation/v1/health HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, read_frame(&mut wrong_path))
            .await
            .unwrap()
            .is_err()
    );
    control.heartbeat(2, &[13; 16]).await.unwrap();
    assert_eq!(
        satellite
            .handle
            .channel
            .current_lease()
            .unwrap()
            .incarnation,
        incarnation
    );
    drop(wrong_path);
    drop(control);
    satellite.finish().await;
}

#[tokio::test]
async fn federation_security_three_refusals_are_visible_without_key_material() {
    let satellite = Satellite::new().await;
    for _ in 0..3 {
        assert!(Control::start(&satellite, 'b', |_| {}).await.is_err());
    }
    let view = satellite.app.federation_registry_view().unwrap();
    let attestation = view["peers"][0]["attestation"].clone();
    let encoded = serde_json::to_string(&attestation).unwrap();
    satellite.finish().await;
    assert_eq!(attestation["state"], "failing");
    assert!(attestation["consecutiveFailures"].as_u64().unwrap_or(0) >= 3);
    assert!(!encoded.contains(&"a".repeat(64)) && !encoded.contains("pairCapabilityRef"));
}
