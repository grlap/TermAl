// Owns retirement admission, replay and late-completion controls.
// Does not simulate child cancellation or claim an Unknown child stopped.
// Split from federation_onboarding_repairs.rs after its review regressions.
use super::federation_onboarding_fixture::{Pair, io};
use super::federation_onboarding_repairs::accepted_probe;
use super::*;

fn reconnect(pair: &Pair) -> (FederationPeerSnapshot, String) {
    let expected = pair
        .satellite
        .federation_peer_snapshot(&pair.satellite_channel.snapshot.peer.host_id)
        .unwrap();
    let incarnation = Uuid::new_v4().to_string();
    let expected = pair
        .satellite
        .federation_peer_connected(&expected, incarnation.clone())
        .unwrap();
    (expected, incarnation)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_retired_probe_keeps_unknown_and_late_observations() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let old = accepted_probe(&pair);
    let old = pair
        .satellite
        .inner
        .lock()
        .unwrap()
        .federation
        .onboarding
        .probes[&old.expected.peer.host_id]
        .clone();
    let (expected, incarnation) = reconnect(&pair);
    assert!(
        pair.satellite
            .prepare_federation_probe(&expected, &incarnation, &old.attempt_id)
            .is_err()
    );
    let fresh = pair
        .satellite
        .prepare_federation_probe(&expected, &incarnation, &Uuid::new_v4().to_string())
        .unwrap();
    let mut late = old.wake.clone().unwrap();
    late.state = FederationProbeWakeState::Delivered;
    assert!(
        pair.satellite
            .finish_federation_probe_wake(&old.expected, &old.link_incarnation, late.clone())
            .is_err()
    );
    let replay = FederationProbeWakeRequest {
        attempt_id: old.attempt_id.clone(),
        session_id: old.session_id.clone(),
    };
    assert!(
        pair.satellite
            .accept_federation_probe_wake(&expected, &incarnation, &replay)
            .is_err()
    );
    assert!(
        pair.satellite
            .prepare_federation_probe(&expected, &incarnation, &old.attempt_id)
            .is_err()
    );
    let loaded = load_state(pair.satellite.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let archive = &loaded.federation.onboarding.retired_probes[&expected.peer.host_id];
    assert_eq!(archive.len(), 1);
    let retired = &archive[0];
    assert_eq!(retired.probe.expected, old.expected);
    assert_eq!(retired.probe.basis, old.basis);
    assert_eq!(retired.probe.link_incarnation, old.link_incarnation);
    assert_eq!(
        retired.probe.wake.as_ref().unwrap().outcome_id,
        late.outcome_id
    );
    assert_eq!(
        retired.probe.wake.as_ref().unwrap().state,
        FederationProbeWakeState::Unknown
    );
    assert!(!retired.retired_at.is_empty());
    assert_eq!(retired.replay_observations, 2);
    assert_eq!(retired.last_completion.as_ref(), Some(&late));
    assert_eq!(
        loaded.federation.onboarding.probes[&expected.peer.host_id],
        fresh
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_reenrollment_retires_accepted_in_the_same_commit() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let old = accepted_probe(&pair);
    let id = old.expected.peer.host_id.clone();
    let record = pair.satellite.inner.lock().unwrap().federation.peers[&id].clone();
    let _ = enroll_federation_satellite(
        State(pair.satellite.clone()),
        AxumPath(id.clone()),
        Json(FederationEnrollmentRequest {
            record,
            engram_enabled: false,
            engram_command: None,
        }),
    )
    .await
    .unwrap();
    let loaded = load_state(pair.satellite.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.federation.peers[&id].enrollment_generation,
        old.expected.enrollment_generation + 1
    );
    assert!(!loaded.federation.onboarding.probes.contains_key(&id));
    assert_eq!(
        loaded.federation.onboarding.retired_probes[&id][0]
            .probe
            .wake
            .as_ref()
            .unwrap()
            .state,
        FederationProbeWakeState::Unknown
    );
    assert!(!loaded.federation.onboarding.engram_enabled.contains(&id));
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_archive_cap_refuses_without_evicting_evidence() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let initial = accepted_probe(&pair);
    for _ in 0..FEDERATION_RETIRED_PROBES_PER_PEER {
        let (expected, incarnation) = reconnect(&pair);
        let fresh = pair
            .satellite
            .prepare_federation_probe(&expected, &incarnation, &Uuid::new_v4().to_string())
            .unwrap();
        pair.satellite
            .accept_federation_probe_wake(
                &expected,
                &incarnation,
                &FederationProbeWakeRequest {
                    attempt_id: fresh.attempt_id,
                    session_id: fresh.session_id,
                },
            )
            .unwrap();
    }
    let (expected, incarnation) = reconnect(&pair);
    let before = pair.satellite.inner.lock().unwrap().federation.clone();
    assert!(
        pair.satellite
            .prepare_federation_probe(&expected, &incarnation, &Uuid::new_v4().to_string())
            .is_err()
    );
    let after = pair.satellite.inner.lock().unwrap().federation.clone();
    assert_eq!(before, after);
    assert_eq!(
        after.onboarding.retired_probes[&expected.peer.host_id].len(),
        FEDERATION_RETIRED_PROBES_PER_PEER
    );
    assert_eq!(
        after.onboarding.retired_probes[&expected.peer.host_id][0]
            .probe
            .session_id,
        initial.session_id
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_late_completion_before_prepare_is_unknown_evidence_only() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let old = accepted_probe(&pair);
    let mut late = pair
        .satellite
        .inner
        .lock()
        .unwrap()
        .federation
        .onboarding
        .probes[&old.expected.peer.host_id]
        .wake
        .clone()
        .unwrap();
    late.state = FederationProbeWakeState::Delivered;
    let (expected, incarnation) = reconnect(&pair);
    assert!(
        pair.satellite
            .finish_federation_probe_wake(&old.expected, &old.link_incarnation, late.clone())
            .is_err()
    );
    let fresh = pair
        .satellite
        .prepare_federation_probe(&expected, &incarnation, &Uuid::new_v4().to_string())
        .unwrap();
    let loaded = load_state(pair.satellite.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let retired = &loaded.federation.onboarding.retired_probes[&expected.peer.host_id][0];
    assert_eq!(
        retired.probe.wake.as_ref().unwrap().state,
        FederationProbeWakeState::Unknown
    );
    assert_eq!(retired.last_completion.as_ref(), Some(&late));
    assert_eq!(retired.replay_observations, 1);
    assert_ne!(fresh.session_id, old.session_id);
    assert!(fresh.wake.is_none());
    pair.finish().await;
}
