// Owns isolated domain identity, membership, placement and persistence controls.
// Does not provision hosts or exercise the SSH implementation. New foundation
// tests mounted from tests/mod.rs and using the parent's disposable state fixture.
use super::*;

pub(super) fn domain_state() -> AppState {
    let state = test_app_state();
    {
        let inner = state.inner.lock().unwrap();
        state.persist_internal_locked(&inner).unwrap();
    }
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    bind_loaded_federation(
        &mut state.inner.lock().unwrap(),
        &connection,
        state.persistence_path.as_path(),
    )
    .unwrap();
    state
}

pub(super) fn central() -> AppState {
    let state = domain_state();
    let local = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    state
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Central,
            central: Some(local),
        })
        .unwrap();
    state
}

pub(super) fn peer(port: u16) -> FederationPeerRecord {
    let id = Uuid::new_v4().to_string();
    FederationPeerRecord {
        identity: FederationHostIdentity {
            host_id: id.clone(),
            host_generation: 1,
        },
        enrollment_generation: 1,
        record_generation: 1,
        transport_generation: 1,
        display_name: "Satellite".to_owned(),
        platform: "windows".to_owned(),
        arch: "x86_64".to_owned(),
        endpoint: FederationSshEndpoint {
            ssh_host: format!("satellite-{id}.example"),
            ssh_user: "pilot".to_owned(),
            ssh_port: 22,
            known_hosts_ref: "known-hosts".to_owned(),
            host_key_ref: "pinned-host".to_owned(),
            satellite_federation_port: 47001,
            central_federation_port: port,
            satellite_reverse_forward_port: 47002,
        },
        pair_capability_ref: "pair-capability".to_owned(),
        profile: FederationExecutionProfile {
            protocol_version: 1,
            schema_version: "1".to_owned(),
            binary_sha256: "a".repeat(64),
            toolchain_identity: "test toolchain".to_owned(),
        },
        capacity: FederationCapacity {
            max_sessions: 1,
            build_slots: 1,
            gate_slots: 1,
        },
        membership: FederationMembershipState::Enrolled,
        verification: None,
        last_observed: None,
    }
}

pub(super) fn checks() -> FederationVerificationChecks {
    FederationVerificationChecks {
        host_key: true,
        capability: true,
        protocol: true,
        bidirectional_rpc: true,
        isolated_wake: true,
        platform_supervisor: true,
    }
}

pub(super) fn connected(state: &AppState, record: FederationPeerRecord) -> FederationPeerSnapshot {
    let id = record.identity.host_id.clone();
    state.enroll_federation_peer(record).unwrap();
    let snapshot = state.federation_peer_snapshot(&id).unwrap();
    state
        .federation_peer_connected(&snapshot, Uuid::new_v4().to_string())
        .unwrap()
}

pub(super) fn active(state: &AppState, record: FederationPeerRecord) -> FederationPeerSnapshot {
    let snapshot = connected(state, record);
    state.verify_federation_peer(&snapshot, checks()).unwrap();
    let verified = state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap();
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap()
}

fn reserve(
    state: &AppState,
    snapshot: &FederationPeerSnapshot,
    key: &str,
    id: &str,
) -> Result<FederationPlacement, ApiError> {
    state.reserve_federation_placement(snapshot, key.to_owned(), id.to_owned(), 1, 0)
}

#[test]
fn federation_default_off_has_no_peer_or_local_routing_adoption() {
    let state = domain_state();
    let before = serde_json::to_value(&state.inner.lock().unwrap().preferences.remotes).unwrap();
    assert!(state.federation_peer_snapshots().unwrap().is_empty());
    assert!(state.federation_peer_snapshot("local").is_err());
    assert!(state.enroll_federation_peer(peer(48001)).is_err());
    assert_eq!(
        serde_json::to_value(&state.inner.lock().unwrap().preferences.remotes).unwrap(),
        before
    );
    assert!(!state.inner.lock().unwrap().preferences.federation.enabled);
}

