// Owns transport-lifetime versus authority-revision regression controls.
// Does not open channels or grant admission; uses the real domain mutation APIs.
// Split from federation_registry.rs to keep the new transport seam focused.
use super::*;

fn incarnation(state: &AppState, snapshot: &FederationPeerSnapshot) -> String {
    state.inner.lock().unwrap().federation.links[&snapshot.peer.host_id].clone()
}

#[test]
fn federation_transport_survives_membership_bookkeeping_without_weakening_authority() {
    let state = central();
    let channel = connected(&state, peer(48001));
    let link = incarnation(&state, &channel);
    state.verify_federation_peer(&channel, checks()).unwrap();
    let verified = state
        .refresh_federation_transport_snapshot(&channel, &link)
        .unwrap();
    assert_eq!(verified.membership, FederationMembershipState::Verified);
    assert!(verified.record_generation > channel.record_generation);
    assert_eq!(verified.transport_generation, channel.transport_generation);
    assert!(channel.same_transport_as(&verified));
    assert!(state.ensure_federation_peer_current(&channel).is_err());
    assert!(reserve(&state, &verified, "verified", "s1").is_err());
    for membership in [
        FederationMembershipState::Active,
        FederationMembershipState::Draining,
        FederationMembershipState::Active,
    ] {
        let before = state
            .refresh_federation_transport_snapshot(&channel, &link)
            .unwrap();
        state
            .transition_federation_peer(&before, membership)
            .unwrap();
        let now = state
            .refresh_federation_transport_snapshot(&channel, &link)
            .unwrap();
        assert_eq!(
            now,
            state
                .federation_peer_snapshot(&channel.peer.host_id)
                .unwrap()
        );
        assert!(now.record_generation > before.record_generation);
        assert_eq!(now.transport_generation, channel.transport_generation);
        assert!(channel.same_transport_as(&now));
        if membership == FederationMembershipState::Draining {
            assert!(reserve(&state, &now, "draining", "s1").is_err());
        }
    }
    let current = state
        .refresh_federation_transport_snapshot(&channel, &link)
        .unwrap();
    assert!(reserve(&state, &channel, "stale", "s1").is_err());
    assert!(reserve(&state, &current, "current", "s1").is_ok());
}

#[test]
fn federation_transport_and_placement_reject_reused_uuid_after_reverification() {
    let state = central();
    let old = active(&state, peer(48001));
    let reservation = reserve(&state, &old, "original", "s1").unwrap();
    let old_link = incarnation(&state, &old);
    let next = state
        .federation_peer_connected(&old, Uuid::new_v4().to_string())
        .unwrap();
    let reused = state
        .federation_peer_connected(&next, old_link.clone())
        .unwrap();
    assert!(reused.transport_generation > next.transport_generation);
    state.verify_federation_peer(&reused, checks()).unwrap();
    let verified = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let current = state.federation_peer_snapshot(&old.peer.host_id).unwrap();
    assert!(!old.same_transport_as(&current));
    assert!(
        state
            .refresh_federation_transport_snapshot(&old, &old_link)
            .is_err()
    );
    assert!(
        state
            .refresh_federation_transport_snapshot(&current, &old_link)
            .is_ok()
    );
    assert!(
        state
            .ensure_federation_placement_current(&reservation)
            .is_err()
    );
    assert!(
        state
            .reconcile_federation_placement(&reservation, true)
            .is_err()
    );
    assert_eq!(
        state.inner.lock().unwrap().federation.placements["original"],
        reservation
    );
}

