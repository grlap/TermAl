// Phase-controlled reconnect races and retained report evidence. Uses the
// authenticated loopback pair, never a live host or provider.
use super::federation_onboarding_fixture::{Pair, io};
use super::federation_build_compatibility::federation_requalification_refusal_status;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_reverse_route_preserves_authority_refusals() {
    for (mode, status) in [
        (1, StatusCode::CONFLICT), // Central protocol mismatch, rejected by satellite.
        (2, StatusCode::CONFLICT), // Central schema mismatch, rejected by satellite.
        (401, StatusCode::CONFLICT),
        (403, StatusCode::CONFLICT),
        (409, StatusCode::CONFLICT),
        (423, StatusCode::LOCKED),
        (429, StatusCode::LOCKED),
        (503, StatusCode::LOCKED),
    ] {
        let selected = Arc::new(AtomicU64::new(0));
        let observed = selected.clone();
        let mut central_io = io();
        let profile = central_io.profile.clone();
        central_io.profile = Arc::new(move || {
            let mode = observed.load(Ordering::SeqCst);
            let mut profile = profile()?;
            match mode {
                0 => {}
                1 => profile.protocol_version += 1,
                2 => profile.schema_version.push_str("-incompatible"),
                code => {
                    return Err(ApiError::from_status(
                        StatusCode::from_u16(code as u16).unwrap(),
                        "controlled identity refusal",
                    ));
                }
            }
            Ok(profile)
        });
        let mut pair =
            Pair::with_central_io(federation_registry::central(), io(), central_io).await;
        assert!(pair.verify().await.unwrap().termal_complete());
        let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .transition_federation_peer(&peer, FederationMembershipState::Active)
            .unwrap();
        pair.reconnect().await;
        selected.store(mode, Ordering::SeqCst);
        // This traverses satellite Reverse -> central Identity and the actual
        // reverse-route profile checks, including signed error responses.
        let error = pair.rpc.requalify(&pair.lease).await.unwrap_err();
        assert_eq!(error.status, status, "central mode {mode}");
        {
            let inner = pair.central.inner.lock().unwrap();
            let report = &inner.federation.onboarding.reports[&pair.id];
            assert!(report.outcomes.iter().any(|outcome| {
                outcome.check == FederationOnboardingCheck::ReverseRpc
                    && !outcome.passed
                    && outcome.reason
                        == if status == StatusCode::CONFLICT {
                            "reverseAuthorityRefused"
                        } else {
                            "reverseRpcFailed"
                        }
            }));
            let peer = &inner.federation.peers[&pair.id];
            assert_eq!(peer.membership, FederationMembershipState::Active);
            assert!(peer.requalification.is_some());
            assert!(!peer.verified_on(Some(&pair.lease.incarnation)));
        }
        if status == StatusCode::LOCKED {
            selected.store(0, Ordering::SeqCst);
            pair.rpc.requalify(&pair.lease).await.unwrap();
            assert_eq!(
                pair.central
                    .federation_peer_snapshot(&pair.id)
                    .unwrap()
                    .membership,
                FederationMembershipState::Active
            );
        }
        pair.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_mid_verify_settling_preserves_same_lease() {
    for phase in [
        "requalificationPrechecked",
        "requalificationFinalize",
        "verificationChecked",
        "incompleteVerificationChecked",
    ] {
        let fail_identity = Arc::new(AtomicBool::new(false));
        let failing = fail_identity.clone();
        let mut measured = io();
        let profile = measured.profile.clone();
        measured.profile = Arc::new(move || {
            if failing.load(Ordering::SeqCst) {
                Err(federation_transport_unavailable())
            } else {
                profile()
            }
        });
        let mut pair = Pair::new(federation_registry::central(), measured).await;
        assert!(pair.verify().await.unwrap().termal_complete());
        let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .transition_federation_peer(&peer, FederationMembershipState::Active)
            .unwrap();
        pair.reconnect().await;
        let before = pair.central.inner.lock().unwrap().federation.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let observed = fired.clone();
        let app = pair.central.clone();
        pair.rpc.channel.control.observed = Arc::new(move |event, _| {
            if event == phase && !observed.swap(true, Ordering::SeqCst) {
                app.inner.lock().unwrap().federation_durable = false;
            }
        });
        pair.central.federation_peer_handles.lock().unwrap().insert(
            pair.id.clone(),
            Arc::new(FederationPeerHandle {
                channel: pair.rpc.channel.clone(),
                rpc: pair.rpc.clone(),
            }),
        );
        fail_identity.store(phase == "incompleteVerificationChecked", Ordering::SeqCst);
        let error = pair.rpc.requalify(&pair.lease).await.unwrap_err();
        assert!(fired.load(Ordering::SeqCst), "phase not reached: {phase}");
        assert_eq!(error.status, StatusCode::LOCKED, "{phase}");
        assert!(pair.lease.with_incarnation(|| Ok(())).is_ok());
        {
            let inner = pair.central.inner.lock().unwrap();
            assert_eq!(inner.federation.version, before.version, "{phase}");
            assert_eq!(inner.federation.peers[&pair.id], before.peers[&pair.id]);
            assert_eq!(
                inner.federation.onboarding.reports,
                before.onboarding.reports
            );
            assert_eq!(inner.federation.links, before.links);
        }
        // Release only the injected hold. Reuse the same lease and retained
        // proof; no reconnect, epoch change or operator verification occurs.
        pair.central.inner.lock().unwrap().federation_durable = true;
        fail_identity.store(false, Ordering::SeqCst);
        pair.rpc.requalify(&pair.lease).await.unwrap();
        assert_eq!(
            pair.central
                .federation_peer_snapshot(&pair.id)
                .unwrap()
                .membership,
            FederationMembershipState::Active
        );
        assert!(pair.lease.with_incarnation(|| Ok(())).is_ok());
        pair.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_settling_keeps_continuity_loss_fatal() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    {
        let mut inner = pair.central.inner.lock().unwrap();
        inner.federation_durable = false;
        inner.federation_continuity_lost = true;
    }
    assert_eq!(
        pair.central
            .ensure_federation_peer_current(&peer)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        pair.central
            .mutate_federation(|_, _| Ok(()))
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    {
        let mut inner = pair.central.inner.lock().unwrap();
        inner.federation_durable = true;
        inner.federation_continuity_lost = false;
    }
    let mut stale = peer;
    stale.enrollment_generation += 1;
    assert_eq!(
        pair.central
            .ensure_federation_peer_current(&stale)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_retained_probe_rechecks_platform_and_identity() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    let initial = pair.verify().await.unwrap();
    let probe = FederationQualifiedProbe::from_report(&initial).unwrap();
    pair.reconnect().await;
    for mismatch in ["platform", "arch", "identity"] {
        let mut report = initial.clone();
        report.expected = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        report.link_incarnation = pair.lease.incarnation.clone();
        let mut record = pair.central.inner.lock().unwrap().federation.peers[&pair.id].clone();
        match mismatch {
            "platform" => record.platform = "different-os".to_owned(),
            "arch" => record.arch = "different-arch".to_owned(),
            _ => report.expected.peer.host_generation += 1,
        }
        run_federation_onboarding_checks(
            &pair.rpc,
            &pair.lease,
            &record,
            false,
            &mut report,
            Some(&probe),
        )
        .await;
        assert!(!report.termal_complete(), "{mismatch}");
        assert_eq!(
            federation_requalification_refusal_status(&report),
            StatusCode::CONFLICT
        );
        let check = if mismatch == "identity" {
            FederationOnboardingCheck::QualifiedIdentity
        } else {
            FederationOnboardingCheck::PlatformPathsSupervisor
        };
        assert!(
            report
                .outcomes
                .iter()
                .any(|outcome| outcome.check == check && !outcome.passed)
        );
    }
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_reconnect_preserves_separate_engram_evidence() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
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
    let initial = pair.verify().await.unwrap();
    assert!(initial.termal_complete());
    assert_eq!(
        initial.engram.as_ref().unwrap().status,
        FederationEngramProbeStatus::Fail
    );
    pair.reconnect().await;
    pair.rpc.health(&pair.lease).await.unwrap();
    let saved = pair
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .onboarding
        .reports[&pair.id]
        .clone();
    assert!(saved.termal_complete());
    assert_eq!(saved.engram, initial.engram);
    pair.finish().await;
}
