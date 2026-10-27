// Owns satellite commit/effect guards over an existing native placement binding.
// No caller can install a binding, choose another executor or adopt a proxy id.

/// Whether this satellite still accepts `central`, at `enrollment_generation`,
/// as its configured central. Only central activates a pair: a satellite's
/// own record of its central stays enrolled, so any membership short of
/// revoked is current here. Link and target checks stay with each caller.
fn federation_satellite_central_current_locked(
    inner: &StateInner,
    central: &FederationHostIdentity,
    enrollment_generation: u64,
) -> bool {
    inner.preferences.federation.enabled
        && inner.preferences.federation.role == FederationRole::Satellite
        && inner.federation_durable && !inner.federation_continuity_lost
        && inner.federation.admission_quarantine.is_none()
        && inner.preferences.federation.central.as_ref() == Some(central)
        && inner.federation.peers.get(&central.host_id).is_some_and(|peer| peer.identity == *central
            && peer.enrollment_generation == enrollment_generation
            && peer.membership != FederationMembershipState::Revoked)
}

fn federation_executor_target_locked(
    inner: &StateInner,
    operation: &FederationExecutorOperation,
) -> Result<(), ApiError> {
    inner.require_delegation_inventory()?;
    let binding = operation.mailbox_binding();
    let local = inner.federation.local.as_ref();
    if operation.central_host_id != binding.central.host_id
        || !federation_satellite_central_current_locked(inner, &binding.central, operation.enrollment_generation)
        || local.is_none_or(|local| local.host_id != operation.target_host_id)
    {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let record = federation_mailbox_root_locked(inner, &operation.session_id)?;
    if record.federation_mailbox_binding.as_ref() != Some(&binding) {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    Ok(())
}

struct FederationExecutorAuthority<'a> {
    app: &'a AppState,
    operation: &'a FederationExecutorOperation,
    lease: Option<&'a FederationLinkLease>,
}

// Permanent captured-identity retirement only. Link loss, disabled settings,
// quarantine and unsettled durability remain visible retry/refusal conditions.
fn federation_executor_operation_retired(inner: &StateInner, operation: &FederationExecutorOperation) -> bool {
    if !inner.delegation_inventory_complete() { return false; }
    let binding = operation.mailbox_binding();
    inner.federation.local.as_ref().is_none_or(|host| host.host_id != operation.target_host_id)
        || inner.preferences.federation.central.as_ref() != Some(&binding.central)
        || inner.federation.peers.get(&operation.central_host_id).is_none_or(|peer|
            peer.identity != binding.central || peer.enrollment_generation != operation.enrollment_generation
                || peer.membership == FederationMembershipState::Revoked)
        || inner.find_session_index(&operation.session_id).is_none_or(|index|
            inner.sessions[index].federation_mailbox_binding.as_ref() != Some(&binding))
}

impl AppState {
    fn federation_executor_host_id(&self) -> Result<String> {
        self.inner.lock().expect("state mutex poisoned").federation.local.as_ref()
            .map(|local| local.host_id.clone()).ok_or_else(federation_executor_conflict)
    }
}

impl FederationExecutorAuthority<'_> {
    // Storage calls this last, after writer/connection admission, and retains
    // both guards through COMMIT. Recovery has no historical link permission.
    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let link = self.lease.map(|lease| {
            let link = lease.channel.state.lock().expect("federation link mutex poisoned");
            if link.incarnation.as_deref() != Some(lease.incarnation.as_str())
                || link.recorded_incarnation.as_deref() != Some(lease.incarnation.as_str())
            {
                return Err(federation_executor_conflict());
            }
            Ok(link)
        }).transpose()?;
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        federation_executor_target_locked(&inner, self.operation)
            .map_err(|_| federation_executor_conflict())?;
        if let Some(lease) = self.lease {
            let peer = self.app.refresh_federation_transport_snapshot_locked(
                &inner, &lease.channel.transport_basis(), &lease.incarnation,
            ).map_err(|_| federation_executor_conflict())?;
            if peer.local_role != FederationRole::Satellite
                || peer.peer != self.operation.mailbox_binding().central
                || peer.local.host_id != self.operation.target_host_id
                || peer.enrollment_generation != self.operation.enrollment_generation
            {
                return Err(federation_executor_conflict());
            }
        }
        Ok(FederationMailboxAdmission { _inner: inner, _link: link })
    }
}
