// Owns authenticated request/response tamper, replay and concurrency controls.
// Does not implement replay or application dedup. Split from security attestation tests.

use super::*;

const HEALTH: &str = "/federation/v1/health";

#[tokio::test]
async fn federation_security_noncanonical_mac_headers_do_not_consume_counter() {
    let (satellite, control) = established().await;
    let (headers, _) = sign_request(
        &control.session,
        1,
        "GET",
        HEALTH,
        satellite.authority_headers(&control.session.incarnation),
        &[],
    );
    let original = headers[AUTH_HEADER].to_str().unwrap();
    let fields: Vec<_> = original.split(':').collect();
    let other = Uuid::new_v4().to_string();
    for invalid in [
        original.to_owned() + ":extra",
        fields[..4].join(":"),
        original.replacen(":1:", ":01:", 1),
        original.replacen(":1:", ":+1:", 1),
        original.replacen(":1:", ":18446744073709551616:", 1),
        original.replacen(":c2s:", ":s2c:", 1),
        original.replace(&control.session.incarnation, &other),
        format!("{original}="),
        format!("{}:{}:{}:{}:AA", fields[0], fields[1], fields[2], fields[3]),
    ] {
        let mut changed = headers.clone();
        changed.insert(AUTH_HEADER, HeaderValue::from_str(&invalid).unwrap());
        assert_eq!(
            wire(satellite.port, "GET", HEALTH, changed, &[])
                .await
                .status,
            StatusCode::CONFLICT
        );
    }
    let accepted = wire(satellite.port, "GET", HEALTH, headers, &[]).await;
    drop(control);
    satellite.finish().await;
    assert_eq!(
        accepted.status,
        StatusCode::OK,
        "parsing refusals never reserve the valid intent"
    );
}

#[tokio::test]
async fn federation_security_reordering_is_accepted_but_counter_replay_is_refused() {
    let (satellite, control) = established().await;
    for counter in [1, 3, 2] {
        let (headers, representation) = sign_request(
            &control.session,
            counter,
            "GET",
            HEALTH,
            satellite.authority_headers(&control.session.incarnation),
            &[],
        );
        let response = wire(satellite.port, "GET", HEALTH, headers.clone(), &[]).await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(response.verified(&control.session, counter, &representation));
        let replay = wire(satellite.port, "GET", HEALTH, headers, &[]).await;
        assert_eq!(replay.status, StatusCode::CONFLICT);
    }
    drop(control);
    satellite.finish().await;
}

#[tokio::test]
async fn federation_security_concurrent_duplicate_counter_admits_at_most_once() {
    let (satellite, control) = established().await;
    let (headers, _) = sign_request(
        &control.session,
        1,
        "GET",
        HEALTH,
        satellite.authority_headers(&control.session.incarnation),
        &[],
    );
    let (first, second) = tokio::join!(
        wire(satellite.port, "GET", HEALTH, headers.clone(), &[]),
        wire(satellite.port, "GET", HEALTH, headers, &[]),
    );
    let statuses = [first.status, second.status];
    drop(control);
    satellite.finish().await;
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1
    );
}

#[tokio::test]
async fn federation_security_invalid_high_counter_cannot_advance_the_window() {
    let (satellite, control) = established().await;
    let (headers, _) = sign_request(
        &control.session,
        1,
        "GET",
        HEALTH,
        satellite.authority_headers(&control.session.incarnation),
        &[],
    );
    let mut invalid = headers.clone();
    let value =
        invalid[AUTH_HEADER]
            .to_str()
            .unwrap()
            .replacen(":1:", &format!(":{}:", u64::MAX), 1);
    invalid.insert(AUTH_HEADER, HeaderValue::from_str(&value).unwrap());
    let forged = wire(satellite.port, "GET", HEALTH, invalid, &[]).await;
    let valid = wire(satellite.port, "GET", HEALTH, headers, &[]).await;
    drop(control);
    satellite.finish().await;
    assert_eq!(forged.status, StatusCode::CONFLICT);
    assert_eq!(
        valid.status,
        StatusCode::OK,
        "MAC verification must precede counter reservation"
    );
}

#[tokio::test]
async fn federation_security_counter_older_than_window_is_refused() {
    let (satellite, control) = established().await;
    let mut statuses = Vec::new();
    for counter in [65, 1, 2] {
        let (headers, _) = sign_request(
            &control.session,
            counter,
            "GET",
            HEALTH,
            satellite.authority_headers(&control.session.incarnation),
            &[],
        );
        statuses.push(
            wire(satellite.port, "GET", HEALTH, headers, &[])
                .await
                .status,
        );
    }
    drop(control);
    satellite.finish().await;
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT, StatusCode::OK]
    );
}

