// Owns immediate availability loss and exact reconnect-basis controls.
// Does not open transports, persist liveness, or reconcile original runtimes.
// New domain coverage mounted under federation_registry.rs.
use super::*;

fn link(state: &AppState, snapshot: &FederationPeerSnapshot) -> String {
    state.inner.lock().unwrap().federation.links[&snapshot.peer.host_id].clone()
}

#[test]
fn federation_disconnect_fences_admission_even_during_a_pending_commit() {
    let state = central();
    let installed = connected(&state, peer(48001));
    let uuid = link(&state, &installed);
    state.verify_federation_peer(&installed, checks()).unwrap();
    let verified = state
        .federation_peer_snapshot(&installed.peer.host_id)
        .unwrap();
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let active = state
        .federation_peer_snapshot(&installed.peer.host_id)
        .unwrap();
    let retained = reserve(&state, &active, "retained", "s1").unwrap();
    assert!(state.federation_reconnect_snapshot(&installed).is_err());
    let before = state.inner.lock().unwrap().federation.clone();
    state.inner.lock().unwrap().federation_durable = false;
    assert!(state.federation_peer_disconnected(&installed, &uuid));
    {
        let mut inner = state.inner.lock().unwrap();
        assert!(!inner.federation_durable);
        assert!(inner.federation.links.is_empty());
        // Availability is deliberately not part of the persisted image.
        assert_eq!(
            serde_json::to_value(&inner.federation).unwrap(),
            serde_json::to_value(&before).unwrap()
        );
        assert!(!inner.federation.peers[&installed.peer.host_id].verified_on(None));
        inner.federation_durable = true;
    }
    assert!(
        state
            .refresh_federation_transport_snapshot(&installed, &uuid)
            .is_err()
    );
    assert!(reserve(&state, &active, "offline", "s2").is_err());
    assert!(
        state
            .ensure_federation_placement_current(&retained)
            .is_err()
    );
    let reconnect = state.federation_reconnect_snapshot(&installed).unwrap();
    assert_eq!(reconnect, active);
    let next = state
        .federation_peer_connected(&reconnect, Uuid::new_v4().to_string())
        .unwrap();
    assert!(next.transport_generation > installed.transport_generation);
    assert_eq!(next.membership, FederationMembershipState::Active);
    assert!(
        state
            .reconcile_federation_placement(&retained, true)
            .is_err()
    );
    assert_eq!(
        state.inner.lock().unwrap().federation.placements["retained"],
        retained
    );
}

#[test]
fn federation_late_disconnect_never_removes_a_successor_even_with_a_reused_uuid() {
    let state = central();
    let original = connected(&state, peer(48001));
    let original_uuid = link(&state, &original);
    assert!(!state.federation_peer_disconnected(&original, &Uuid::new_v4().to_string()));
    let next = state
        .federation_peer_connected(&original, Uuid::new_v4().to_string())
        .unwrap();
    let next_uuid = link(&state, &next);
    assert!(!state.federation_peer_disconnected(&original, &original_uuid));
    assert!(!state.federation_peer_disconnected(&original, &next_uuid));
    let reused = state
        .federation_peer_connected(&next, original_uuid.clone())
        .unwrap();
    assert!(!state.federation_peer_disconnected(&original, &original_uuid));
    assert!(
        state
            .refresh_federation_transport_snapshot(&reused, &original_uuid)
            .is_ok()
    );
    assert!(state.federation_reconnect_snapshot(&original).is_err());
    assert!(state.federation_peer_disconnected(&reused, &original_uuid));
    assert!(!state.federation_peer_disconnected(&reused, &original_uuid));
    assert_eq!(
        state.federation_reconnect_snapshot(&reused).unwrap(),
        reused
    );
}

#[test]
fn federation_reconnect_never_adopts_new_enrollment_or_unresolved_durability() {
    let state = central();
    let record = peer(48001);
    let original = connected(&state, record.clone());
    assert!(state.federation_peer_disconnected(&original, &link(&state, &original)));
    {
        let mut inner = state.inner.lock().unwrap();
        inner.federation_durable = false;
        assert!(
            state
                .federation_reconnect_snapshot_locked(&inner, &original)
                .is_err()
        );
        inner.federation_durable = true;
    }
    assert!(state.federation_reconnect_snapshot(&original).is_ok());
    state.enroll_federation_peer(record).unwrap();
    assert!(state.federation_reconnect_snapshot(&original).is_err());
    let current = state
        .federation_peer_snapshot(&original.peer.host_id)
        .unwrap();
    assert!(state.federation_reconnect_snapshot(&current).is_ok());
    state
        .transition_federation_peer(&current, FederationMembershipState::Revoked)
        .unwrap();
    assert!(state.federation_reconnect_snapshot(&current).is_err());
}
