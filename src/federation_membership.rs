// Owns enrollment, connection and verification state transitions. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

// Staged domain APIs are consumed by the separately landed transport/operator slices.
#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn enroll_federation_peer(&self, mut peer: FederationPeerRecord) -> Result<(), ApiError> {
        self.mutate_federation(|domain, settings| {
            if !settings.enabled
                || settings.role == FederationRole::Standalone
                || (settings.role == FederationRole::Satellite
                    && settings.central.as_ref() != Some(&peer.identity))
            {
                return Err(ApiError::conflict(
                    "peer does not belong to the configured federation role",
                ));
            }
            let id = peer.identity.host_id.clone();
            let earlier = domain.peers.get(&id);
            if earlier
                .is_some_and(|old| old.identity.host_generation > peer.identity.host_generation)
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            peer.enrollment_generation = earlier
                .map(|row| federation_next_generation(row.enrollment_generation))
                .transpose()?
                .unwrap_or(1);
            peer.record_generation = earlier
                .map(|row| federation_next_generation(row.record_generation))
                .transpose()?
                .unwrap_or(1);
            peer.transport_generation = earlier
                .map(|row| federation_next_generation(row.transport_generation))
                .transpose()?
                .unwrap_or(1);
            peer.membership = FederationMembershipState::Enrolled;
            peer.verification = None;
            peer.validate()?;
            domain.links.remove(&id);
            domain.peers.insert(id, peer);
            Ok(())
        })
    }

    fn federation_peer_connected(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: String,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        if Uuid::parse_str(&incarnation)
            .ok()
            .filter(|id| !id.is_nil())
            .map(|id| id.to_string())
            .as_deref()
            != Some(incarnation.as_str())
        {
            return Err(ApiError::bad_request("invalid federation link incarnation"));
        }
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation(|domain, _| {
            let peer = domain
                .peers
                .get_mut(&expected.peer.host_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            if peer.record_generation != expected.record_generation {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            if domain.links.get(&peer.identity.host_id) != Some(&incarnation) {
                peer.record_generation = federation_next_generation(peer.record_generation)?;
                peer.transport_generation = federation_next_generation(peer.transport_generation)?;
                peer.verification = None;
                if peer.membership != FederationMembershipState::Draining {
                    peer.membership = FederationMembershipState::Enrolled;
                }
                peer.last_observed = Some(FederationLiveness {
                    link_incarnation: incarnation.clone(),
                    last_successful_rpc: None,
                    last_successful_wake: None,
                });
                domain
                    .links
                    .insert(peer.identity.host_id.clone(), incarnation);
            }
            let mut updated = expected.clone();
            updated.record_generation = peer.record_generation;
            updated.transport_generation = peer.transport_generation;
            updated.membership = peer.membership;
            Ok(updated)
        })
    }

    fn verify_federation_peer(
        &self,
        expected: &FederationPeerSnapshot,
        checks: FederationVerificationChecks,
    ) -> Result<(), ApiError> {
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation(|domain, _| {
            let peer = domain
                .peers
                .get_mut(&expected.peer.host_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            if peer.record_generation != expected.record_generation
                || !matches!(
                    peer.membership,
                    FederationMembershipState::Enrolled | FederationMembershipState::Draining
                )
                || !checks.complete()
            {
                return Err(ApiError::conflict(
                    "federation onboarding verification is incomplete or stale",
                ));
            }
            let link = domain
                .links
                .get(&peer.identity.host_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            peer.verification = Some(FederationVerification {
                basis: peer.verification_basis(),
                link_incarnation: link.clone(),
                checks,
            });
            if peer.membership != FederationMembershipState::Draining {
                peer.membership = FederationMembershipState::Verified;
            }
            peer.record_generation = federation_next_generation(peer.record_generation)?;
            Ok(())
        })
    }

    fn transition_federation_peer(
        &self,
        expected: &FederationPeerSnapshot,
        next: FederationMembershipState,
    ) -> Result<(), ApiError> {
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation(|domain, _| {
            if next == FederationMembershipState::Active && domain.admission_quarantine.is_some() {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let peer = domain
                .peers
                .get_mut(&expected.peer.host_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            if peer.record_generation != expected.record_generation {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let permitted = matches!(
                (peer.membership, next),
                (
                    FederationMembershipState::Verified,
                    FederationMembershipState::Active
                ) | (
                    FederationMembershipState::Active,
                    FederationMembershipState::Draining
                ) | (
                    FederationMembershipState::Draining,
                    FederationMembershipState::Active
                )
            ) && peer.verified_on(domain.links.get(&peer.identity.host_id));
            if next != FederationMembershipState::Revoked && !permitted {
                return Err(ApiError::conflict(
                    "invalid federation membership transition",
                ));
            }
            peer.membership = next;
            peer.record_generation = federation_next_generation(peer.record_generation)?;
            if next == FederationMembershipState::Revoked {
                peer.transport_generation = federation_next_generation(peer.transport_generation)?;
                peer.verification = None;
                domain.links.remove(&peer.identity.host_id);
            }
            Ok(())
        })
    }
}
