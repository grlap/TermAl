// Ordinary mailbox routing follows the enrolled authenticated pair. Diagnostic
// reports and execution placement remain separate from native mail and wakes.
use super::*;
use super::native_addressing::{address, ordinary, receive_and_ack};
use super::super::federation_onboarding_fixture::{Pair, io, profile};

fn busy_recipient(state: &AppState, runtime_id: &str)
    -> (String, std::sync::mpsc::Receiver<ClaudeRuntimeCommand>)
{
    let native = test_session_id(state, Agent::Claude);
    let (runtime, input) = test_claude_runtime_handle(runtime_id);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&native).unwrap();
    inner.sessions[index].session.status = SessionStatus::Active;
    inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    (native, input)
}

async fn native_exchange(pair: &Pair, central: &str, satellite: &str, key: &str)
    -> (MailboxAppendReceipt, MailboxAppendReceipt)
{
    let forward = ordinary(&pair.central, central, &address(&pair.satellite, satellite),
        &format!("{key}-forward")).await.unwrap();
    // First-recipient home recovery uses the original pair and principal.
    receive_and_ack(&pair.satellite, satellite, &forward).await.unwrap();
    let reverse = ordinary(&pair.satellite, satellite, &address(&pair.central, central),
        &format!("{key}-reverse")).await.unwrap();
    receive_and_ack(&pair.central, central, &reverse).await.unwrap();
    assert_eq!(forward.mailbox_id, reverse.mailbox_id);
    pair.central.run_mailbox_relay_wake_batch(0, 0).await;
    (forward, reverse)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrolled_pair_routes_native_mail_and_wakes_without_verification_or_activation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = Pair::with_enrolled_profile(federation_mailboxes::central_store(), io(), io(), profile()).await;
    let (central, central_input) = busy_recipient(&pair.central, "enrolled-central");
    let (satellite, satellite_input) = busy_recipient(&pair.satellite, "enrolled-satellite");
    let (forward, reverse) = native_exchange(&pair, &central, &satellite, "enrolled").await;
    for (state, native, runtime, input, sent) in [
        (&pair.central, &central, "enrolled-central", &central_input, &reverse),
        (&pair.satellite, &satellite, "enrolled-satellite", &satellite_input, &forward),
    ] {
        state.finish_turn_ok_if_runtime_matches(native, &RuntimeToken::Claude(runtime.into())).unwrap();
        let ClaudeRuntimeCommand::Prompt(command) = input.try_recv().expect("ordinary native mail wakes either host") else {
            panic!("expected mailbox prompt");
        };
        assert!(command.text.contains(&sent.mailbox_id));
        assert!(!command.text.contains("enrolled-forward"));
    }
    let sender = native(&pair.central, "Directory reader");
    let rows = directory(&pair.central, &sender).await;
    assert!(rows.iter().any(|row| row.binding.identity.session_id == satellite
        && row.binding.identity.host_id == pair.id));
    let snapshot = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    assert!(pair.central.reserve_federation_placement(&snapshot, "no-execution".into(),
        "session-never-placed".into(), 0, 0).is_err());
    {
        let inner = pair.central.inner.lock().unwrap();
        let peer = &inner.federation.peers[&pair.id];
        assert_eq!(peer.membership, FederationMembershipState::Enrolled);
        assert!(peer.verification.is_none());
        assert!(!inner.federation.onboarding.reports.contains_key(&pair.id));
        assert!(inner.federation.placements.values().all(|placement| placement.mailbox_only
            && !federation_placement_authority(&inner.federation, &inner.preferences.federation, placement)));
    }
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_keeps_native_and_domain_identity_without_running_diagnostic_probes() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let probes = Arc::new(AtomicU64::new(0));
    let mut measured = io();
    let observed = probes.clone();
    measured.profile = Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Err(ApiError::internal("diagnostic profile unavailable after rebuild"))
    });
    let mut pair = Pair::with_enrolled_profile(federation_mailboxes::central_store(),
        measured.clone(), measured.clone(), profile()).await;
    let central = native(&pair.central, "Central");
    let satellite = native(&pair.satellite, "Satellite");
    let local = native(&pair.central, "Bare local control");
    directory(&pair.central, &central).await;
    let binding = pair.satellite.federation_mailbox_actor(&satellite).unwrap().unwrap().binding;
    let reference = pair.central.capture_federation_reference(&central, &binding.participant_key()).unwrap();
    reference.proof(&pair.central).await.unwrap();
    let creation = {
        let inner = pair.satellite.inner.lock().unwrap();
        federation_mailbox_root_locked(&inner, &satellite).unwrap().federation_native_creation_id.clone()
    };
    let placements = pair.central.inner.lock().unwrap().federation.placements.clone();
    let (first, _) = native_exchange(&pair, &central, &satellite, "before-restart").await;
    let old = pair.lease.clone();
    pair.reconnect().await;
    pair.rpc.health(&pair.lease).await.unwrap();
    assert_ne!(old.incarnation, pair.lease.incarnation);
    assert!(old.with_incarnation(|| Ok(())).is_err());
    assert!(reference.with_current(&pair.central, |_| Ok(())).is_err(),
        "an old reference context cannot cross into the replacement lease");
    directory(&pair.central, &central).await;
    let refreshed = pair.central.capture_federation_reference(&central, &binding.participant_key()).unwrap();
    refreshed.proof(&pair.central).await.unwrap();
    assert_eq!(refreshed.placement, reference.placement);
    let (next, _) = native_exchange(&pair, &central, &satellite, "after-restart").await;
    assert_eq!(next.mailbox_id, first.mailbox_id);
    let duplicate = ordinary(&pair.central, &central, &address(&pair.satellite, &satellite),
        "before-restart-forward").await.unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, first.message_id);
    let control = ordinary(&pair.central, &central, &local, "bare-local").await.unwrap();
    receive_and_ack(&pair.central, &local, &control).await.unwrap();
    assert!(!control.mailbox_id.starts_with("relay-"));
    assert_eq!(pair.satellite.federation_mailbox_actor(&satellite).unwrap().unwrap().binding, binding);
    {
        let inner = pair.satellite.inner.lock().unwrap();
        assert_eq!(federation_mailbox_root_locked(&inner, &satellite).unwrap().federation_native_creation_id, creation);
    }
    assert_eq!(pair.central.inner.lock().unwrap().federation.placements, placements);
    let (central_state, satellite_state, listeners) = pair.suspend().await;
    for state in [&central_state, &satellite_state] {
        *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    }
    let pair = Pair::resume(central_state, satellite_state, listeners, measured.clone(), measured).await;
    pair.rpc.health(&pair.lease).await.unwrap();
    directory(&pair.satellite, &satellite).await;
    let (restored, _) = native_exchange(&pair, &central, &satellite, "after-both-hosts-reload").await;
    assert_eq!(restored.mailbox_id, first.mailbox_id);
    assert_eq!(pair.satellite.federation_mailbox_actor(&satellite).unwrap().unwrap().binding, binding);
    assert_eq!(probes.load(Ordering::SeqCst), 0, "mail and health must not invoke requalification");
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostic_failure_preserves_link_but_identity_failure_retires_it() {
    let pair = Pair::with_enrolled_profile(federation_mailboxes::central_store(), io(), io(), profile()).await;
    assert!(pair.rpc.retain_health_refusal(&pair.lease, &ApiError::internal("diagnostic unavailable")));
    pair.rpc.health(&pair.lease).await.unwrap();
    assert!(pair.rpc.retain_health_refusal(&pair.lease, &ApiError::conflict("signed diagnostic refusal")));
    let mut health: FederationHealth = pair.rpc.request(FederationRpcRoute::Health(pair.lease.clone())).await.unwrap();
    health.host_generation += 1;
    assert!(pair.rpc.validate_health(&pair.lease, health).is_err());
    assert!(pair.lease.with_incarnation(|| Ok(())).is_err());
    assert!(!pair.rpc.retain_health_refusal(&pair.lease, &ApiError::internal("cannot revive retired link")));
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_mail_authority_still_refuses_material_changes_and_continuity_loss() {
    let pair = Pair::with_enrolled_profile(federation_mailboxes::central_store(), io(), io(), profile()).await;
    let sender = native(&pair.central, "Caller");
    let recipient = native(&pair.satellite, "Recipient");
    directory(&pair.central, &sender).await;
    let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    let inner = pair.central.inner.lock().unwrap();
    let domain = inner.federation.clone();
    let settings = inner.preferences.federation.clone();
    let placement = domain.placements.values().find(|placement| placement.identity.session_id == recipient).unwrap().clone();
    drop(inner);
    for change in ["enrollment", "key", "endpoint", "identity", "revocation", "quarantine", "link"] {
        let mut changed = domain.clone();
        let peer = changed.peers.get_mut(&pair.id).unwrap();
        match change {
            "enrollment" => peer.enrollment_generation += 1,
            "key" => peer.pair_capability_ref.push_str("-rotated"),
            "endpoint" => peer.endpoint.ssh_host.push_str(".changed"),
            "identity" => peer.identity.host_generation += 1,
            "revocation" => peer.membership = FederationMembershipState::Revoked,
            "quarantine" => changed.admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore),
            "link" => { changed.links.remove(&pair.id); },
            _ => unreachable!(),
        }
        assert!(!federation_mailbox_placement_authority(&changed, &settings, &placement), "{change}");
        assert!(federation_participation_qualified(&changed, &settings, &current, &pair.lease.incarnation).is_err(), "{change}");
    }
    pair.central.inner.lock().unwrap().federation_continuity_lost = true;
    assert!(ordinary(&pair.central, &sender, &address(&pair.satellite, &recipient), "continuity-refusal").await.is_err());
    pair.central.inner.lock().unwrap().federation_continuity_lost = false;
    pair.finish().await;
}
