// Availability regressions for ordinary reconnects. Initial probe ownership and
// old-epoch wake handling stay covered by the onboarding recovery controls.
use super::federation_onboarding_fixture::{Pair, io};
use super::*;

fn missing_compiler(program: &str, _: &[&str], _: &FsPath) -> Result<String, ApiError> {
    if program == "rustc" {
        Err(ApiError::from_status(
            StatusCode::SERVICE_UNAVAILABLE,
            "compiler unavailable",
        ))
    } else {
        Ok("TERMAL_PROBE_WAKE".to_owned())
    }
}

fn without_compiler() -> FederationOnboardingIo {
    let mut measured = io();
    measured.profile = Arc::new(|| {
        federation_execution_profile_with_probe(
            &std::env::current_dir().unwrap(),
            &FederationProcessImage::capture(None),
            missing_compiler,
        )
    });
    measured.platform = Arc::new(|| {
        federation_platform_probe_with(&std::env::current_dir().unwrap(), missing_compiler)
    });
    measured.image = Arc::new(|| FederationImageObservation::Unknown);
    measured
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_missing_compiler_on_either_host_is_diagnostic() {
    for central in [false, true] {
        let measured = without_compiler();
        assert_eq!(
            (measured.profile)().unwrap().toolchain_identity,
            "unavailable"
        );
        let platform = (measured.platform)().unwrap();
        assert_eq!(platform.toolchain_identity, "unavailable");
        assert!(platform.child_spawned_and_reaped && platform.paths_resolved);
        let mut pair = Pair::with_central_io(
            federation_registry::central(),
            if central { io() } else { measured.clone() },
            if central { measured } else { io() },
        )
        .await;
        assert!(pair.verify().await.unwrap().termal_complete());
        let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .transition_federation_peer(&peer, FederationMembershipState::Active)
            .unwrap();
        pair.reconnect().await;
        pair.rpc.health(&pair.lease).await.unwrap();
        assert_eq!(
            pair.central
                .federation_peer_snapshot(&pair.id)
                .unwrap()
                .membership,
            FederationMembershipState::Active
        );
        pair.finish().await;
    }
    // The shell spawn/reap check remains mandatory during initial onboarding.
    assert!(
        federation_platform_probe_with(&std::env::current_dir().unwrap(), |_, _, _| {
            Err(ApiError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "shell unavailable",
            ))
        })
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_durability_hold_preserves_lease_and_recovery() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Active)
        .unwrap();
    pair.reconnect().await;
    pair.central.inner.lock().unwrap().federation_durable = false;
    let held = pair.rpc.requalify(&pair.lease).await.unwrap_err();
    assert_eq!(held.status, StatusCode::LOCKED);
    assert_eq!(
        pair.lease
            .with_current(&pair.central, |_, _| Ok(()))
            .unwrap_err()
            .status,
        StatusCode::LOCKED
    );
    assert!(pair.lease.with_incarnation(|| Ok(())).is_ok());
    pair.central.inner.lock().unwrap().federation_durable = true;
    pair.rpc.health(&pair.lease).await.unwrap();
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Active
    );
    // Permanent continuity loss is still a stale-authority refusal, not settling.
    pair.central
        .inner
        .lock()
        .unwrap()
        .federation_continuity_lost = true;
    assert_eq!(
        pair.lease
            .with_current(&pair.central, |_, _| Ok(()))
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    pair.central
        .inner
        .lock()
        .unwrap()
        .federation_continuity_lost = false;
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_reconnect_does_not_spend_probe_archive() {
    let calls = Arc::new(AtomicU64::new(0));
    let observed = calls.clone();
    let mut measured = io();
    let platform = measured.platform.clone();
    measured.platform = Arc::new(move || {
        assert_eq!(
            observed.fetch_add(1, Ordering::SeqCst),
            0,
            "ordinary reconnect must not launch another isolated child"
        );
        platform()
    });
    let mut pair = Pair::new(federation_registry::central(), measured).await;
    let initial = pair.verify().await.unwrap();
    assert!(initial.termal_complete());
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Active)
        .unwrap();
    let satellite_peer = pair
        .satellite
        .federation_peer_snapshots()
        .unwrap()
        .remove(0)
        .peer
        .host_id;
    pair.satellite
        .mutate_federation(|domain, _| {
            let template = domain.onboarding.probes[&satellite_peer].clone();
            let mut rows = Vec::new();
            for _ in 0..FEDERATION_RETIRED_PROBES_PER_PEER {
                let mut probe = template.clone();
                probe.attempt_id = Uuid::new_v4().to_string();
                probe.session_id = Uuid::new_v4().to_string();
                let wake = probe.wake.as_mut().unwrap();
                wake.session_id = probe.session_id.clone();
                wake.outcome_id = Uuid::new_v4().to_string();
                wake.state = FederationProbeWakeState::Unknown;
                rows.push(FederationRetiredProbe {
                    probe,
                    retired_at: chrono::Utc::now().to_rfc3339(),
                    replay_observations: 0,
                    last_completion: None,
                });
            }
            domain
                .onboarding
                .retired_probes
                .insert(satellite_peer.clone(), rows);
            Ok(())
        })
        .unwrap();
    let archive = pair
        .satellite
        .inner
        .lock()
        .unwrap()
        .federation
        .onboarding
        .retired_probes
        .clone();
    for cycle in 0..(FEDERATION_RETIRED_PROBES_PER_PEER + 2) {
        pair.reconnect().await;
        if cycle == 0 {
            // Retained qualification survives another disconnect and store reload.
            *pair.central.inner.lock().unwrap() =
                load_state(pair.central.persistence_path.as_path())
                    .unwrap()
                    .unwrap();
            pair.reconnect().await;
        }
        pair.rpc.health(&pair.lease).await.unwrap();
        let inner = pair.central.inner.lock().unwrap();
        let report = &inner.federation.onboarding.reports[&pair.id];
        assert!(report.termal_complete());
        assert_eq!(report.probe_session_id, initial.probe_session_id);
        assert_eq!(report.attempt_id, initial.attempt_id,
            "ordinary reconnect retains the original diagnostic report");
        assert_eq!(
            inner.federation.peers[&pair.id].membership,
            FederationMembershipState::Active
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        pair.satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .onboarding
            .retired_probes,
        archive
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_preserves_membership_while_quarantine_refuses_admission() {
    for membership in [
        FederationMembershipState::Verified,
        FederationMembershipState::Draining,
    ] {
        let mut pair = Pair::new(federation_registry::central(), io()).await;
        assert!(pair.verify().await.unwrap().termal_complete());
        if membership == FederationMembershipState::Draining {
            let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
            pair.central
                .transition_federation_peer(&peer, FederationMembershipState::Active)
                .unwrap();
            let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
            pair.central
                .transition_federation_peer(&peer, membership)
                .unwrap();
        }
        pair.reconnect().await;
        pair.rpc.health(&pair.lease).await.unwrap();
        assert_eq!(
            pair.central
                .federation_peer_snapshot(&pair.id)
                .unwrap()
                .membership,
            membership
        );
        pair.reconnect().await;
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore);
        pair.rpc.health(&pair.lease).await.unwrap();
        assert!(pair.lease.with_incarnation(|| Ok(())).is_ok(),
            "admission quarantine does not erase the authenticated transport");
        assert!(pair.central.ensure_federation_admission_allowed().is_err());
        pair.lease.with_current(&pair.central, |inner, current| {
            assert!(federation_participation_qualified(&inner.federation,
                &inner.preferences.federation, current, &pair.lease.incarnation).is_err(),
                "ordinary routing must still refuse quarantine");
            Ok(())
        }).unwrap();
        assert_eq!(pair.rpc.requalify(&pair.lease).await.unwrap_err().status, StatusCode::LOCKED);
        assert!(
            !pair.central.inner.lock().unwrap().federation.peers[&pair.id]
                .verified_on(Some(&pair.lease.incarnation))
        );
        pair.central
            .inner
            .lock()
            .unwrap()
            .federation
            .admission_quarantine = None;
        pair.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_reverse_image_text_is_only_validated_evidence() {
    let mut central_io = io();
    central_io.image = Arc::new(|| {
        let FederationImageObservation::Known(mut image) = federation_onboarding_fixture::image()
        else {
            unreachable!()
        };
        image.path_at_start = "invalid\npeer text".to_owned();
        FederationImageObservation::Known(image)
    });
    let pair = Pair::with_central_io(federation_registry::central(), io(), central_io).await;
    let report = pair.verify().await.unwrap();
    assert!(report.termal_complete());
    assert_eq!(
        report.reverse_running_image,
        FederationImageObservation::Unknown
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_build_compatibility_migration_excludes_other_epoch_basis_and_completed() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .transition_federation_peer(&peer, FederationMembershipState::Active)
        .unwrap();
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central
        .reserve_federation_placement(
            &peer,
            "current".to_owned(),
            test_session_id(&pair.satellite, Agent::Codex),
            0,
            0,
        )
        .unwrap();
    // Inspect the migration helper's exclusion boundary without manufacturing
    // a verified runtime state or making these intentionally stale rows active.
    let mut domain = pair.central.inner.lock().unwrap().federation.clone();
    for key in ["old-epoch", "other-basis", "completed"] {
        let mut row = domain.placements["current"].clone();
        row.admission_key = key.to_owned();
        row.identity.creation_id = Uuid::new_v4().to_string();
        domain.placements.insert(key.to_owned(), row);
    }
    domain
        .placements
        .get_mut("old-epoch")
        .unwrap()
        .peer
        .transport_generation += 1;
    domain
        .placements
        .get_mut("other-basis")
        .unwrap()
        .execution_basis = "f".repeat(64);
    domain.placements.get_mut("completed").unwrap().state = FederationPlacementState::Completed;
    let before = domain.placements.clone();
    pair.reconnect().await;
    let pending = pair.central.inner.lock().unwrap().federation.clone();
    domain.peers = pending.peers;
    domain.links = pending.links;
    let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    federation_requalification_finish(&mut domain, &pair.id, &current).unwrap();
    for key in ["old-epoch", "other-basis", "completed"] {
        assert_eq!(
            domain.placements[key], before[key],
            "must not migrate {key}"
        );
    }
    assert_ne!(
        domain.placements["current"].peer.transport_generation,
        before["current"].peer.transport_generation
    );
    assert_eq!(
        domain.placements["current"].identity,
        before["current"].identity
    );
    pair.finish().await;
}
