// Owns startup/proof classification and configuration boundary regressions.
// Does not change production clocks or run SSH. Split from federation_startup_budget.rs.

use super::super::federation_onboarding_fixture::Pair as HostPair;
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn observation(pair: &HostPair) -> Value {
    pair.central.inner.lock().unwrap().federation_attestation[&pair.id].project()
}

#[test]
fn federation_attestation_timeout_settings_reject_instead_of_clamping() {
    for value in ["0", "999", "120001", "-1", "no", "18446744073709551616"] {
        assert_eq!(
            federation_timeout_setting(Some(value), FEDERATION_ATTEST_TIMEOUT),
            (FEDERATION_ATTEST_TIMEOUT, true)
        );
    }
    assert_eq!(
        federation_timeout_setting(None, FEDERATION_ATTEST_TIMEOUT),
        (Duration::from_secs(10), false)
    );
    for value in [1_000, 120_000] {
        assert_eq!(
            federation_timeout_setting(Some(&value.to_string()), FEDERATION_LINK_STARTUP_TIMEOUT),
            (Duration::from_millis(value), false)
        );
    }
}

#[tokio::test]
async fn federation_attestation_partial_first_frame_remains_startup() {
    let pair = HostPair::unattested(federation_registry::central()).await;
    let (c, mut s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    // A byte already arrived, but never a complete length prefix or frame.
    s.write_all(&[0]).await.unwrap();
    let result = federation_attest_central_started(
        &pair.rpc.channel,
        &mut c,
        tokio::time::Instant::now() - FEDERATION_LINK_STARTUP_TIMEOUT,
    )
    .await;
    let status = result.err().unwrap().status;
    let observed = observation(&pair);
    pair.finish().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(observed["lastStage"], "startup");
    assert_eq!(observed["lastReason"], "startupExpired");
    assert_eq!(observed["consecutiveFailures"], 0);
}

#[tokio::test]
async fn federation_attestation_overbound_first_frame_counts_parse_refusal() {
    let pair = HostPair::unattested(federation_registry::central()).await;
    let (c, mut s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    s.write_u32((FEDERATION_FRAME_LIMIT + 1) as u32)
        .await
        .unwrap();
    let status = federation_attest_central(&pair.rpc.channel, &mut c)
        .await
        .err()
        .unwrap()
        .status;
    let observed = observation(&pair);
    pair.finish().await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(observed["lastStage"], "attest");
    assert_eq!(observed["lastReason"], "invalidFrame");
    assert_eq!(observed["consecutiveFailures"], 1);
}

async fn proof_response(bad_tag: bool) -> (StatusCode, Value) {
    let pair = HostPair::unattested(federation_registry::central()).await;
    let (c, mut s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    let snapshot = pair.rpc.channel.snapshot.clone();
    let peer = async move {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        s.read_exact(&mut preface).await.unwrap();
        federation_write_frame(
            &mut s,
            &federation_encode(
                b"termal-hello-s-v1",
                &[
                    snapshot.peer.host_id.as_bytes(),
                    &snapshot.enrollment_generation.to_be_bytes(),
                    Uuid::new_v4().to_string().as_bytes(),
                    &[7; 32],
                ],
            ),
        )
        .await
        .unwrap();
        let _ = federation_read_frame(&mut s).await.unwrap();
        if bad_tag {
            federation_write_frame(
                &mut s,
                &federation_encode(b"termal-hello-proof-v1", &[&[0; 32]]),
            )
            .await
            .unwrap();
        }
        // EOF after a valid first frame is a transport loss during S2.
    };
    let (result, ()) = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        tokio::join!(federation_attest_central(&pair.rpc.channel, &mut c), peer)
    })
    .await
    .unwrap();
    let status = result.err().unwrap().status;
    let observed = observation(&pair);
    pair.finish().await;
    (status, observed)
}

#[tokio::test]
async fn federation_attestation_eof_during_proof_is_not_bad_key() {
    let (status, observed) = proof_response(false).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(observed["lastStage"], "attest");
    assert_eq!(observed["lastReason"], "linkDownBeforeTag");
    assert_eq!(observed["consecutiveFailures"], 0);
}

#[tokio::test]
async fn federation_attestation_bad_tag_is_named_crypto_refusal() {
    let (status, observed) = proof_response(true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(observed["lastStage"], "attest");
    assert_eq!(observed["lastReason"], "proofRefused");
    assert_eq!(observed["consecutiveFailures"], 1);
}

#[tokio::test]
async fn federation_attestation_report_does_not_reuse_prior_owner_or_basis_failure() {
    let pair = HostPair::unattested(federation_registry::central()).await;
    let channel = &pair.rpc.channel;
    let basis = channel.current_authority().unwrap();
    let owner = channel.next_security_owner().unwrap();
    channel.observe_attestation_failure(Some(owner), "attest", "proofRefused", true);
    let current = pair.central.federation_observed_attestation_failure(&basis);
    let mut successor = basis.clone();
    successor.transport_generation += 1;
    let stale_basis = pair
        .central
        .federation_observed_attestation_failure(&successor);
    channel.next_security_owner().unwrap();
    channel.observe_attestation_failure(Some(owner), "attest", "proofRefused", true);
    let pending_successor = pair.central.federation_observed_attestation_failure(&basis);
    pair.finish().await;
    assert!(current);
    assert!(!stale_basis && !pending_successor);
}
