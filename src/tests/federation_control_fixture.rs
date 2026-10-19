// Owns real mutual-proof setup and signed requests for transport unit tests.
// Does not bypass MAC admission or model SSH ownership. Split from federation_link tests.

use super::*;

pub(super) struct Attested {
    _central: FederationControlGuard,
    _satellite: FederationControlGuard,
}

pub(super) async fn establish(channel: &FederationChannel) -> (FederationChannel, Attested) {
    let mut opposite = channel.snapshot.clone();
    std::mem::swap(&mut opposite.local, &mut opposite.peer);
    opposite.local_role = if opposite.local_role == FederationRole::Central {
        FederationRole::Satellite
    } else {
        FederationRole::Central
    };
    let peer = FederationChannel::new(
        opposite,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
    );
    let proof = if channel.snapshot.local_role == FederationRole::Central {
        attest(channel, &peer).await
    } else {
        attest(&peer, channel).await
    };
    (peer, proof)
}

pub(super) fn authenticated_router(channel: FederationChannel, router: Router) -> Router {
    let state = FederationListenerState {
        channel,
        ensure_current: Arc::new(|| Ok(())),
    };
    router.layer(axum::middleware::from_fn_with_state(
        state,
        federation_authenticate,
    ))
}

pub(super) async fn attest(central: &FederationChannel, satellite: &FederationChannel) -> Attested {
    let (central_io, satellite_io) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut central_io: FederationControlIo = Box::new(central_io);
    let mut satellite_io: FederationControlIo = Box::new(satellite_io);
    let satellite_proof = async {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        tokio::io::AsyncReadExt::read_exact(&mut satellite_io, &mut preface)
            .await
            .unwrap();
        assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
        federation_attest_satellite(satellite, &mut satellite_io).await
    };
    let (c, s) = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::join!(
            federation_attest_central(central, &mut central_io),
            satellite_proof
        )
    })
    .await
    .unwrap();
    Attested {
        _central: c.unwrap().1,
        _satellite: s.unwrap().1,
    }
}

pub(super) fn signed(
    channel: &FederationChannel,
    counter: u64,
    method: &str,
    target: &str,
    mut headers: axum::http::HeaderMap,
    body: &[u8],
) -> Request<Body> {
    let context = channel.security_context().unwrap();
    let direction = if channel.snapshot.local_role == FederationRole::Central {
        "c2s"
    } else {
        "s2c"
    };
    let key = if direction == "c2s" { 0 } else { 2 };
    headers.insert(
        FEDERATION_INCARNATION_HEADER,
        HeaderValue::from_str(&context.incarnation).unwrap(),
    );
    if !headers.contains_key(FEDERATION_AUTHORITY_HEADER) {
        headers.insert(
            FEDERATION_AUTHORITY_HEADER,
            HeaderValue::from_str(
                &serde_json::to_string(&FederationRequestAuthority::outgoing(&channel.snapshot))
                    .unwrap(),
            )
            .unwrap(),
        );
    }
    let representation = federation_http_representation(
        &context, counter, direction, method, target, &headers, body,
    )
    .unwrap();
    let tag = federation_mac(&context.keys[key], &[&representation]).unwrap();
    headers.insert(
        FEDERATION_MAC_HEADER,
        federation_mac_header(&context, direction, counter, &tag).unwrap(),
    );
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .body(Body::from(body.to_vec()))
        .unwrap();
    *request.headers_mut() = headers;
    request
}
