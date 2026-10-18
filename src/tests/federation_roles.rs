// Owns populated-registry role and central-binding transition controls.
// Does not migrate sessions or revoke peers implicitly. New regression coverage
// mounted below federation_registry.rs and using its disposable domain fixture.
use super::*;

#[test]
fn federation_satellite_role_refuses_incompatible_live_peers_without_mutating_the_domain() {
    let state = central();
    let central_peer = peer(48001);
    let other_peer = peer(48002);
    state.enroll_federation_peer(central_peer.clone()).unwrap();
    state.enroll_federation_peer(other_peer.clone()).unwrap();
    let before = PersistedState::metadata_from_inner(&state.inner.lock().unwrap());
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let stored_before = read_federation_persist_image(&connection).unwrap();
    let satellite = FederationSettings {
        enabled: true,
        role: FederationRole::Satellite,
        central: Some(central_peer.identity.clone()),
    };
    assert_eq!(
        state
            .configure_federation(satellite.clone())
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        federation_persist_image(&PersistedState::metadata_from_inner(
            &state.inner.lock().unwrap()
        )),
        federation_persist_image(&before)
    );
    assert_eq!(
        read_federation_persist_image(&connection).unwrap(),
        stored_before
    );
    assert!(state.inner.lock().unwrap().federation_durable);
    assert_eq!(state.federation_peer_snapshots().unwrap().len(), 2);
    let other = state
        .federation_peer_snapshot(&other_peer.identity.host_id)
        .unwrap();
    state
        .transition_federation_peer(&other, FederationMembershipState::Revoked)
        .unwrap();
    state.configure_federation(satellite).unwrap();
    let snapshots = state.federation_peer_snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].local_role, FederationRole::Satellite);
    assert_eq!(snapshots[0].peer, central_peer.identity);
    assert_eq!(snapshots[0].central, central_peer.identity);
    assert_eq!(
        state.inner.lock().unwrap().federation.peers[&other_peer.identity.host_id].membership,
        FederationMembershipState::Revoked
    );
    assert!(state.enroll_federation_peer(other_peer).is_err());
    let connected = state
        .federation_peer_connected(&snapshots[0], Uuid::new_v4().to_string())
        .unwrap();
    state.verify_federation_peer(&connected, checks()).unwrap();
    assert_eq!(state.federation_peer_snapshots().unwrap().len(), 1);
}

#[test]
fn federation_central_identity_rebinding_requires_explicit_revocation_including_epoch_changes() {
    for same_host in [false, true] {
        let state = central();
        let original = peer(48001);
        state
            .configure_federation(FederationSettings {
                enabled: true,
                role: FederationRole::Satellite,
                central: Some(original.identity.clone()),
            })
            .unwrap();
        state.enroll_federation_peer(original.clone()).unwrap();
        let snapshot = state
            .federation_peer_snapshot(&original.identity.host_id)
            .unwrap();
        let mut replacement = if same_host {
            original.clone()
        } else {
            peer(48002)
        };
        if same_host {
            replacement.identity.host_generation += 1;
        }
        let changed = FederationSettings {
            enabled: true,
            role: FederationRole::Satellite,
            central: Some(replacement.identity.clone()),
        };
        let before = federation_persist_image(&PersistedState::metadata_from_inner(
            &state.inner.lock().unwrap(),
        ));
        assert!(state.configure_federation(changed.clone()).is_err());
        assert_eq!(
            federation_persist_image(&PersistedState::metadata_from_inner(
                &state.inner.lock().unwrap()
            )),
            before
        );
        assert_eq!(
            state.federation_peer_snapshots().unwrap(),
            vec![snapshot.clone()]
        );
        state
            .transition_federation_peer(&snapshot, FederationMembershipState::Revoked)
            .unwrap();
        state.configure_federation(changed).unwrap();
        assert!(state.federation_peer_snapshots().unwrap().is_empty());
        assert!(state.enroll_federation_peer(original).is_err());
        state.enroll_federation_peer(replacement.clone()).unwrap();
        let snapshots = state.federation_peer_snapshots().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].central, replacement.identity);
        assert_eq!(snapshots[0].peer, replacement.identity);
        assert!(state.ensure_federation_peer_current(&snapshot).is_err());
    }
}
