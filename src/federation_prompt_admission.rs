// Owns source intent capture and receiver permission observation using existing owners.
// Does not persist a journal or activate a queue. Recovered from the policy component.
// Exact own-caller intent capture and permission observations for the existing
// executor admission path. No journal, queue, lifecycle or scheduler is owned
// here. Each later effect must retain the existing owner guards and revalidate.

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_native_value(
    host: &FederationHostIdentity,
    session: &FederationSessionIdentity,
) -> Result<remote_prompt_authority::NativeRoot, ApiError> {
    remote_prompt_authority::NativeRoot::new(
        federation_prompt_uuid(&host.host_id)?,
        host.host_generation,
        session.session_id.clone(),
        federation_prompt_uuid(&session.creation_id)?,
    )
    .map_err(|_| federation_stale_link())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_intent_value(
    permission: &FederationPromptPermission,
    request: &remote_prompt_protocol::OwnPrompt,
) -> Result<remote_prompt_authority::Intent, ApiError> {
    let consent = permission
        .consent
        .as_ref()
        .ok_or_else(federation_stale_link)?;
    let decision = |identity: &FederationPromptPermissionIdentity| {
        remote_prompt_authority::DecisionIdentity::new(
            federation_prompt_uuid(&identity.id)?,
            identity.generation,
        )
        .map_err(|_| federation_stale_link())
    };
    let scope = &permission.scope;
    let basis = remote_prompt_authority::AuthorityBasis {
        source: federation_prompt_native_value(&scope.source_host, &scope.source)?,
        target: federation_prompt_native_value(&scope.target_host, &scope.target)?,
        grant: decision(&permission.identity)?,
        consent: decision(consent)?,
        enrollment_generation: std::num::NonZeroU64::new(scope.enrollment_generation)
            .ok_or_else(federation_stale_link)?,
        placement_generation: scope
            .placement_generation
            .map(|g| std::num::NonZeroU64::new(g).ok_or_else(federation_stale_link))
            .transpose()?,
    };
    remote_prompt_authority::Intent::new(
        basis,
        request.reference,
        request.idempotency_key.clone(),
        request.text.clone(),
    )
    .map_err(|_| federation_stale_link())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_own_grant_locked(
    inner: &StateInner,
    caller: &str,
    reference: Uuid,
) -> Result<FederationPromptPermission, ApiError> {
    let grant = inner
        .federation
        .prompt_permissions
        .as_ref()
        .and_then(|policy| policy.find(&reference.to_string()))
        .filter(|grant| {
            grant.kind == FederationPromptPermissionKind::CallerGrant
                && grant.scope.source.session_id == caller
        })
        .ok_or_else(|| {
            ApiError::from_status(
                StatusCode::FORBIDDEN,
                "caller has no current prompt permission",
            )
        })?;
    let consent = grant.consent.as_ref().ok_or_else(federation_stale_link)?;
    federation_prompt_dispatch_policy_locked(
        inner,
        &grant.scope,
        &grant.identity,
        consent,
        FederationRole::Central,
    )?;
    Ok(grant.clone())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_issuer_proof_current(
    lease: &FederationLinkLease,
    proof: &remote_prompt_protocol::UnverifiedProof,
) -> Result<(), ApiError> {
    federation_prompt_issuer_proof_current_at(lease, proof, federation_prompt_now()?)
}

// This check runs only on the satellite that issued the absolute expiry.
// The source validates its own monotonic observation deadline instead.
#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_issuer_proof_current_at(
    lease: &FederationLinkLease,
    proof: &remote_prompt_protocol::UnverifiedProof,
    issuer_now: u64,
) -> Result<(), ApiError> {
    let (link, _, expires) = proof.components().map_err(|_| federation_stale_link())?;
    if link.to_string() != lease.incarnation
        || expires <= issuer_now
        || expires > issuer_now.saturating_add(30)
    {
        return Err(federation_stale_link());
    }
    Ok(())
}

// Only a committed permanent revocation is a cancellation fact. Missing data,
// disabled settings, link/IO loss, quarantine and pause remain scheduling holds.
#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_intent_revoked_locked(
    inner: &StateInner,
    intent: &remote_prompt_authority::Intent,
) -> bool {
    // The mutation owner installs a candidate before its exact write/readback.
    // Such a candidate is a scheduling hold, never permanent cancellation.
    if !inner.federation_durable
        || inner.federation_continuity_lost
        || inner.federation.admission_quarantine.is_some()
    {
        return false;
    }
    let (basis, _, _, _) = intent.components();
    let scope = federation_prompt_scope_from_basis(basis);
    let Some(policy) = &inner.federation.prompt_permissions else {
        return false;
    };
    [
        (FederationPromptPermissionKind::CallerGrant, &basis.grant),
        (
            FederationPromptPermissionKind::DestinationConsent,
            &basis.consent,
        ),
    ]
    .into_iter()
    .any(|(kind, identity)| {
        let identity = federation_prompt_identity_from_value(identity);
        policy.find(&identity.id).is_some_and(|record| {
            record.kind == kind
                && record.scope == scope
                && record.revoked
                && record.identity.generation >= identity.generation
        })
    })
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    // Called only on NEW source admission. Historical status/reconciliation
    // reads the retained exact intent and never enters this method.
    async fn capture_remote_prompt_for_caller(
        &self,
        caller: &str,
        request: &remote_prompt_protocol::OwnPrompt,
    ) -> Result<
        (
            remote_prompt_authority::Intent,
            remote_prompt_protocol::UnverifiedProof,
            Arc<FederationPeerHandle>,
            FederationLinkLease,
            FederationPromptFreshness,
        ),
        ApiError,
    > {
        remote_prompt_protocol::encode_own(request)
            .map_err(|_| ApiError::bad_request("invalid or excessive own prompt request"))?;
        let app = self.clone();
        let caller = caller.to_owned();
        let reference = request.reference;
        let captured_caller = caller.clone();
        let grant = run_blocking_api(move || {
            let inner = app.inner.lock().expect("state mutex poisoned");
            federation_prompt_own_grant_locked(&inner, &captured_caller, reference)
        })
        .await?;
        let handle = self
            .federation_peer_handle(&grant.scope.target_host.host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = handle.channel.current_lease()?;
        let observed = self
            .read_federation_prompt_policy(
                &handle.rpc,
                &lease,
                FederationPromptPolicyOperation::DestinationProof {
                    scope: grant.scope.clone(),
                    consent: grant.consent.clone().ok_or_else(federation_stale_link)?,
                },
            )
            .await?;
        let intent = federation_prompt_intent_value(&grant, request)?;
        let proof = remote_prompt_protocol::UnverifiedProof::new(
            federation_prompt_uuid(&observed.link_incarnation)?,
            federation_prompt_uuid(&observed.nonce)?,
            observed.expires_at,
        )
        .map_err(|_| federation_stale_link())?;
        remote_prompt_protocol::encode_receive(&intent, &proof)
            .map_err(|_| ApiError::bad_request("prompt receive envelope exceeds its bounds"))?;
        let captured_grant = grant.clone();
        let freshness = observed.freshness()?;
        let capture_lease = lease.clone();
        self.read_federation_prompt_current(&lease, move |inner, _| {
            if federation_prompt_own_grant_locked(inner, &caller, reference)? != captured_grant {
                return Err(federation_stale_link());
            }
            freshness.ensure_fresh_at(federation_prompt_observation_now(
                &capture_lease,
                "promptPolicyCaptureReturn",
            ))
        })
        .await?;
        Ok((intent, proof, handle, lease, freshness))
    }

    // Called before FIRST receiver admission, not duplicate/status recovery.
    // Source permission is read from its authenticated owner, then retained as
    // data in the same primary federation unit as destination-local consent.
    async fn observe_remote_prompt_grant(
        &self,
        lease: &FederationLinkLease,
        rpc: &FederationRpcClient,
        intent: &remote_prompt_authority::Intent,
        proof: &remote_prompt_protocol::UnverifiedProof,
    ) -> Result<(), ApiError> {
        let (basis, _, _, _) = intent.components();
        federation_prompt_link_scope(
            lease,
            &federation_prompt_scope_from_basis(basis),
            FederationRole::Satellite,
        )?;
        federation_prompt_issuer_proof_current(lease, proof)?;
        let (basis, reference, _, _) = intent.components();
        let scope = federation_prompt_scope_from_basis(basis);
        let grant = federation_prompt_identity_from_value(&basis.grant);
        let consent = federation_prompt_identity_from_value(&basis.consent);
        if reference.to_string() != grant.id {
            return Err(federation_stale_link());
        }
        let captured_scope = scope.clone();
        let captured_consent = consent.clone();
        self.read_federation_prompt_current(lease, move |inner, _| {
            federation_prompt_scope_current_locked(
                inner,
                &captured_scope,
                FederationRole::Satellite,
            )?;
            federation_prompt_permission_current_locked(
                inner,
                FederationPromptPermissionKind::DestinationConsent,
                &captured_consent,
                &captured_scope,
            )?;
            Ok(())
        })
        .await?;
        let observed = self
            .read_federation_prompt_policy(
                rpc,
                lease,
                FederationPromptPolicyOperation::CallerGrant {
                    scope: scope.clone(),
                    grant,
                },
            )
            .await?;
        if observed.permission.consent.as_ref() != Some(&consent) {
            return Err(federation_stale_link());
        }
        let app = self.clone();
        let install_lease = lease.clone();
        let install_proof = proof.clone();
        let policy_scope = scope.clone();
        let policy_consent = consent.clone();
        let policy_grant = federation_prompt_identity_from_value(&basis.grant);
        run_blocking_api(move || {
            app.mutate_federation_core_on_link(
                Some(&install_lease),
                false,
                |domain, _, context| {
                    observed.ensure_fresh_at(federation_prompt_observation_now(
                        &install_lease,
                        "promptPolicyMirrorMutation",
                    ))?;
                    federation_prompt_issuer_proof_current(&install_lease, &install_proof)?;
                    federation_prompt_scope_current_locked(
                        context.native_state,
                        &scope,
                        FederationRole::Satellite,
                    )?;
                    federation_prompt_permission_current_locked(
                        context.native_state,
                        FederationPromptPermissionKind::DestinationConsent,
                        &consent,
                        &scope,
                    )?;
                    domain
                        .prompt_permissions
                        .get_or_insert_with(FederationPromptPermissions::default)
                        .retain(observed.permission.clone())?;
                    Ok(())
                },
                |_| {},
                EngramBudgetClock::Real,
            )
        })
        .await?;
        self.read_federation_prompt_current(lease, move |inner, _| {
            federation_prompt_dispatch_policy_locked(
                inner,
                &policy_scope,
                &policy_grant,
                &policy_consent,
                FederationRole::Satellite,
            )
        })
        .await
    }

    // Local revoke is already committed before transport. A failed or lost
    // reply leaves remote effectiveness unknown and retains that revocation.
    async fn confirm_remote_prompt_revocation(
        &self,
        id: &str,
    ) -> Result<FederationPromptPermission, ApiError> {
        federation_prompt_uuid(id)?;
        let app = self.clone();
        let record_id = id.to_owned();
        let record = run_blocking_api(move || {
            let inner = app.inner.lock().expect("state mutex poisoned");
            let local = inner
                .federation
                .local
                .as_ref()
                .ok_or_else(federation_stale_link)?;
            let record = inner
                .federation
                .prompt_permissions
                .as_ref()
                .and_then(|policy| policy.find(&record_id))
                .cloned()
                .ok_or_else(|| ApiError::not_found("prompt permission not found"))?;
            if !record.revoked || record.issuer != *local {
                return Err(federation_stale_link());
            }
            Ok(record)
        })
        .await?;
        let remote = match record.kind {
            FederationPromptPermissionKind::CallerGrant => &record.scope.target_host,
            FederationPromptPermissionKind::DestinationConsent => &record.scope.source_host,
        };
        let handle = self
            .federation_peer_handle(&remote.host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = handle.channel.current_lease()?;
        let observed = self
            .read_federation_prompt_policy(
                &handle.rpc,
                &lease,
                FederationPromptPolicyOperation::ObserveRevocation {
                    permission: record.clone(),
                },
            )
            .await?;
        let app = self.clone();
        let id = id.to_owned();
        run_blocking_api(move || {
            app.mutate_federation_core_on_link(
                Some(&lease),
                false,
                |domain, _, _| {
                    observed.ensure_fresh_at(federation_prompt_observation_now(
                        &lease,
                        "promptPolicyAckMutation",
                    ))?;
                    let policy = domain
                        .prompt_permissions
                        .as_mut()
                        .ok_or_else(federation_stale_link)?;
                    if policy.find(&id) != Some(&record) {
                        return Err(federation_stale_link());
                    }
                    policy.retain(observed.permission)
                },
                |_| {},
                EngramBudgetClock::Real,
            )
        })
        .await
    }
}
