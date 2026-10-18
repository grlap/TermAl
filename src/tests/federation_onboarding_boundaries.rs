// Owns unresolved-wake, reverse-RPC and opt-in boundary regressions.
// Does not manufacture verification or operate user sessions. Split from
// federation_onboarding.rs to keep independent failure cases readable.
use super::federation_onboarding_fixture::{Pair, io};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_unresolved_durable_acceptance_never_relaunches() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let mut probe_io = io();
    let normal = probe_io.platform.clone();
    probe_io.platform = Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        normal()
    });
    let pair = Pair::new(federation_registry::central(), probe_io).await;
    let expected = pair.satellite_channel.current_authority().unwrap();
    let attempt_id = Uuid::new_v4().to_string();
    let probe = pair
        .satellite
        .prepare_federation_probe(&expected, &pair.lease.incarnation, &attempt_id)
        .unwrap();
    let request = FederationProbeWakeRequest {
        attempt_id,
        session_id: probe.session_id,
    };
    let (accepted, fresh) = pair
        .satellite
        .accept_federation_probe_wake(&expected, &pair.lease.incarnation, &request)
        .unwrap();
    assert!(fresh);
    assert_eq!(accepted.state, FederationProbeWakeState::Accepted);
    let duplicate: FederationProbeWakeOutcome = pair
        .rpc
        .request(FederationRpcRoute::Onboarding(
            pair.lease.clone(),
            FederationOnboardingRpc::Wake(request),
        ))
        .await
        .unwrap();
    assert_eq!(duplicate, accepted);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        pair.satellite
            .prepare_federation_probe(
                &expected,
                &pair.lease.incarnation,
                &Uuid::new_v4().to_string()
            )
            .is_err()
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_engram_disabled_never_invokes_external_command() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let mut probe_io = io();
    probe_io.engram = Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        FederationEngramOnboardingOutcome::unavailable()
    });
    let pair = Pair::new(federation_registry::central(), probe_io).await;
    let report = pair.verify().await.unwrap();
    assert!(report.engram.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_reverse_direction_failure_is_named() {
    let mut central_io = io();
    central_io.profile = Arc::new(|| Err(ApiError::internal("fake central identity failure")));
    let pair = Pair::with_central_io(federation_registry::central(), io(), central_io).await;
    let report = pair.verify().await.unwrap();
    assert!(
        report
            .outcomes
            .iter()
            .any(|check| check.check == FederationOnboardingCheck::ReverseRpc && !check.passed)
    );
    assert!(
        report
            .outcomes
            .iter()
            .any(|check| check.check == FederationOnboardingCheck::ForwardRpc && check.passed)
    );
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Enrolled
    );
    pair.finish().await;
}