#[test]
fn federation_equal_local_ids_on_distinct_hosts_are_distinct_principals() {
    let state = central();
    let left = active(&state, peer(48001));
    let right = active(&state, peer(48002));
    let a = reserve(&state, &left, "left", "session-1").unwrap();
    let b = reserve(&state, &right, "right", "session-1").unwrap();
    assert_eq!(a.identity.session_id, b.identity.session_id);
    assert_ne!(a.identity.host_id, b.identity.host_id);
    assert_ne!(a.identity.creation_id, b.identity.creation_id);
    assert!(
        state
            .validate_federation_session_assertion(&left, &a.identity)
            .is_ok()
    );
    assert!(
        state
            .validate_federation_session_assertion(&left, &b.identity)
            .is_err()
    );
    let mut spoof = a.identity.clone();
    spoof.host_id = left.local.host_id.clone();
    assert!(
        state
            .validate_federation_session_assertion(&left, &spoof)
            .is_err()
    );
    spoof = a.identity.clone();
    spoof.creation_id = Uuid::new_v4().to_string();
    assert!(
        state
            .validate_federation_session_assertion(&left, &spoof)
            .is_err()
    );
    assert!(state.federation_peer_snapshot("Satellite").is_err());
}

#[test]
fn federation_recreated_local_id_never_inherits_creation_or_placement() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    let old = reserve(&state, &snapshot, "first", "session-1").unwrap();
    state.reconcile_federation_placement(&old, true).unwrap();
    let new = reserve(&state, &snapshot, "second", "session-1").unwrap();
    assert_ne!(old.identity.creation_id, new.identity.creation_id);
    assert!(new.placement_generation > old.placement_generation);
    assert!(state.ensure_federation_placement_current(&old).is_err());
    assert!(
        state
            .validate_federation_session_assertion(&snapshot, &old.identity)
            .is_err()
    );
    assert!(state.ensure_federation_placement_current(&new).is_ok());
}

#[test]
fn federation_verified_is_not_active_and_incomplete_checklist_never_verifies() {
    let state = central();
    let snapshot = connected(&state, peer(48001));
    assert!(reserve(&state, &snapshot, "enrolled", "s1").is_err());
    let mut incomplete = checks();
    incomplete.isolated_wake = false;
    assert!(state.verify_federation_peer(&snapshot, incomplete).is_err());
    assert_eq!(
        state
            .federation_peer_snapshot(&snapshot.peer.host_id)
            .unwrap(),
        snapshot
    );
    state.verify_federation_peer(&snapshot, checks()).unwrap();
    let verified = state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap();
    assert_eq!(verified.membership, FederationMembershipState::Verified);
    assert!(reserve(&state, &verified, "verified", "s1").is_err());
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let active = state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap();
    assert!(reserve(&state, &active, "active", "s1").is_ok());
}

#[test]
fn federation_configuration_change_invalidates_verification_and_old_enrollment() {
    let state = central();
    let original = peer(48001);
    let snapshot = active(&state, original.clone());
    let mut changed = original.clone();
    changed.profile.binary_sha256 = "b".repeat(64);
    state.enroll_federation_peer(changed).unwrap();
    let now = state
        .federation_peer_snapshot(&original.identity.host_id)
        .unwrap();
    assert!(now.enrollment_generation > snapshot.enrollment_generation);
    assert_eq!(now.membership, FederationMembershipState::Enrolled);
    assert_eq!(
        state
            .ensure_federation_peer_current(&snapshot)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert!(reserve(&state, &now, "not-verified", "s1").is_err());
    // Restoring identical configuration bytes does not revive the old fence.
    state.enroll_federation_peer(original).unwrap();
    assert!(state.ensure_federation_peer_current(&snapshot).is_err());
}

#[test]
fn federation_link_aba_fences_old_replies_and_requires_reverification() {
    let state = central();
    let initial = active(&state, peer(48001));
    let old = reserve(&state, &initial, "original", "session-1").unwrap();
    let original_link = old.link_incarnation.clone();
    state
        .federation_peer_connected(&initial, Uuid::new_v4().to_string())
        .unwrap();
    let next = state
        .federation_peer_snapshot(&initial.peer.host_id)
        .unwrap();
    state
        .federation_peer_connected(&next, original_link)
        .unwrap();
    assert!(state.ensure_federation_peer_current(&initial).is_err());
    assert!(state.ensure_federation_placement_current(&old).is_err());
    assert_eq!(state.inner.lock().unwrap().federation.placements.len(), 1);
}

#[test]
fn federation_draining_keeps_exact_reconciliation_but_never_admits_new_work() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    let old = reserve(&state, &snapshot, "original", "s1").unwrap();
    state
        .transition_federation_peer(&snapshot, FederationMembershipState::Draining)
        .unwrap();
    let draining = state
        .federation_peer_snapshot(&snapshot.peer.host_id)
        .unwrap();
    assert!(reserve(&state, &draining, "new", "s2").is_err());
    state.reconcile_federation_placement(&old, false).unwrap();
    let uncertain = state.inner.lock().unwrap().federation.placements["original"].clone();
    assert_eq!(uncertain.state, FederationPlacementState::Uncertain);
    assert_eq!(
        state.federation_registry_view().unwrap()["peers"][0]["allocatedSessions"],
        json!(1)
    );
    state
        .reconcile_federation_placement(&uncertain, true)
        .unwrap();
    assert_eq!(
        state.federation_registry_view().unwrap()["peers"][0]["allocatedSessions"],
        json!(0)
    );
}

