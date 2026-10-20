// Owns explicit qualified placement, retained reservations and caller fences.
// Does not launch/stop runtimes, pick hosts, or grant test/gate permission.
// New foundation invoked under the registry's serialized state mutation.

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationPlacementState {
    Reserved,
    Uncertain,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationPlacement {
    admission_key: String,
    identity: FederationSessionIdentity,
    placement_generation: u64,
    peer: FederationPeerSnapshot,
    link_incarnation: String,
    execution_basis: String,
    build_slots: u32,
    gate_slots: u32,
    state: FederationPlacementState,
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_placement_authority(
    domain: &FederationState,
    settings: &FederationSettings,
    placement: &FederationPlacement,
) -> bool {
    let Some(peer) = domain.peers.get(&placement.identity.host_id) else {
        return false;
    };
    domain.admission_quarantine.is_none()
        && settings.enabled
        && settings.role == FederationRole::Central
        && domain.local.as_ref() == Some(&placement.peer.local)
        && settings.central.as_ref() == Some(&placement.peer.central)
        && peer.identity == placement.peer.peer
        && peer.enrollment_generation == placement.peer.enrollment_generation
        && peer.transport_generation == placement.peer.transport_generation
        && peer.endpoint == placement.peer.endpoint
        && peer.pair_capability_ref == placement.peer.pair_capability_ref
        && peer.verification_basis() == placement.execution_basis
        && matches!(
            peer.membership,
            FederationMembershipState::Active | FederationMembershipState::Draining
        )
        && peer.verified_on(domain.links.get(&peer.identity.host_id))
        && domain.links.get(&peer.identity.host_id) == Some(&placement.link_incarnation)
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn reserve_federation_placement(
        &self,
        expected: &FederationPeerSnapshot,
        admission_key: String,
        session_id: String,
        build_slots: u32,
        gate_slots: u32,
    ) -> Result<FederationPlacement, ApiError> {
        self.reserve_federation_placement_with(
            expected,
            admission_key,
            session_id,
            build_slots,
            gate_slots,
            None,
            |_, _, placement| Ok(placement.clone()),
        )
    }

    // The callback changes only the cloned candidate. A mailbox install intent
    // and its reservation therefore enter the same content-fenced transaction.
    fn reserve_federation_placement_with<T>(
        &self,
        expected: &FederationPeerSnapshot,
        admission_key: String,
        session_id: String,
        build_slots: u32,
        gate_slots: u32,
        lease: Option<&FederationLinkLease>,
        retain: impl FnOnce(
            &mut FederationState,
            &FederationSettings,
            &FederationPlacement,
        ) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        validate_federation_reference(&admission_key)?;
        self.ensure_federation_peer_current(expected)?;
        self.mutate_federation_core_on_link(
            lease,
            false,
            |domain, settings, _| {
                if domain.admission_quarantine.is_some() {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                if !settings.enabled || settings.role != FederationRole::Central {
                    return Err(ApiError::conflict("central federation role required"));
                }
                if let Some(existing) = domain.placements.get(&admission_key).cloned() {
                    if existing.identity.host_id != expected.peer.host_id
                        || existing.identity.session_id != session_id
                        || existing.build_slots != build_slots
                        || existing.gate_slots != gate_slots
                    {
                        return Err(ApiError::conflict(
                            "federation admission key conflicts with its original intent",
                        ));
                    }
                    return retain(domain, settings, &existing);
                }
                let peer = domain
                    .peers
                    .get(&expected.peer.host_id)
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                if peer.record_generation != expected.record_generation
                    || peer.membership != FederationMembershipState::Active
                    || !peer.verified_on(domain.links.get(&peer.identity.host_id))
                {
                    return Err(ApiError::conflict(
                        "satellite is not active with current verification",
                    ));
                }
                let mut sessions = 0_u64;
                let mut builds = 0_u64;
                let mut gates = 0_u64;
                let mut generation: u64 = 0;
                for old in domain
                    .placements
                    .values()
                    .filter(|old| old.identity.host_id == peer.identity.host_id)
                {
                    if old.identity.session_id == session_id {
                        generation = generation.max(old.placement_generation);
                        if old.state != FederationPlacementState::Completed {
                            return Err(ApiError::conflict(
                                "session already has an unresolved placement",
                            ));
                        }
                    }
                    if old.state != FederationPlacementState::Completed {
                        sessions += 1;
                        builds += u64::from(old.build_slots);
                        gates += u64::from(old.gate_slots);
                    }
                }
                if sessions >= u64::from(peer.capacity.max_sessions)
                    || builds + u64::from(build_slots) > u64::from(peer.capacity.build_slots)
                    || gates + u64::from(gate_slots) > u64::from(peer.capacity.gate_slots)
                {
                    return Err(ApiError::conflict(
                        "satellite declared capacity is fully reserved",
                    ));
                }
                let identity = FederationSessionIdentity {
                    host_id: peer.identity.host_id.clone(),
                    session_id,
                    creation_id: Uuid::new_v4().to_string(),
                };
                identity.validate()?;
                if domain
                    .placements
                    .values()
                    .any(|old| old.identity.creation_id == identity.creation_id)
                {
                    return Err(ApiError::conflict(
                        "session creation identity collision; retry admission",
                    ));
                }
                let placement = FederationPlacement {
                    admission_key: admission_key.clone(),
                    identity,
                    placement_generation: federation_next_generation(generation)?,
                    peer: expected.clone(),
                    link_incarnation: domain
                        .links
                        .get(&peer.identity.host_id)
                        .expect("verified link")
                        .clone(),
                    execution_basis: peer.verification_basis(),
                    build_slots,
                    gate_slots,
                    state: FederationPlacementState::Reserved,
                };
                domain.placements.insert(admission_key, placement.clone());
                retain(domain, settings, &placement)
            },
            |_| {},
            EngramBudgetClock::Real,
        )
    }

    fn ensure_federation_placement_current(
        &self,
        expected: &FederationPlacement,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.federation_durable
            || inner.federation_continuity_lost
            || !federation_placement_authority(
                &inner.federation,
                &inner.preferences.federation,
                expected,
            )
            || expected.state == FederationPlacementState::Completed
            || inner.federation.placements.get(&expected.admission_key) != Some(expected)
            || inner.federation.links.get(&expected.identity.host_id)
                != Some(&expected.link_incarnation)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }

    /// Only a current exact runtime owner may reconcile its reservation.
    /// Disconnection, stale liveness and revocation never release capacity.
    fn reconcile_federation_placement(
        &self,
        expected: &FederationPlacement,
        completed: bool,
    ) -> Result<(), ApiError> {
        self.ensure_federation_placement_current(expected)?;
        self.mutate_federation(|domain, settings| {
            if domain.links.get(&expected.identity.host_id) != Some(&expected.link_incarnation)
                || domain.placements.get(&expected.admission_key) != Some(expected)
                || !federation_placement_authority(domain, settings, expected)
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let placement = domain
                .placements
                .get_mut(&expected.admission_key)
                .expect("exact placement checked");
            placement.state = if completed {
                FederationPlacementState::Completed
            } else {
                FederationPlacementState::Uncertain
            };
            Ok(())
        })
    }

    fn validate_federation_session_assertion(
        &self,
        authenticated_peer: &FederationPeerSnapshot,
        asserted: &FederationSessionIdentity,
    ) -> Result<(), ApiError> {
        asserted.validate()?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_federation_peer_current_locked(&inner, authenticated_peer)?;
        if asserted.host_id != authenticated_peer.peer.host_id
            || !inner.federation.placements.values().any(|placement| {
                placement.identity == *asserted
                    && placement.state != FederationPlacementState::Completed
                    && federation_placement_authority(
                        &inner.federation,
                        &inner.preferences.federation,
                        placement,
                    )
                    && inner.federation.links.get(&asserted.host_id)
                        == Some(&placement.link_incarnation)
            })
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
}
