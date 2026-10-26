// Owns enrollment, connection and verification state transitions. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

// Staged domain APIs are consumed by the separately landed transport/operator slices.
#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn enroll_federation_peer(&self, peer: FederationPeerRecord) -> Result<(), ApiError> {
        self.enroll_federation_peer_with(peer, |_, _| Ok(()))
    }

    // The callback edits only this cloned candidate; it cannot perform I/O,
    // re-enter AppState or dispatch a second mutation. Error installs nothing.
    fn enroll_federation_peer_with(
        &self,
        mut peer: FederationPeerRecord,
        configure: impl FnOnce(&mut FederationState, &FederationPeerRecord) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
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
            peer.requalification = None;
            peer.validate()?;
            domain.links.remove(&id);
            domain.peers.insert(id, peer.clone());
            configure(domain, &peer)
        })
    }

    fn federation_peer_connected(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: String,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        self.federation_peer_connected_owned(expected, incarnation, &mut None)
    }

    // The installed record receipt survives durability errors for the owning
    // channel's accounting. A runtime link is published only after durability.
    fn federation_peer_connected_owned(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: String,
        installed: &mut Option<FederationPeerSnapshot>,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        self.federation_peer_connected_owned_with_clock(
            expected,
            incarnation,
            installed,
            EngramBudgetClock::Real,
        )
    }

    fn federation_peer_connected_owned_with_clock(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: String,
        installed: &mut Option<FederationPeerSnapshot>,
        clock: EngramBudgetClock,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        *installed = None;
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
        let updated = self.mutate_federation_with_install_receipt(
            false,
            |domain, _| {
                let requalification = federation_requalification_capture(domain, expected);
                let peer = domain
                    .peers
                    .get_mut(&expected.peer.host_id)
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                if peer.record_generation != expected.record_generation {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                if domain.links.get(&peer.identity.host_id) != Some(&incarnation) {
                    peer.requalification = requalification;
                    peer.record_generation = federation_next_generation(peer.record_generation)?;
                    peer.transport_generation =
                        federation_next_generation(peer.transport_generation)?;
                    // Enrollment/operator authorization survives a process or
                    // build change. The retained report remains diagnostic:
                    // its old incarnation is never current on this new link.
                    peer.last_observed = Some(FederationLiveness {
                        link_incarnation: incarnation.clone(),
                        last_successful_rpc: None,
                        last_successful_wake: None,
                    });
                    // The prior channel is no longer authoritative for this
                    // record. Do not publish the new UUID before its record commits.
                    domain.links.remove(&peer.identity.host_id);
                }
                let mut updated = expected.clone();
                updated.record_generation = peer.record_generation;
                updated.transport_generation = peer.transport_generation;
                updated.membership = peer.membership;
                Ok(updated)
            },
            |updated| *installed = Some(updated.clone()),
            clock,
        )?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        // A concurrent mutation may have superseded this returned record
        // after its fence settled. Never publish into a successor's basis.
        self.ensure_federation_peer_current_locked(&inner, &updated)?;
        inner
            .federation
            .links
            .insert(expected.peer.host_id.clone(), incarnation);
        Ok(updated)
    }

    fn verify_federation_peer(
        &self,
        expected: &FederationPeerSnapshot,
        checks: FederationVerificationChecks,
    ) -> Result<(), ApiError> {
        self.verify_federation_peer_with(expected, checks, |_| Ok(()))
    }

    // Report finalization shares the proof's exact candidate and content fence.
    // The callback must preserve its peer/link/proof and Draining admission hold.
    fn verify_federation_peer_with(
        &self,
        expected: &FederationPeerSnapshot,
        checks: FederationVerificationChecks,
        finish: impl FnOnce(&mut FederationState) -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        self.ensure_federation_peer_current(expected)?;
        #[cfg(test)]
        if let Some(peer) = self.federation_peer_handle(&expected.peer.host_id) {
            peer.channel.control.observe("verificationChecked", 0);
        }
        self.mutate_federation(|domain, _| {
            let peer = domain
                .peers
                .get_mut(&expected.peer.host_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            if peer.record_generation != expected.record_generation
                || peer.membership == FederationMembershipState::Revoked
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
            if peer.membership == FederationMembershipState::Enrolled {
                peer.membership = FederationMembershipState::Verified;
            }
            peer.record_generation = federation_next_generation(peer.record_generation)?;
            finish(domain)
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
                peer.requalification = None;
                peer.transport_generation = federation_next_generation(peer.transport_generation)?;
                peer.verification = None;
                domain.links.remove(&peer.identity.host_id);
            }
            Ok(())
        })
    }
}