#[test]
fn federation_revoke_retains_reservations_and_refuses_every_old_principal() {
    let state = central();
    let snapshot = active(&state, peer(48001));
    let placement = reserve(&state, &snapshot, "key", "s1").unwrap();
    state
        .transition_federation_peer(&snapshot, FederationMembershipState::Revoked)
        .unwrap();
    assert!(state.federation_peer_snapshots().unwrap().is_empty());
    assert!(
        state
            .ensure_federation_placement_current(&placement)
            .is_err()
    );
    assert!(
        state
            .validate_federation_session_assertion(&snapshot, &placement.identity)
            .is_err()
    );
    assert_eq!(
        state.federation_registry_view().unwrap()["peers"][0]["allocatedSessions"],
        json!(1)
    );
}

#[test]
fn federation_last_slot_is_atomic_and_identical_admission_recovers_its_reservation() {
    let state = Arc::new(central());
    let snapshot = active(&state, peer(48001));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for key in ["one", "two"] {
        let state = Arc::clone(&state);
        let snapshot = snapshot.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            reserve(&state, &snapshot, key, key)
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let winner = results.into_iter().find_map(|result| result.ok()).unwrap();
    let duplicate = reserve(
        &state,
        &snapshot,
        &winner.admission_key,
        &winner.identity.session_id,
    )
    .unwrap();
    assert_eq!(duplicate, winner);
    assert!(reserve(&state, &snapshot, &winner.admission_key, "different").is_err());
    assert_eq!(state.inner.lock().unwrap().federation.placements.len(), 1);
}

#[test]
fn federation_registry_has_no_fixed_satellite_roster_and_rejects_channel_collision_atomically() {
    let state = central();
    for port in [48001, 48002, 48003] {
        state.enroll_federation_peer(peer(port)).unwrap();
    }
    assert_eq!(state.federation_peer_snapshots().unwrap().len(), 3);
    let before = state.federation_registry_view().unwrap();
    assert!(state.enroll_federation_peer(peer(48001)).is_err());
    assert_eq!(state.federation_registry_view().unwrap(), before);
    let encoded = serde_json::to_string(&before).unwrap();
    assert!(encoded.contains("pairCapabilityRef"));
    assert!(!encoded.contains("pairSecret"));
    assert!(!encoded.contains("storeBinding"));
}

#[path = "federation_persistence.rs"]
mod persistence;

#[path = "federation_transport.rs"]
mod transport;

#[path = "federation_upgrade.rs"]
mod upgrade;

#[path = "federation_disconnect.rs"]
mod disconnect;

#[path = "federation_roles.rs"]
mod roles;

#[path = "federation_operator.rs"]
mod operator;

#[path = "federation_satellite_operator.rs"]
mod satellite_operator;
