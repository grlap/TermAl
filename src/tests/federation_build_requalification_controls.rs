// Negative controls for automatic reconnect qualification, separate from the
// key/nonce/transcript wire attacks in federation_security_attestation.rs.
use super::federation_build_compatibility::measured;
use super::federation_onboarding_fixture::{Pair, io, profile};
use super::*;

async fn active_pair() -> Pair {
    let pair = Pair::new(federation_registry::central(), io()).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Active)
        .unwrap();
    pair
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_recovery_rejects_changed_authority() {
    let pair = active_pair().await;
    let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    let inner = pair.central.inner.lock().unwrap();
    let retained = federation_requalification_capture(&inner.federation, &current).unwrap();
    let peer = inner.federation.peers[&pair.id].clone();
    drop(inner);
    let mut hash_only = peer.clone();
    hash_only.profile.binary_sha256 = "c".repeat(64);
    assert!(
        retained.matches(&hash_only, &current),
        "hash is not authority"
    );
    for field in [
        "local",
        "peer",
        "central",
        "enrollment",
        "key",
        "endpoint",
        "protocol",
        "schema",
    ] {
        let mut changed = current.clone();
        let mut record = peer.clone();
        match field {
            "local" => changed.local.host_generation += 1,
            "peer" => changed.peer.host_generation += 1,
            "central" => changed.central.host_generation += 1,
            "enrollment" => changed.enrollment_generation += 1,
            "key" => changed.pair_capability_ref.push_str("-rotated"),
            "endpoint" => changed.endpoint.ssh_host.push_str(".other"),
            "protocol" => record.profile.protocol_version += 1,
            "schema" => record.profile.schema_version.push_str("-other"),
            _ => unreachable!(),
        }
        assert!(
            !retained.matches(&record, &changed),
            "{field} must not inherit authority"
        );
    }
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_explicit_diagnostic_refusal_does_not_activate_a_new_link() {
    for protocol in [true, false] {
        let profile = Arc::new(Mutex::new(profile()));
        let mut pair = Pair::new(federation_registry::central(), measured(profile.clone())).await;
        assert!(pair.verify().await.unwrap().termal_complete());
        let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .transition_federation_peer(&peer, FederationMembershipState::Active)
            .unwrap();
        {
            let mut changed = profile.lock().unwrap();
            changed.binary_sha256 = "d".repeat(64);
            if protocol {
                changed.protocol_version += 1;
            } else {
                changed.schema_version.push_str("-other");
            }
        }
        pair.reconnect().await;
        pair.rpc.health(&pair.lease).await.unwrap();
        let refused = pair.rpc.requalify(&pair.lease).await.unwrap_err();
        assert_eq!(refused.status, StatusCode::CONFLICT);
        assert!(refused.message.contains("re-attestation checks refused"));
        let inner = pair.central.inner.lock().unwrap();
        let peer = &inner.federation.peers[&pair.id];
        assert_eq!(peer.membership, FederationMembershipState::Active);
        assert!(!peer.verified_on(inner.federation.links.get(&pair.id)));
        assert!(peer.requalification.is_some());
        assert!(!inner.federation.onboarding.reports[&pair.id].termal_complete());
        drop(inner);
        pair.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_reenrollment_does_not_auto_activate() {
    let mut pair = active_pair().await;
    pair.reconnect().await;
    assert!(
        pair.central.inner.lock().unwrap().federation.peers[&pair.id]
            .requalification
            .is_some()
    );
    let record = pair.central.inner.lock().unwrap().federation.peers[&pair.id].clone();
    let previous_generation = record.enrollment_generation;
    pair.central.enroll_federation_peer(record).unwrap();
    let inner = pair.central.inner.lock().unwrap();
    let peer = &inner.federation.peers[&pair.id];
    assert_eq!(peer.enrollment_generation, previous_generation + 1);
    assert_eq!(peer.membership, FederationMembershipState::Enrolled);
    assert!(peer.requalification.is_none());
    assert!(peer.verification.is_none());
    drop(inner);
    assert!(
        pair.rpc.health(&pair.lease).await.is_err(),
        "retired generation cannot qualify its successor"
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_configuration_aba_clears_recovery() {
    let mut pair = active_pair().await;
    pair.reconnect().await;
    let original = pair
        .central
        .inner
        .lock()
        .unwrap()
        .preferences
        .federation
        .clone();
    let mut disabled = original.clone();
    disabled.enabled = false;
    pair.central.configure_federation(disabled).unwrap();
    pair.central.configure_federation(original).unwrap();
    let inner = pair.central.inner.lock().unwrap();
    let peer = &inner.federation.peers[&pair.id];
    assert!(peer.requalification.is_none());
    assert_eq!(peer.membership, FederationMembershipState::Enrolled);
    drop(inner);
    assert!(pair.rpc.health(&pair.lease).await.is_err());
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_revocation_clears_recovery() {
    let mut pair = active_pair().await;
    pair.reconnect().await;
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Revoked)
        .unwrap();
    let inner = pair.central.inner.lock().unwrap();
    let peer = &inner.federation.peers[&pair.id];
    assert!(peer.requalification.is_none());
    assert!(peer.verification.is_none());
    drop(inner);
    assert!(pair.rpc.health(&pair.lease).await.is_err());
    pair.finish().await;
}
