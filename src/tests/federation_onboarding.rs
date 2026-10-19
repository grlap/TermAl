// Owns onboarding's authenticated fake-satellite acceptance and failure cases.
// Does not provision real hosts or replace platform/pilot proof. New O1 tests
// use federation_onboarding_fixture.rs and production checklist entrypoints.
use super::federation_onboarding_fixture::{Pair, io};
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_two_satellites_verify_by_configuration_without_activation() {
    let central = federation_registry::central();
    let first = Pair::new(central.clone(), io()).await;
    let second = Pair::new(central.clone(), io()).await;
    for pair in [&first, &second] {
        let report = pair.verify().await.unwrap();
        assert!(
            report.outcomes.iter().all(|check| check.passed),
            "{:?}",
            report.outcomes
        );
        assert!(report.termal_complete());
        assert!(report.engram.is_none());
        assert_eq!(
            central
                .federation_peer_snapshot(&pair.id)
                .unwrap()
                .membership,
            FederationMembershipState::Verified
        );
        let inner = pair.satellite.inner.lock().unwrap();
        let wake = inner
            .federation
            .onboarding
            .probes
            .values()
            .next()
            .unwrap()
            .wake
            .as_ref()
            .unwrap();
        assert_eq!(wake.state, FederationProbeWakeState::Delivered);
        assert_eq!(report.wake_outcome_id.as_ref(), Some(&wake.outcome_id));
    }
    first.finish().await;
    second.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_profile_mismatch_is_named_and_leaves_enrolled() {
    let mut fault = io();
    fault.profile = Arc::new(|| {
        let mut profile = federation_onboarding_fixture::profile();
        profile.schema_version = "wrong".to_owned();
        Ok(profile)
    });
    let pair = Pair::new(federation_registry::central(), fault).await;
    let report = pair.verify().await.unwrap();
    assert!(
        report
            .outcomes
            .iter()
            .any(|check| check.check == FederationOnboardingCheck::VersionSchema && !check.passed)
    );
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Enrolled
    );
    assert!(
        pair.satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .onboarding
            .probes
            .is_empty()
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_supervisor_failure_records_durable_failed_wake() {
    let mut fault = io();
    fault.platform = Arc::new(|| Err(ApiError::internal("fake spawn failure")));
    let pair = Pair::new(federation_registry::central(), fault).await;
    let report = pair.verify().await.unwrap();
    assert!(!report.termal_complete());
    assert!(
        report
            .outcomes
            .iter()
            .any(|check| check.check == FederationOnboardingCheck::IsolatedWake && !check.passed)
    );
    assert!(
        pair.satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .onboarding
            .probes
            .values()
            .all(|probe| probe.wake.as_ref().unwrap().state == FederationProbeWakeState::Failed)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_external_engram_failure_does_not_prevent_activation() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let central_id = pair.lease.channel.snapshot.local.host_id.clone();
    pair.central
        .mutate_federation(|domain, _| {
            domain.onboarding.engram_enabled.insert(pair.id.clone());
            Ok(())
        })
        .unwrap();
    pair.satellite
        .mutate_federation(|domain, _| {
            domain.onboarding.engram_enabled.insert(central_id.clone());
            domain.onboarding.engram_commands.insert(
                central_id,
                FederationEngramProbeCommand {
                    binary: std::env::current_exe().unwrap(),
                    args: Vec::new(),
                    project_root: std::env::current_dir().unwrap(),
                    json_summary: false,
                },
            );
            Ok(())
        })
        .unwrap();
    let report = pair.verify().await.unwrap();
    assert_eq!(
        report.engram.unwrap().status,
        FederationEngramProbeStatus::Fail
    );
    let expected = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    assert_eq!(expected.membership, FederationMembershipState::Verified);
    pair.central
        .transition_federation_peer(&expected, FederationMembershipState::Active)
        .unwrap();
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_duplicate_probe_wake_cannot_spawn_twice() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = count.clone();
    let mut probe_io = io();
    let normal = probe_io.platform.clone();
    probe_io.platform = Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        normal()
    });
    let pair = Pair::new(federation_registry::central(), probe_io).await;
    let central_root = pair.central.test_temp_root_path().unwrap().to_owned();
    let central_cleaned = pair
        .central
        .test_temp_root
        .as_ref()
        .unwrap()
        .observe_cleanup();
    let satellite_root = pair.satellite.test_temp_root_path().unwrap().to_owned();
    let satellite_cleaned = pair
        .satellite
        .test_temp_root
        .as_ref()
        .unwrap()
        .observe_cleanup();
    let probe: FederationProbeSession = pair
        .rpc
        .request(FederationRpcRoute::Onboarding(
            pair.lease.clone(),
            FederationOnboardingRpc::Prepare(FederationProbeRequest {
                attempt_id: Uuid::new_v4().to_string(),
            }),
        ))
        .await
        .unwrap();
    let request = FederationProbeWakeRequest {
        session_id: probe.session_id,
        attempt_id: probe.attempt_id,
    };
    let first: FederationProbeWakeOutcome = pair
        .rpc
        .request(FederationRpcRoute::Onboarding(
            pair.lease.clone(),
            FederationOnboardingRpc::Wake(request.clone()),
        ))
        .await
        .unwrap();
    let second: FederationProbeWakeOutcome = pair
        .rpc
        .request(FederationRpcRoute::Onboarding(
            pair.lease.clone(),
            FederationOnboardingRpc::Wake(request),
        ))
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    {
        let connection =
            rusqlite::Connection::open(pair.satellite.persistence_path.as_path()).unwrap();
        let image = read_federation_persist_image(&connection).unwrap().unwrap();
        assert!(
            image["domain"]["onboarding"]["probes"]
                .as_object()
                .unwrap()
                .values()
                .any(|probe| probe["wake"]["outcomeId"] == first.outcome_id)
        );
    }
    pair.finish().await;
    phase_sync::receive(&central_cleaned, "duplicate wake central cleanup").unwrap();
    phase_sync::receive(&satellite_cleaned, "duplicate wake satellite cleanup").unwrap();
    for root in [central_root, satellite_root] {
        assert!(!root.exists());
        assert!(!TestTempRoot::owner_marker(&root).exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_stale_probe_results_cannot_attach_to_reenrollment() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let (release_tx, release_rx) = mpsc::channel();
    let release = Mutex::new(release_rx);
    let mut fault = io();
    fault.profile = Arc::new(move || {
        if let Some(sender) = entered.lock().unwrap().take() {
            let _ = sender.send(());
            release
                .lock()
                .unwrap()
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap();
        }
        Ok(federation_onboarding_fixture::profile())
    });
    let pair = Pair::new(federation_registry::central(), fault).await;
    let app = pair.central.clone();
    let id = pair.id.clone();
    let run = tokio::spawn(verify_federation_satellite(app, id));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, entered_rx)
        .await
        .unwrap()
        .unwrap();
    let record = pair.central.inner.lock().unwrap().federation.peers[&pair.id].clone();
    pair.central.enroll_federation_peer(record).unwrap();
    release_tx.send(()).unwrap();
    assert!(run.await.unwrap().is_err());
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Enrolled
    );
    pair.finish().await;
}

#[tokio::test]
async fn federation_onboarding_disabled_has_no_probe_or_membership_effect() {
    let app = federation_registry::domain_state();
    let response = app_router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/federation/peers/{}/verify", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        app.inner
            .lock()
            .unwrap()
            .federation
            .onboarding
            .reports
            .is_empty()
    );
    assert!(app.federation_peer_handles.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_capability_mismatch_is_named_and_cannot_verify() {
    let pair = Pair::unattested(federation_registry::central()).await;
    let mut wrong_key = pair.rpc.channel.clone();
    wrong_key.capability = FederationPairCapability::new("b".repeat(64)).unwrap();
    let (c, s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    let satellite = pair.satellite_channel.clone();
    let satellite_proof = async move {
        let mut s: FederationControlIo = Box::new(s);
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        tokio::io::AsyncReadExt::read_exact(&mut s, &mut preface)
            .await
            .unwrap();
        assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
        federation_attest_satellite(&satellite, &mut s).await
    };
    let (central_proof, satellite_proof) =
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            tokio::join!(
                federation_attest_central(&wrong_key, &mut c),
                satellite_proof
            )
        })
        .await
        .unwrap();
    let no_context = pair.rpc.channel.security_context().is_err()
        && pair.satellite_channel.security_context().is_err();
    let no_rpc = pair.rpc.open_link().await.is_err();
    let report = pair.verify().await.unwrap();
    let membership = pair
        .central
        .federation_peer_snapshot(&pair.id)
        .unwrap()
        .membership;
    let no_sessions = pair.satellite.inner.lock().unwrap().sessions.is_empty();
    pair.finish().await;
    assert!(central_proof.is_err() && satellite_proof.is_err());
    assert!(no_context && no_rpc && no_sessions);
    assert!(report.outcomes.iter().any(|check| check.check
        == FederationOnboardingCheck::CapabilityExchange
        && !check.passed
        && check.reason == "attestationFailed"));
    assert!(
        report
            .outcomes
            .iter()
            .any(|check| check.check == FederationOnboardingCheck::Reachability && !check.passed)
    );
    assert_eq!(membership, FederationMembershipState::Enrolled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_domain_routes_reject_missing_capability_and_user_session_fields() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let response = pair
        .rpc
        .client
        .get(format!(
            "http://{}/federation/v1/onboarding/identity",
            pair.rpc.address
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        serde_json::from_value::<FederationProbeWakeRequest>(json!({
            "attemptId":Uuid::new_v4().to_string(), "sessionId":"user-session", "prompt":"run something",
        }))
        .is_err()
    );
    assert!(pair.satellite.inner.lock().unwrap().sessions.is_empty());
    pair.finish().await;
}
