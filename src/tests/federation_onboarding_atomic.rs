// Owns atomic enrollment/verification callback and failed-fence controls.
// Does not replace persistence tests. Split from onboarding review regressions;
// the controlled writer comes from federation_onboarding_writer.rs.
use super::federation_onboarding_fixture::{Pair, io};
use super::federation_onboarding_writer::ControlledWriter;
use super::*;

#[test]
fn federation_onboarding_enroll_callback_error_installs_nothing() {
    let app = federation_registry::central();
    let peer = federation_registry::peer(47461);
    app.enroll_federation_peer(peer.clone()).unwrap();
    let before = app.inner.lock().unwrap().federation.clone();
    let id = peer.identity.host_id.clone();
    assert!(
        app.enroll_federation_peer_with(peer, |domain, installed| {
            assert_eq!(
                installed.enrollment_generation,
                before.peers[&id].enrollment_generation + 1
            );
            assert_eq!(installed.membership, FederationMembershipState::Enrolled);
            assert!(installed.verification.is_none());
            domain.onboarding.engram_enabled.insert(id.clone());
            Err(ApiError::conflict("injected configure rejection"))
        })
        .is_err()
    );
    assert_eq!(app.inner.lock().unwrap().federation, before);
    assert_eq!(
        load_state(app.persistence_path.as_path())
            .unwrap()
            .unwrap()
            .federation
            .onboarding,
        before.onboarding
    );
}

#[test]
fn federation_onboarding_verify_callback_error_and_stale_basis_install_nothing() {
    let app = federation_registry::central();
    let expected = federation_registry::connected(&app, federation_registry::peer(47462));
    let before = app.inner.lock().unwrap().federation.clone();
    assert!(
        app.verify_federation_peer_with(&expected, federation_registry::checks(), |domain| {
            assert_eq!(
                domain.peers[&expected.peer.host_id].membership,
                FederationMembershipState::Verified
            );
            domain
                .onboarding
                .engram_enabled
                .insert(expected.peer.host_id.clone());
            Err(ApiError::conflict("injected report rejection"))
        })
        .is_err()
    );
    assert_eq!(app.inner.lock().unwrap().federation, before);
    let mut stale = expected.clone();
    stale.record_generation += 1;
    assert!(
        app.verify_federation_peer_with(&stale, federation_registry::checks(), |_| {
            panic!("stale completion must not invoke callback")
        })
        .is_err()
    );
    assert_eq!(app.inner.lock().unwrap().federation, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_failed_enrollment_fence_retains_coherent_candidate_and_hold() {
    let mut app = federation_registry::central();
    let peer = federation_registry::peer(47463);
    let id = peer.identity.host_id.clone();
    app.enroll_federation_peer(peer.clone()).unwrap();
    let mut writer = ControlledWriter::attach(&mut app);
    let worker = app.clone();
    let enabled = id.clone();
    let job = tokio::task::spawn_blocking(move || {
        worker.enroll_federation_peer_with(peer, |domain, _| {
            domain.onboarding.engram_enabled.insert(enabled);
            Ok(())
        })
    });
    let fence = writer.take().await;
    fence.finish(Err(PersistFenceError::WriteFailed(
        "injected writer failure".to_owned(),
    )));
    assert!(job.await.unwrap().is_err());
    let inner = app.inner.lock().unwrap();
    assert!(!inner.federation_durable);
    assert_eq!(inner.federation.peers[&id].enrollment_generation, 2);
    assert!(inner.federation.onboarding.engram_enabled.contains(&id));
    drop(inner);
    assert!(app.federation_peer_snapshot(&id).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_failed_verification_fence_retains_report_proof_and_hold() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    let mut writer = ControlledWriter::attach(&mut pair.central);
    let job = tokio::spawn(verify_federation_satellite(
        pair.central.clone(),
        pair.id.clone(),
    ));
    let fence = writer.take().await;
    fence.finish(Err(PersistFenceError::WriteFailed(
        "injected verification write failure".to_owned(),
    )));
    assert!(job.await.unwrap().is_err());
    let inner = pair.central.inner.lock().unwrap();
    assert!(!inner.federation_durable);
    assert!(inner.federation.onboarding.reports[&pair.id].termal_complete());
    assert!(inner.federation.peers[&pair.id].verification.is_some());
    drop(inner);
    assert!(pair.central.federation_peer_snapshot(&pair.id).is_err());
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_delayed_incomplete_report_cannot_replace_verifying_report() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let complete = pair.verify().await.unwrap();
    let mut delayed = complete.clone();
    delayed.outcomes[0].passed = false;
    assert!(
        pair.central
            .save_federation_onboarding_report(delayed)
            .is_err()
    );
    let loaded = load_state(pair.central.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(loaded.federation.onboarding.reports[&pair.id], complete);
    assert_eq!(
        loaded.federation.peers[&pair.id].membership,
        FederationMembershipState::Verified
    );
    pair.finish().await;
}
