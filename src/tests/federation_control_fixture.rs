// Owns real mutual-proof setup and signed requests for transport unit tests.
// Does not bypass MAC admission or model SSH ownership. Split from federation_link tests.

use super::*;

mod clock {
    use super::*;
    include!("federation_control_fixture_clock.rs");
}

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
    attest_over(central, satellite, &mut central_io, &mut satellite_io).await
}

/// `attest`, with every central-to-satellite frame after the preface
/// arriving `delay` of real time late, as on a slow loaded host. Central's
/// proof timer runs while its challenge is in flight.
pub(super) async fn attest_after_satellite_delay(
    central: &FederationChannel,
    satellite: &FederationChannel,
    delay: Duration,
) -> Attested {
    let (central_io, central_far) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let (satellite_far, satellite_io) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let (central_read, central_write) = tokio::io::split(central_far);
    let (satellite_read, satellite_write) = tokio::io::split(satellite_far);
    let preface = FEDERATION_CONTROL_PREFACE.len();
    let forward_down = tokio::spawn(fixture_forward(central_read, satellite_write, preface, delay));
    let forward_up = tokio::spawn(fixture_forward(satellite_read, central_write, usize::MAX, Duration::ZERO));
    let mut central_io: FederationControlIo = Box::new(central_io);
    let mut satellite_io: FederationControlIo = Box::new(satellite_io);
    let attested = attest_over(central, satellite, &mut central_io, &mut satellite_io).await;
    forward_down.abort();
    forward_up.abort();
    attested
}

/// Copies bytes, sleeping `delay` before each chunk once `undelayed` bytes passed.
async fn fixture_forward(
    mut from: impl tokio::io::AsyncRead + Unpin,
    mut to: impl tokio::io::AsyncWrite + Unpin,
    undelayed: usize,
    delay: Duration,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut passed, mut buffer) = (0usize, vec![0u8; FEDERATION_FRAME_LIMIT]);
    while let Ok(read) = from.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        if passed >= undelayed {
            tokio::time::sleep(delay).await;
        }
        passed = passed.saturating_add(read);
        if to.write_all(&buffer[..read]).await.is_err() {
            break;
        }
    }
}

/// The channel this fixture attests on. Fixtures prove attestation, not host
/// scheduling, so a channel that has not opted into scripted protocol time
/// attests on a clone whose deadlines measure a scripted clock: elapsed wall
/// time on a loaded host never expires the window. The clone shares the
/// channel's link state, so the proof is installed for the caller. A channel
/// that opted in keeps its own clock, which its test may advance.
fn fixture_attest_channel(channel: &FederationChannel) -> FederationChannel {
    let mut channel = channel.clone();
    if !channel.control.clock.io_deadlines {
        channel.control.clock = FederationControlClock::scripted_io();
    }
    channel
}

async fn attest_over(
    central: &FederationChannel,
    satellite: &FederationChannel,
    central_io: &mut FederationControlIo,
    satellite_io: &mut FederationControlIo,
) -> Attested {
    let (central, satellite) = (fixture_attest_channel(central), fixture_attest_channel(satellite));
    let (central, satellite) = (&central, &satellite);
    let satellite_proof = async {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        tokio::io::AsyncReadExt::read_exact(&mut *satellite_io, &mut preface)
            .await
            .unwrap();
        assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
        federation_attest_satellite(satellite, satellite_io).await
    };
    let (c, s) = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::join!(
            federation_attest_central(central, central_io),
            satellite_proof
        )
    })
    .await
    .unwrap();
    let (central_context, central_guard) = c.unwrap();
    let (satellite_context, satellite_guard) = s.unwrap();
    // This fixture used to stop at mutual proof. Satellite RPC now needs the
    // existing first heartbeat too: run the real receiver, never set its flag.
    let receive = federation_control_satellite_established(
        satellite, satellite_io, &satellite_context,
    );
    tokio::pin!(receive);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::select! {
            _ = &mut receive => panic!("satellite control ended before readiness"),
            _ = heartbeat_round_trip(central_io, &central_context, 1) => {},
        }
    }).await.expect("first authenticated heartbeat");
    Attested { _central: central_guard, _satellite: satellite_guard }
}

pub(super) fn heartbeat_request(context: &FederationSecurityContext, sequence: u64) -> Vec<u8> {
    let sequence = sequence.to_be_bytes();
    let challenge = [7; 16];
    let tag = federation_mac(&context.keys[4],
        &[b"hb-req", &context.transcript, &sequence, &challenge]).unwrap();
    federation_encode(b"termal-hb-req-v1", &[&sequence, &challenge, &tag])
}

pub(super) async fn heartbeat_round_trip(
    io: &mut FederationControlIo,
    context: &FederationSecurityContext,
    sequence: u64,
) {
    federation_write_frame(io, &heartbeat_request(context, sequence)).await.unwrap();
    let response = federation_read_frame(io).await.unwrap();
    let fields = federation_decode(&response, b"termal-hb-resp-v1", 2).unwrap();
    assert_eq!(fields[0], sequence.to_be_bytes());
    federation_verify_mac(&context.keys[5],
        &[b"hb-resp", &context.transcript, fields[0], &[7; 16]], fields[1]).unwrap();
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
