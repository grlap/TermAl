// Owns current permission/identity predicates and transient proof freshness.
// Does not retain authority snapshots or start work. Recovered from the policy component.
// Current enqueueTextPrompt policy predicates for the existing federation
// owner. Callers retain their existing link -> state/effect guards; these pure
// checks neither acquire authority nor install bindings or schedule work.

// A creation-key replay is committed history, not fresh execution authority.
// Its local issuer and immutable inputs must match even when consent, native
// placement or the remote transport is no longer current.
#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_creation_replay(
    domain: &FederationState,
    settings: &FederationSettings,
    kind: FederationPromptPermissionKind,
    creation_key: &str,
    scope: &FederationPromptScope,
    consent: Option<&FederationPromptPermissionIdentity>,
) -> Result<Option<FederationPromptPermission>, ApiError> {
    let (issuer, role) = match kind {
        FederationPromptPermissionKind::CallerGrant =>
            (&scope.source_host, FederationRole::Central),
        FederationPromptPermissionKind::DestinationConsent =>
            (&scope.target_host, FederationRole::Satellite),
    };
    if domain.local.as_ref() != Some(issuer) || settings.role != role {
        return Err(federation_stale_link());
    }
    let existing = domain.prompt_permissions.as_ref().and_then(|policy| {
        policy.records.iter().find(|record| {
            record.kind == kind && &record.issuer == issuer
                && record.creation_key == creation_key
        })
    });
    if let Some(record) = existing {
        if &record.scope != scope || record.consent.as_ref() != consent {
            return Err(ApiError::conflict(
                "permission creation key conflicts with its original scope",
            ));
        }
        return Ok(Some(record.clone()));
    }
    Ok(None)
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn read_federation_prompt_creation_replay(
        &self,
        kind: FederationPromptPermissionKind,
        creation_key: &str,
        scope: &FederationPromptScope,
        consent: Option<&FederationPromptPermissionIdentity>,
    ) -> Result<Option<FederationPromptPermission>, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let retained = federation_prompt_creation_replay(
            &inner.federation, &inner.preferences.federation,
            kind, creation_key, scope, consent,
        )?;
        if retained.is_some() {
            if inner.federation_continuity_lost {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            if !inner.federation_durable {
                return Err(ApiError::from_status(StatusCode::LOCKED,
                    "federation persistence is settling; retry"));
            }
        }
        Ok(retained)
    }

    async fn read_federation_prompt_grant_replay(
        &self,
        creation_key: &str,
        scope: &FederationPromptScope,
        consent: &FederationPromptPermissionIdentity,
    ) -> Result<Option<FederationPromptPermission>, ApiError> {
        let app = self.clone();
        let key = creation_key.to_owned();
        let scope = scope.clone();
        let consent = consent.clone();
        run_blocking_api(move || app.read_federation_prompt_creation_replay(
            FederationPromptPermissionKind::CallerGrant, &key, &scope, Some(&consent),
        )).await
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptPolicyReply {
    fn freshness(&self) -> Result<FederationPromptFreshness, ApiError> {
        Ok(FederationPromptFreshness {
            deadline: self.observed_until.ok_or_else(federation_stale_link)?,
        })
    }

    fn ensure_fresh_at(&self, now: std::time::Instant) -> Result<(), ApiError> {
        self.freshness()?.ensure_fresh_at(now)
    }

    fn observe(
        mut self,
        request_id: &str,
        incarnation: &str,
        deadline: std::time::Instant,
        now: std::time::Instant,
    ) -> Result<Self, ApiError> {
        self.capabilities.validate()?;
        self.permission.validate()?;
        federation_prompt_uuid(&self.nonce)?;
        if self.protocol_version != 1
            || self.request_id != request_id
            || self.link_incarnation != incarnation
            || self.expires_at == 0
        {
            return Err(federation_stale_link());
        }
        // expires_at is in the issuer's wall-clock domain. It returns to that
        // issuer in the receive envelope; the requester never compares epochs.
        self.observed_until = Some(deadline);
        self.ensure_fresh_at(now)?;
        Ok(self)
    }
}

// Immutable requester-local timing, returned with source capture so journal
// and transport cuts can recheck it. It grants no authority and is not stored.
#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptFreshness {
    deadline: std::time::Instant,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptFreshness {
    fn ensure_fresh_at(&self, now: std::time::Instant) -> Result<(), ApiError> {
        if now >= self.deadline {
            return Err(federation_stale_link());
        }
        Ok(())
    }
}

// Use the same requester clock at observation and each later owner cut. The
// per-channel fixture hook can expire time between a reply and its mutation;
// it does not replace policy, leases or the immutable observation deadline.
#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_observation_now(
    lease: &FederationLinkLease,
    phase: &'static str,
) -> std::time::Instant {
    lease.channel.control.observe(phase, 0);
    lease.channel.control.clock.now()
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    // Reuse the existing link -> StateInner owner on the blocking pool. Only
    // an owned result crosses await; neither mutex guard reaches the reactor.
    async fn read_federation_prompt_current<T, F>(
        &self,
        lease: &FederationLinkLease,
        read: F,
    ) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: FnOnce(&mut StateInner, &FederationPeerSnapshot) -> Result<T, ApiError> + Send + 'static,
    {
        let app = self.clone();
        let lease = lease.clone();
        run_blocking_api(move || lease.with_current(&app, read)).await
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_link_scope(
    lease: &FederationLinkLease,
    scope: &FederationPromptScope,
    role: FederationRole,
) -> Result<(), ApiError> {
    let (local, remote) = match role {
        FederationRole::Central => (&scope.source_host, &scope.target_host),
        FederationRole::Satellite => (&scope.target_host, &scope.source_host),
        FederationRole::Standalone => return Err(federation_stale_link()),
    };
    let captured = &lease.channel.snapshot;
    if captured.local_role != role
        || &captured.local != local
        || &captured.peer != remote
        || captured.enrollment_generation != scope.enrollment_generation
    {
        return Err(federation_stale_link());
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_target_placement_locked(
    inner: &StateInner,
    scope: &FederationPromptScope,
) -> Result<(), ApiError> {
    let record = federation_mailbox_root_locked(inner, &scope.target.session_id)?;
    match (
        &record.federation_mailbox_binding,
        scope.placement_generation,
    ) {
        (None, None) => Ok(()),
        (Some(binding), Some(generation))
            if binding.central == scope.source_host
                && binding.identity.host_id == scope.target_host.host_id
                && binding.identity.session_id == scope.target.session_id
                && binding.placement_generation == generation =>
        {
            Ok(())
        }
        _ => Err(federation_stale_link()),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_source_placement_locked(
    inner: &StateInner,
    scope: &FederationPromptScope,
) -> Result<(), ApiError> {
    let mut placements = inner.federation.placements.values().filter(|placement| {
        !placement.mailbox_only
            && placement.identity.host_id == scope.target_host.host_id
            && placement.identity.session_id == scope.target.session_id
            && placement.state != FederationPlacementState::Completed
    });
    let current = placements.next();
    if placements.next().is_some()
        || current.map(|p| p.placement_generation) != scope.placement_generation
        || current.is_some_and(|p| {
            !federation_placement_authority(&inner.federation, &inner.preferences.federation, p)
        })
    {
        return Err(federation_stale_link());
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_scope_current_locked(
    inner: &StateInner,
    scope: &FederationPromptScope,
    role: FederationRole,
) -> Result<(), ApiError> {
    scope.validate()?;
    let (local, remote, native) = match role {
        FederationRole::Central => (&scope.source_host, &scope.target_host, &scope.source),
        FederationRole::Satellite => (&scope.target_host, &scope.source_host, &scope.target),
        FederationRole::Standalone => return Err(federation_stale_link()),
    };
    let central = &scope.source_host;
    if !inner.preferences.federation.enabled
        || inner.preferences.federation.role != role
        || inner.preferences.federation.central.as_ref() != Some(central)
        || inner.federation.local.as_ref() != Some(local)
        || !inner.federation_durable
        || inner.federation_continuity_lost
        || inner.federation.admission_quarantine.is_some()
    {
        return Err(federation_stale_link());
    }
    let peer = inner
        .federation
        .peers
        .get(&remote.host_id)
        .ok_or_else(federation_stale_link)?;
    let peer_current = match role {
        FederationRole::Central => &peer.identity == remote
            && peer.enrollment_generation == scope.enrollment_generation
            && peer.membership == FederationMembershipState::Active
            && peer.verified_on(inner.federation.links.get(&remote.host_id)),
        FederationRole::Satellite => federation_satellite_central_current_locked(
            inner, central, scope.enrollment_generation),
        FederationRole::Standalone => false,
    };
    if !peer_current {
        return Err(federation_stale_link());
    }
    let identity =
        FederationMutationContext::locked(inner).prompt_native_root(&native.session_id)?;
    if &identity != native {
        return Err(federation_stale_link());
    }
    match role {
        FederationRole::Central => federation_prompt_source_placement_locked(inner, scope),
        FederationRole::Satellite => federation_prompt_target_placement_locked(inner, scope),
        FederationRole::Standalone => unreachable!(),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_permission_current_locked<'a>(
    inner: &'a StateInner,
    kind: FederationPromptPermissionKind,
    identity: &FederationPromptPermissionIdentity,
    scope: &FederationPromptScope,
) -> Result<&'a FederationPromptPermission, ApiError> {
    identity.validate()?;
    let permission = inner
        .federation
        .prompt_permissions
        .as_ref()
        .and_then(|policy| policy.find(&identity.id))
        .ok_or_else(|| {
            ApiError::from_status(
                StatusCode::FORBIDDEN,
                "enqueueTextPrompt permission is missing",
            )
        })?;
    if permission.kind != kind
        || permission.identity != *identity
        || permission.scope != *scope
        || permission.revoked
    {
        return Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "enqueueTextPrompt permission is revoked or changed",
        ));
    }
    Ok(permission)
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_dispatch_policy_locked(
    inner: &StateInner,
    scope: &FederationPromptScope,
    grant: &FederationPromptPermissionIdentity,
    consent: &FederationPromptPermissionIdentity,
    role: FederationRole,
) -> Result<(), ApiError> {
    federation_prompt_scope_current_locked(inner, scope, role)?;
    let grant = federation_prompt_permission_current_locked(
        inner,
        FederationPromptPermissionKind::CallerGrant,
        grant,
        scope,
    )?;
    federation_prompt_permission_current_locked(
        inner,
        FederationPromptPermissionKind::DestinationConsent,
        consent,
        scope,
    )?;
    if grant.consent.as_ref() != Some(consent) {
        return Err(federation_stale_link());
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_scope_from_basis(
    basis: &remote_prompt_authority::AuthorityBasis,
) -> FederationPromptScope {
    let (source_host, source_epoch, source_session, source_creation) = basis.source.components();
    let (target_host, target_epoch, target_session, target_creation) = basis.target.components();
    FederationPromptScope {
        source_host: FederationHostIdentity {
            host_id: source_host.to_string(),
            host_generation: source_epoch,
        },
        source: FederationSessionIdentity {
            host_id: source_host.to_string(),
            session_id: source_session.into(),
            creation_id: source_creation.to_string(),
        },
        target_host: FederationHostIdentity {
            host_id: target_host.to_string(),
            host_generation: target_epoch,
        },
        target: FederationSessionIdentity {
            host_id: target_host.to_string(),
            session_id: target_session.into(),
            creation_id: target_creation.to_string(),
        },
        enrollment_generation: basis.enrollment_generation.get(),
        placement_generation: basis.placement_generation.map(std::num::NonZeroU64::get),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_identity_from_value(
    identity: &remote_prompt_authority::DecisionIdentity,
) -> FederationPromptPermissionIdentity {
    let (id, generation) = identity.components();
    FederationPromptPermissionIdentity {
        id: id.to_string(),
        generation,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_intent_policy_locked(
    inner: &StateInner,
    intent: &remote_prompt_authority::Intent,
    role: FederationRole,
) -> Result<(), ApiError> {
    let (basis, reference, _, _) = intent.components();
    let grant = federation_prompt_identity_from_value(&basis.grant);
    if reference.to_string() != grant.id {
        return Err(federation_stale_link());
    }
    federation_prompt_dispatch_policy_locked(
        inner,
        &federation_prompt_scope_from_basis(basis),
        &grant,
        &federation_prompt_identity_from_value(&basis.consent),
        role,
    )
}