#[tokio::test]
async fn federation_security_method_target_body_and_semantic_headers_are_mac_bound() {
    let (satellite, control) = established().await;
    let mut original = satellite.authority_headers(&control.session.incarnation);
    original.insert("content-type", HeaderValue::from_static("application/json"));
    original.insert(
        "x-termal-qualified-sender",
        HeaderValue::from_static("sender-one"),
    );
    original.insert(
        "x-termal-idempotency-key",
        HeaderValue::from_static("same-intent"),
    );
    original.insert(
        "x-termal-state-stamp",
        HeaderValue::from_static("state-one"),
    );
    original.insert("x-termal-topic", HeaderValue::from_static("topic-one"));
    let target = "/federation/v1/health?phase=one";
    let (headers, _) = sign_request(&control.session, 1, "GET", target, original, b"{}");
    let changed_query = wire(
        satellite.port,
        "GET",
        "/federation/v1/health?phase=two",
        headers.clone(),
        b"{}",
    )
    .await;
    assert_eq!(changed_query.status, StatusCode::CONFLICT);
    let changed_body = wire(satellite.port, "GET", target, headers.clone(), b"{ }").await;
    assert_eq!(changed_body.status, StatusCode::CONFLICT);
    let changed_method = wire(satellite.port, "POST", target, headers.clone(), b"{}").await;
    assert_eq!(changed_method.status, StatusCode::CONFLICT);
    for name in [
        "content-type",
        "content-encoding",
        "x-termal-qualified-sender",
        "x-termal-idempotency-key",
        "x-termal-state-stamp",
        "x-termal-topic",
    ] {
        let mut changed = headers.clone();
        changed.insert(name, HeaderValue::from_static("changed"));
        assert_eq!(
            wire(satellite.port, "GET", target, changed, b"{}")
                .await
                .status,
            StatusCode::CONFLICT,
            "{name}"
        );
    }
    let valid = wire(satellite.port, "GET", target, headers, b"{}").await;
    drop(control);
    satellite.finish().await;
    assert_eq!(
        valid.status,
        StatusCode::OK,
        "refused mutations never consume the signed intent"
    );
}

#[tokio::test]
async fn federation_security_duplicate_auth_and_forbidden_fields_are_refused() {
    let (satellite, control) = established().await;
    let (headers, _) = sign_request(
        &control.session,
        1,
        "GET",
        HEALTH,
        satellite.authority_headers(&control.session.incarnation),
        &[],
    );
    let mut duplicate = headers.clone();
    duplicate.append(AUTH_HEADER, headers[AUTH_HEADER].clone());
    let response = wire(satellite.port, "GET", HEALTH, duplicate, &[]).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    for name in ["authorization", "cookie", "x-unlisted-principal"] {
        let mut forbidden = headers.clone();
        forbidden.insert(name, HeaderValue::from_static("untrusted"));
        assert_eq!(
            wire(satellite.port, "GET", HEALTH, forbidden, &[])
                .await
                .status,
            StatusCode::CONFLICT,
            "{name}"
        );
    }
    let valid = wire(satellite.port, "GET", HEALTH, headers, &[]).await;
    drop(control);
    satellite.finish().await;
    assert_eq!(valid.status, StatusCode::OK);
}

#[tokio::test]
async fn federation_security_direction_and_domain_keys_cannot_be_interchanged() {
    let (satellite, control) = established().await;
    let original = satellite.authority_headers(&control.session.incarnation);
    let representation = request_repr(&control.session, 1, "c2s", "GET", HEALTH, &original, &[]);
    for wrong_key in [1, 2, 3, 4, 5] {
        let mut headers = original.clone();
        let value = authentication(
            &control.session,
            1,
            "c2s",
            &mac(&control.session.keys[wrong_key], &representation),
        );
        headers.insert(AUTH_HEADER, HeaderValue::from_str(&value).unwrap());
        assert_eq!(
            wire(satellite.port, "GET", HEALTH, headers, &[])
                .await
                .status,
            StatusCode::CONFLICT
        );
    }
    let (headers, _) = sign_request(&control.session, 1, "GET", HEALTH, original, &[]);
    let valid = wire(satellite.port, "GET", HEALTH, headers, &[]).await;
    drop(control);
    satellite.finish().await;
    assert_eq!(valid.status, StatusCode::OK);
}

#[tokio::test]
async fn federation_security_changed_status_or_response_metadata_is_outcome_unknown() {
    for fault in 1..=5 {
        let pair = Pair::established().await;
        pair.relay.response_fault.store(fault, Ordering::SeqCst);
        let lease = pair.central_peer.channel.current_lease().unwrap();
        let result = pair.central_peer.rpc.health(&lease).await;
        let exercised = pair.relay.response_fault.load(Ordering::SeqCst) == 0;
        pair.finish().await;
        assert!(
            exercised,
            "exercise the real response path before judging refusal"
        );
        assert!(
            result.is_err(),
            "unsigned or altered response never becomes a negative operation result"
        );
    }
}

#[tokio::test]
async fn federation_security_response_cannot_answer_another_request() {
    let (satellite, control) = established().await;
    let mut representations = Vec::new();
    let mut responses = Vec::new();
    for counter in [1, 2] {
        let (headers, representation) = sign_request(
            &control.session,
            counter,
            "GET",
            HEALTH,
            satellite.authority_headers(&control.session.incarnation),
            &[],
        );
        representations.push(representation);
        responses.push(wire(satellite.port, "GET", HEALTH, headers, &[]).await);
    }
    let first_valid = responses[0].verified(&control.session, 1, &representations[0]);
    let repaired = responses[0].verified(&control.session, 2, &representations[1]);
    drop(control);
    satellite.finish().await;
    assert!(first_valid && !repaired);
}
