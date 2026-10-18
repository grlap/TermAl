// Owns controlled commit-cut and stale-wake regressions from the O1 review.
// Does not provision hosts or replace L1/M4 testing. New O1 tests reuse the
// existing controlled content-fence writer from federation_connect_writer.rs.
use super::federation_onboarding_fixture::{Pair, io};
use super::federation_onboarding_writer::ControlledWriter;
use super::*;

async fn settle<T: Send + 'static>(
    app: &AppState,
    writer: &mut ControlledWriter,
    mut job: tokio::task::JoinHandle<Result<T, ApiError>>,
) -> Result<T, ApiError> {
    loop {
        tokio::select! {
            result = &mut job => return result.unwrap(),
            fence = writer.fences.recv() => ControlledWriter::commit(app, &fence.unwrap()),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_enrollment_and_profile_share_the_first_commit() {
    let mut app = federation_registry::central();
    let mut record = federation_registry::peer(47451);
    let id = record.identity.host_id.clone();
    app.enroll_federation_peer(record.clone()).unwrap();
    app.mutate_federation(|domain, _| {
        domain.onboarding.engram_enabled.insert(id.clone());
        domain.onboarding.engram_commands.insert(
            id.clone(),
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
    let mut writer = ControlledWriter::attach(&mut app);
    record.display_name = "replacement-disabled-profile".to_owned();
    let request = FederationEnrollmentRequest {
        record,
        engram_enabled: false,
        engram_command: None,
    };
    let job = tokio::spawn(enroll_federation_satellite(
        State(app.clone()),
        AxumPath(id.clone()),
        Json(request),
    ));
    let fence = writer.take().await;
    let coherent = {
        let inner = app.inner.lock().unwrap();
        inner.federation.peers[&id].display_name == "replacement-disabled-profile"
            && !inner.federation.onboarding.engram_enabled.contains(&id)
            && !inner
                .federation
                .onboarding
                .engram_commands
                .contains_key(&id)
    };
    ControlledWriter::commit(&app, &fence);
    let _ = settle(&app, &mut writer, job).await.unwrap();
    let loaded = load_state(app.persistence_path.as_path()).unwrap().unwrap();
    let inner = app.inner.lock().unwrap();
    assert_eq!(loaded.federation.peers[&id], inner.federation.peers[&id]);
    assert_eq!(loaded.federation.onboarding, inner.federation.onboarding);
    drop(inner);
    assert!(
        coherent,
        "first durable candidate must contain the matching enrollment/profile, including disablement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_complete_report_and_proof_share_the_first_commit() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    let mut writer = ControlledWriter::attach(&mut pair.central);
    let job = tokio::spawn(verify_federation_satellite(
        pair.central.clone(),
        pair.id.clone(),
    ));
    let fence = writer.take().await;
    let coherent = {
        let inner = pair.central.inner.lock().unwrap();
        inner
            .federation
            .onboarding
            .reports
            .get(&pair.id)
            .is_some_and(FederationOnboardingReport::termal_complete)
            && inner.federation.peers[&pair.id].membership == FederationMembershipState::Verified
    };
    ControlledWriter::commit(&pair.central, &fence);
    settle(&pair.central, &mut writer, job).await.unwrap();
    let loaded = load_state(pair.central.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let inner = pair.central.inner.lock().unwrap();
    assert_eq!(
        loaded.federation.peers[&pair.id],
        inner.federation.peers[&pair.id]
    );
    assert_eq!(loaded.federation.onboarding, inner.federation.onboarding);
    drop(inner);
    assert!(
        coherent,
        "a complete report and the verification it proves must commit together"
    );
    pair.finish().await;
}

pub(super) fn accepted_probe(pair: &Pair) -> FederationProbeSession {
    let expected = pair.satellite_channel.current_authority().unwrap();
    let probe = pair
        .satellite
        .prepare_federation_probe(
            &expected,
            &pair.lease.incarnation,
            &Uuid::new_v4().to_string(),
        )
        .unwrap();
    let (_, fresh) = pair
        .satellite
        .accept_federation_probe_wake(
            &expected,
            &pair.lease.incarnation,
            &FederationProbeWakeRequest {
                attempt_id: probe.attempt_id.clone(),
                session_id: probe.session_id.clone(),
            },
        )
        .unwrap();
    assert!(fresh);
    probe
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_reconnect_can_verify_without_relaunching_old_accepted_probe() {
    let mut pair = Pair::new(federation_registry::central(), io()).await;
    let old = accepted_probe(&pair);
    pair.lease.channel.invalidate();
    pair.satellite_channel.invalidate();
    pair.lease = pair.rpc.open_link().await.unwrap();
    let report = pair.verify().await.unwrap();
    assert!(
        report.termal_complete(),
        "new explicit probe must be possible after the old link is fenced"
    );
    assert_ne!(report.probe_session_id.as_ref(), Some(&old.session_id));
    let delayed = pair
        .rpc
        .request::<FederationProbeWakeOutcome>(FederationRpcRoute::Onboarding(
            pair.lease.clone(),
            FederationOnboardingRpc::Wake(FederationProbeWakeRequest {
                attempt_id: old.attempt_id,
                session_id: old.session_id,
            }),
        ))
        .await;
    assert!(
        delayed.is_err(),
        "the old wake identity must never relaunch"
    );
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_durable_reload_can_retire_accepted_probe_under_a_new_link() {
    let pair = Pair::new(federation_registry::central(), io()).await;
    let old = accepted_probe(&pair);
    let loaded = load_state(pair.satellite.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.federation.links.is_empty());
    *pair.satellite.inner.lock().unwrap() = loaded;
    let expected = pair
        .satellite
        .federation_peer_snapshot(&old.expected.peer.host_id)
        .unwrap();
    let incarnation = Uuid::new_v4().to_string();
    let expected = pair
        .satellite
        .federation_peer_connected(&expected, incarnation.clone())
        .unwrap();
    let fresh = pair.satellite.prepare_federation_probe(
        &expected,
        &incarnation,
        &Uuid::new_v4().to_string(),
    );
    assert!(
        fresh.is_ok(),
        "a persisted Accepted outcome must not wedge every later enrollment/link"
    );
    assert_ne!(fresh.unwrap().session_id, old.session_id);
    pair.finish().await;
}
