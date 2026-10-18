// Owns current enrollment snapshots and exact authority validation. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

#[cfg_attr(not(test), allow(dead_code))]
fn federation_snapshot_locked(
    inner: &StateInner,
    id: &str,
) -> Result<FederationPeerSnapshot, ApiError> {
    let settings = &inner.preferences.federation;
    if !settings.enabled || settings.role == FederationRole::Standalone {
        return Err(ApiError::conflict("federation is disabled"));
    }
    if !inner.federation_durable || inner.federation_continuity_lost {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let local = inner
        .federation
        .local
        .clone()
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    settings.validate(&local)?;
    let peer = inner
        .federation
        .peers
        .get(id)
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    let snapshot = FederationPeerSnapshot {
        local,
        local_role: settings.role,
        peer: peer.identity.clone(),
        central: settings
            .central
            .clone()
            .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?,
        enrollment_generation: peer.enrollment_generation,
        record_generation: peer.record_generation,
        transport_generation: peer.transport_generation,
        pair_capability_ref: peer.pair_capability_ref.clone(),
        endpoint: peer.endpoint.clone(),
        membership: peer.membership,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    /// Runtime-only availability CAS. Losing a channel must fence admission
    /// immediately, including while an unrelated durable mutation is pending.
    /// A late teardown can never clear a replacement incarnation or basis.
    fn federation_peer_disconnected(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
    ) -> bool {
        if expected.validate().is_err() {
            return false;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(peer) = inner.federation.peers.get(&expected.peer.host_id) else {
            return false;
        };
        if inner.federation.local.as_ref() != Some(&expected.local)
            || inner.preferences.federation.role != expected.local_role
            || inner.preferences.federation.central.as_ref() != Some(&expected.central)
            || peer.identity != expected.peer
            || peer.enrollment_generation != expected.enrollment_generation
            || peer.transport_generation != expected.transport_generation
            || peer.endpoint != expected.endpoint
            || peer.pair_capability_ref != expected.pair_capability_ref
            || inner
                .federation
                .links
                .get(&expected.peer.host_id)
                .map(String::as_str)
                != Some(incarnation)
        {
            return false;
        }
        inner.federation.links.remove(&expected.peer.host_id);
        true
    }

    /// Reconnect only the caller's unchanged installed transport basis after
    /// its link is gone. Membership may advance; configuration cannot be adopted.
    fn federation_reconnect_snapshot(
        &self,
        expected: &FederationPeerSnapshot,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        self.federation_reconnect_snapshot_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            expected,
        )
    }

    fn federation_reconnect_snapshot_locked(
        &self,
        inner: &StateInner,
        expected: &FederationPeerSnapshot,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        expected.validate()?;
        let current = federation_snapshot_locked(inner, &expected.peer.host_id)?;
        if !expected.same_transport_as(&current)
            || inner.federation.links.contains_key(&expected.peer.host_id)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(current)
    }

    /// Refresh membership bookkeeping only for this channel's unchanged
    /// transport authority. This grants neither placement nor new admission.
    fn refresh_federation_transport_snapshot(
        &self,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        self.refresh_federation_transport_snapshot_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            expected,
            incarnation,
        )
    }

    fn refresh_federation_transport_snapshot_locked(
        &self,
        inner: &StateInner,
        expected: &FederationPeerSnapshot,
        incarnation: &str,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        expected.validate()?;
        let current = federation_snapshot_locked(inner, &expected.peer.host_id)?;
        if !expected.same_transport_as(&current)
            || inner
                .federation
                .links
                .get(&expected.peer.host_id)
                .map(String::as_str)
                != Some(incarnation)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(current)
    }

    /// For NEW coordinated admissions, separately from a transport snapshot
    /// used to establish a link or reconcile an original outcome.
    fn ensure_federation_admission_allowed(&self) -> Result<(), ApiError> {
        self.ensure_federation_admission_allowed_locked(
            &self.inner.lock().expect("state mutex poisoned"),
        )
    }

    fn ensure_federation_admission_allowed_locked(
        &self,
        inner: &StateInner,
    ) -> Result<(), ApiError> {
        if !inner.preferences.federation.enabled
            || inner.preferences.federation.role == FederationRole::Standalone
            || !inner.federation_durable
            || inner.federation_continuity_lost
            || inner.federation.admission_quarantine.is_some()
            || inner.federation.local.is_none()
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
    fn federation_peer_snapshot(
        &self,
        peer_host_id: &str,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        federation_snapshot_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            peer_host_id,
        )
    }

    fn federation_peer_snapshots(&self) -> Result<Vec<FederationPeerSnapshot>, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.preferences.federation.enabled
            || inner.preferences.federation.role == FederationRole::Standalone
        {
            return Ok(Vec::new());
        }
        if !inner.federation_durable
            || inner.federation_continuity_lost
            || inner.federation.local.is_none()
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        inner
            .federation
            .peers
            .values()
            .filter(|peer| peer.membership != FederationMembershipState::Revoked)
            .map(|peer| federation_snapshot_locked(&inner, &peer.identity.host_id))
            .collect()
    }

    fn ensure_federation_peer_current(
        &self,
        expected: &FederationPeerSnapshot,
    ) -> Result<(), ApiError> {
        self.ensure_federation_peer_current_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            expected,
        )
    }

    fn ensure_federation_peer_current_locked(
        &self,
        inner: &StateInner,
        expected: &FederationPeerSnapshot,
    ) -> Result<(), ApiError> {
        expected.validate()?;
        if federation_snapshot_locked(inner, &expected.peer.host_id)? != *expected {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    }
}