#[test]
fn federation_transport_rejects_configuration_enrollment_and_revocation_aba() {
    for change in [
        "enabled",
        "enrollment",
        "endpoint",
        "capability",
        "profile",
        "revoke",
    ] {
        let state = central();
        let original = peer(48001);
        let channel = connected(&state, original.clone());
        let link = incarnation(&state, &channel);
        if change == "enabled" {
            let settings = state.inner.lock().unwrap().preferences.federation.clone();
            let mut disabled = settings.clone();
            disabled.enabled = false;
            state.configure_federation(disabled).unwrap();
            state.configure_federation(settings).unwrap();
        } else if change == "revoke" {
            state
                .transition_federation_peer(&channel, FederationMembershipState::Revoked)
                .unwrap();
            assert!(
                state
                    .refresh_federation_transport_snapshot(&channel, &link)
                    .is_err()
            );
            state.enroll_federation_peer(original.clone()).unwrap();
        } else {
            let mut changed = original.clone();
            match change {
                "endpoint" => changed.endpoint.ssh_host = "other.example".to_owned(),
                "capability" => changed.pair_capability_ref = "other-pair".to_owned(),
                "profile" => changed.profile.binary_sha256 = "b".repeat(64),
                _ => (),
            }
            state.enroll_federation_peer(changed).unwrap();
            state.enroll_federation_peer(original).unwrap();
        }
        let before_connect = state
            .federation_peer_snapshot(&channel.peer.host_id)
            .unwrap();
        let current = state
            .federation_peer_connected(&before_connect, link.clone())
            .unwrap();
        assert!(
            current.transport_generation > channel.transport_generation,
            "{change}"
        );
        assert!(!channel.same_transport_as(&current), "{change}");
        let error = state
            .refresh_federation_transport_snapshot(&channel, &link)
            .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT, "{change}");
        assert!(
            state
                .refresh_federation_transport_snapshot(&current, &link)
                .is_ok()
        );
    }
}

#[test]
fn federation_transport_refresh_requires_durability_and_exact_link_but_grants_no_admission() {
    let state = central();
    let channel = connected(&state, peer(48001));
    let link = incarnation(&state, &channel);
    assert!(
        state
            .refresh_federation_transport_snapshot(&channel, &Uuid::new_v4().to_string())
            .is_err()
    );
    for flag in ["durability", "continuity", "missing-link"] {
        let mut inner = state.inner.lock().unwrap();
        inner.federation_durable = flag != "durability";
        inner.federation_continuity_lost = flag == "continuity";
        if flag == "missing-link" {
            inner.federation.links.clear();
        }
        assert!(
            state
                .refresh_federation_transport_snapshot_locked(&inner, &channel, &link)
                .is_err()
        );
        inner.federation_durable = true;
        inner.federation_continuity_lost = false;
    }
    let mut inner = state.inner.lock().unwrap();
    inner
        .federation
        .links
        .insert(channel.peer.host_id.clone(), link.clone());
    inner.federation.admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore);
    assert_eq!(
        state
            .refresh_federation_transport_snapshot_locked(&inner, &channel, &link)
            .unwrap(),
        channel
    );
    assert!(
        state
            .ensure_federation_admission_allowed_locked(&inner)
            .is_err()
    );
    drop(inner);
    assert!(reserve(&state, &channel, "no-grant", "s1").is_err());
}

#[test]
fn federation_draining_reconnect_reverifies_without_reactivating_or_freeing_old_reservations() {
    let state = central();
    let original = active(&state, peer(48001));
    let retained = reserve(&state, &original, "retained", "s1").unwrap();
    state
        .transition_federation_peer(&original, FederationMembershipState::Draining)
        .unwrap();
    let draining = state
        .federation_peer_snapshot(&original.peer.host_id)
        .unwrap();
    let channel = state
        .federation_peer_connected(&draining, Uuid::new_v4().to_string())
        .unwrap();
    assert_eq!(channel.membership, FederationMembershipState::Draining);
    assert!(
        state
            .transition_federation_peer(&channel, FederationMembershipState::Active)
            .is_err()
    );
    let mut incomplete = checks();
    incomplete.isolated_wake = false;
    assert!(state.verify_federation_peer(&channel, incomplete).is_err());
    state.verify_federation_peer(&channel, checks()).unwrap();
    let verified = state
        .refresh_federation_transport_snapshot(&channel, &incarnation(&state, &channel))
        .unwrap();
    assert_eq!(verified.membership, FederationMembershipState::Draining);
    assert_eq!(verified.transport_generation, channel.transport_generation);
    assert!(reserve(&state, &verified, "draining-new", "s2").is_err());
    assert!(
        state
            .ensure_federation_placement_current(&retained)
            .is_err()
    );
    state
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let reactivated = state
        .federation_peer_snapshot(&original.peer.host_id)
        .unwrap();
    assert_eq!(reactivated.membership, FederationMembershipState::Active);
    assert!(
        state
            .ensure_federation_placement_current(&retained)
            .is_err()
    );
    assert!(
        state
            .reconcile_federation_placement(&retained, true)
            .is_err()
    );
    assert!(reserve(&state, &reactivated, "still-full", "s2").is_err());
    assert_eq!(
        state.inner.lock().unwrap().federation.placements["retained"],
        retained
    );
}
