// Owns bounded authenticated policy observations on existing pair transport.
// Does not mount the production route or deliver prompts. Recovered from the policy component.
// Authenticated permission/native observations inside the existing executor
// service. This endpoint cannot create a session, binding or queued prompt.
const FEDERATION_PROMPT_POLICY_ROUTE: &str = "/federation/v1/executor/prompt-policy";
const FEDERATION_PROMPT_POLICY_REQUEST_BYTES: usize = 32_768;
const FEDERATION_PROMPT_OBSERVATION_LIFETIME: Duration = Duration::from_secs(30);

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_policy_unknown_outcome() -> ApiError {
    ApiError::from_status(
        StatusCode::SERVICE_UNAVAILABLE,
        "prompt policy outcome is unknown; retry the original operation with the same creationKey or permission identity",
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptCapabilities {
    protocol_version: u16,
    enqueue_text_prompt: u16,
    text_bytes: u64,
    request_bytes: u64,
    metadata_bytes: u64,
    response_bytes: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptCapabilities {
    fn local() -> Self {
        Self {
            protocol_version: 1,
            enqueue_text_prompt: 1,
            text_bytes: 65_536,
            request_bytes: 524_288,
            metadata_bytes: 32_768,
            response_bytes: 16_384,
        }
    }
    fn validate(&self) -> Result<(), ApiError> {
        if self != &Self::local() {
            return Err(ApiError::conflict(
                "enqueueTextPrompt capability or bounds are unsupported",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPolicyRequest {
    protocol_version: u16,
    request_id: String,
    operation: FederationPromptPolicyOperation,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
enum FederationPromptPolicyOperation {
    DestinationProof {
        scope: FederationPromptScope,
        consent: FederationPromptPermissionIdentity,
    },
    CallerGrant {
        scope: FederationPromptScope,
        grant: FederationPromptPermissionIdentity,
    },
    ObserveRevocation {
        permission: FederationPromptPermission,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPolicyReply {
    protocol_version: u16,
    request_id: String,
    link_incarnation: String,
    nonce: String,
    expires_at: u64,
    capabilities: FederationPromptCapabilities,
    permission: FederationPromptPermission,
    // Requester-local timing only; never accepted from or serialized onto wire.
    #[serde(skip)]
    observed_until: Option<std::time::Instant>,
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_now() -> Result<u64, ApiError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| federation_stale_link())
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationPromptPolicyRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.protocol_version != 1 {
            return Err(ApiError::bad_request("unsupported prompt policy protocol"));
        }
        federation_prompt_uuid(&self.request_id)?;
        match &self.operation {
            FederationPromptPolicyOperation::DestinationProof { scope, consent } => {
                scope.validate()?;
                consent.validate()?;
            }
            FederationPromptPolicyOperation::CallerGrant { scope, grant } => {
                scope.validate()?;
                grant.validate()?;
            }
            FederationPromptPolicyOperation::ObserveRevocation { permission } => {
                permission.validate()?;
                if !permission.revoked {
                    return Err(ApiError::bad_request(
                        "only committed revocation may be observed",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_policy_routes() -> Router<FederationListenerState> {
    // Foundation only: production listener registration waits for the complete
    // delivery vertical. Isolated policy fixtures mount this router directly.
    Router::new()
        .route(
            FEDERATION_PROMPT_POLICY_ROUTE,
            post(serve_federation_prompt_policy),
        )
        .layer(DefaultBodyLimit::max(
            FEDERATION_PROMPT_POLICY_REQUEST_BYTES,
        ))
}

#[cfg_attr(not(test), allow(dead_code))]
async fn serve_federation_prompt_policy(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    if bytes.len() > FEDERATION_PROMPT_POLICY_REQUEST_BYTES {
        return Err(ApiError::from_status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "prompt policy metadata exceeds bound",
        ));
    }
    let request: FederationPromptPolicyRequest = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("invalid prompt policy request"))?;
    request.validate()?;
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease
        .channel
        .app
        .clone()
        .ok_or_else(federation_transport_unavailable)?;
    let operation = request.operation;
    let permit = peer
        .mailbox_permit
        .ok_or_else(federation_transport_unavailable)?;
    let worker_lease = lease.clone();
    let permission = run_blocking_api(move || {
        let _permit = permit;
        worker_lease
            .channel
            .control
            .observe("promptPolicyWorkerEntered", 0);
        let permission = match operation {
            FederationPromptPolicyOperation::DestinationProof { scope, consent } => worker_lease
                .with_current(&app, |inner, authority| {
                    if authority.local_role != FederationRole::Satellite
                        || authority.peer != scope.source_host
                        || authority.local != scope.target_host
                        || authority.enrollment_generation != scope.enrollment_generation
                    {
                        return Err(federation_stale_link());
                    }
                    federation_prompt_scope_current_locked(
                        inner,
                        &scope,
                        FederationRole::Satellite,
                    )?;
                    federation_prompt_permission_current_locked(
                        inner,
                        FederationPromptPermissionKind::DestinationConsent,
                        &consent,
                        &scope,
                    )
                    .cloned()
                })?,
            FederationPromptPolicyOperation::CallerGrant { scope, grant } => worker_lease
                .with_current(&app, |inner, authority| {
                    if authority.local_role != FederationRole::Central
                        || authority.peer != scope.target_host
                        || authority.local != scope.source_host
                        || authority.enrollment_generation != scope.enrollment_generation
                    {
                        return Err(federation_stale_link());
                    }
                    federation_prompt_scope_current_locked(inner, &scope, FederationRole::Central)?;
                    federation_prompt_permission_current_locked(
                        inner,
                        FederationPromptPermissionKind::CallerGrant,
                        &grant,
                        &scope,
                    )
                    .cloned()
                })?,
            FederationPromptPolicyOperation::ObserveRevocation { permission } => {
                app.observe_federation_prompt_revocation(&worker_lease, permission)?
            }
        };
        Ok(permission)
    })
    .await?;
    let reply = FederationPromptPolicyReply {
        protocol_version: 1,
        request_id: request.request_id,
        link_incarnation: lease.incarnation.clone(),
        nonce: Uuid::new_v4().to_string(),
        expires_at: federation_prompt_now()?
            .checked_add(30)
            .ok_or_else(federation_stale_link)?,
        capabilities: FederationPromptCapabilities::local(),
        permission,
        observed_until: None,
    };
    let bytes = serde_json::to_vec(&reply).map_err(|_| federation_transport_unavailable())?;
    if bytes.len() > REMOTE_PROMPT_STATUS_BYTES {
        return Err(federation_transport_unavailable());
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn observe_federation_prompt_revocation(
        &self,
        lease: &FederationLinkLease,
        mut record: FederationPromptPermission,
    ) -> Result<FederationPromptPermission, ApiError> {
        record.validate()?;
        if !record.revoked {
            return Err(ApiError::bad_request("prompt permission is not revoked"));
        }
        let receiver_role = match record.kind {
            FederationPromptPermissionKind::CallerGrant => FederationRole::Satellite,
            FederationPromptPermissionKind::DestinationConsent => FederationRole::Central,
        };
        federation_prompt_link_scope(lease, &record.scope, receiver_role)?;
        self.mutate_federation_core_on_link(
            Some(lease),
            false,
            |domain, settings, _| {
                let (role, local, remote) = match record.kind {
                    FederationPromptPermissionKind::CallerGrant => (
                        FederationRole::Satellite,
                        &record.scope.target_host,
                        &record.scope.source_host,
                    ),
                    FederationPromptPermissionKind::DestinationConsent => (
                        FederationRole::Central,
                        &record.scope.source_host,
                        &record.scope.target_host,
                    ),
                };
                let peer = domain
                    .peers
                    .get(&remote.host_id)
                    .ok_or_else(federation_stale_link)?;
                if settings.role != role
                    || settings.central.as_ref() != Some(&record.scope.source_host)
                    || domain.local.as_ref() != Some(local)
                    || &record.issuer != remote
                    || &peer.identity != remote
                    || peer.enrollment_generation != record.scope.enrollment_generation
                    || peer.membership == FederationMembershipState::Revoked
                {
                    return Err(federation_stale_link());
                }
                // This host has now observed this exact revocation; the issuer
                // learns remote effectiveness only from the authenticated reply.
                record.receiver_observed_generation = Some(record.identity.generation);
                domain
                    .prompt_permissions
                    .get_or_insert_with(FederationPromptPermissions::default)
                    .retain(record)
            },
            |_| {},
            EngramBudgetClock::Real,
        )
    }

    async fn read_federation_prompt_policy(
        &self,
        rpc: &FederationRpcClient,
        lease: &FederationLinkLease,
        operation: FederationPromptPolicyOperation,
    ) -> Result<FederationPromptPolicyReply, ApiError> {
        if !Arc::ptr_eq(&rpc.channel.state, &lease.channel.state) {
            return Err(federation_stale_link());
        }
        let request = FederationPromptPolicyRequest {
            protocol_version: 1,
            request_id: Uuid::new_v4().to_string(),
            operation,
        };
        request.validate()?;
        // Start before transport, so a delayed reply cannot reset its lifetime.
        let observation_deadline =
            federation_prompt_observation_now(lease, "promptPolicyObservationStarted")
                .checked_add(FEDERATION_PROMPT_OBSERVATION_LIFETIME)
                .ok_or_else(federation_stale_link)?;
        let reply: FederationPromptPolicyReply = rpc
            .request(FederationRpcRoute::PromptPolicy(
                lease.clone(),
                request.clone(),
            ))
            .await?;
        let reply = reply.observe(
            &request.request_id,
            &lease.incarnation,
            observation_deadline,
            lease.channel.control.clock.now(),
        )?;
        lease
            .channel
            .control
            .observe("promptPolicyObservationReady", 0);
        match &request.operation {
            FederationPromptPolicyOperation::DestinationProof { scope, consent } => {
                federation_prompt_link_scope(lease, scope, FederationRole::Central)?;
                if reply.permission.kind != FederationPromptPermissionKind::DestinationConsent
                    || reply.permission.scope != *scope
                    || reply.permission.identity != *consent
                    || reply.permission.revoked
                {
                    return Err(federation_stale_link());
                }
            }
            FederationPromptPolicyOperation::CallerGrant { scope, grant } => {
                federation_prompt_link_scope(lease, scope, FederationRole::Satellite)?;
                if reply.permission.kind != FederationPromptPermissionKind::CallerGrant
                    || reply.permission.scope != *scope
                    || reply.permission.identity != *grant
                    || reply.permission.revoked
                {
                    return Err(federation_stale_link());
                }
            }
            FederationPromptPolicyOperation::ObserveRevocation { permission } => {
                let role = match permission.kind {
                    FederationPromptPermissionKind::CallerGrant => FederationRole::Central,
                    FederationPromptPermissionKind::DestinationConsent => FederationRole::Satellite,
                };
                federation_prompt_link_scope(lease, &permission.scope, role)?;
                if !reply.permission.same_permission(permission)
                    || reply.permission.identity != permission.identity
                    || !reply.permission.revoked
                    || reply.permission.receiver_observed_generation
                        != Some(permission.identity.generation)
                {
                    return Err(federation_stale_link());
                }
            }
        }
        self.read_federation_prompt_current(lease, |_, _| Ok(()))
            .await?;
        Ok(reply)
    }

    async fn issue_federation_prompt_grant(
        &self,
        expected_version: u64,
        creation_key: String,
        scope: FederationPromptScope,
        consent: FederationPromptPermissionIdentity,
    ) -> Result<FederationPromptPermission, ApiError> {
        scope.validate()?;
        consent.validate()?;
        validate_federation_reference(&creation_key)?;
        if let Some(retained) = self.read_federation_prompt_grant_replay(
            &creation_key, &scope, &consent,
        ).await? {
            return Ok(retained);
        }
        let handle = self
            .federation_peer_handle(&scope.target_host.host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = handle.channel.current_lease()?;
        self.issue_federation_prompt_grant_on_link(
            expected_version,
            creation_key,
            scope,
            consent,
            &lease,
            &handle.rpc,
        )
        .await
    }

    async fn issue_federation_prompt_grant_on_link(
        &self,
        expected_version: u64,
        creation_key: String,
        scope: FederationPromptScope,
        consent: FederationPromptPermissionIdentity,
        lease: &FederationLinkLease,
        rpc: &FederationRpcClient,
    ) -> Result<FederationPromptPermission, ApiError> {
        scope.validate()?;
        consent.validate()?;
        validate_federation_reference(&creation_key)?;
        if let Some(retained) = self.read_federation_prompt_grant_replay(
            &creation_key, &scope, &consent,
        ).await? {
            return Ok(retained);
        }
        federation_prompt_link_scope(lease, &scope, FederationRole::Central)?;
        let captured_scope = scope.clone();
        self.read_federation_prompt_current(lease, move |inner, _| {
            federation_prompt_scope_current_locked(inner, &captured_scope, FederationRole::Central)
        })
        .await?;
        let proof = self
            .read_federation_prompt_policy(
                rpc,
                &lease,
                FederationPromptPolicyOperation::DestinationProof {
                    scope: scope.clone(),
                    consent: consent.clone(),
                },
            )
            .await?;
        let app = self.clone();
        let lease = lease.clone();
        run_blocking_api(move || {
            app.mutate_federation_permission(
                Some(&lease),
                |domain, settings, context| {
                    if let Some(retained) = federation_prompt_creation_replay(
                        domain, settings, FederationPromptPermissionKind::CallerGrant,
                        &creation_key, &scope, Some(&consent),
                    )? {
                        return Ok(retained);
                    }
                    if domain.version != expected_version {
                        return Err(ApiError::conflict("federation policy version changed"));
                    }
                    federation_prompt_scope_current_locked(
                        context.native_state,
                        &scope,
                        FederationRole::Central,
                    )?;
                    if settings.role != FederationRole::Central {
                        return Err(federation_stale_link());
                    }
                    proof.ensure_fresh_at(federation_prompt_observation_now(
                        &lease,
                        "promptPolicyGrantMutation",
                    ))?;
                    let policy = domain
                        .prompt_permissions
                        .get_or_insert_with(FederationPromptPermissions::default);
                    policy.retain(proof.permission.clone())?;
                    policy.retain(FederationPromptPermission {
                        kind: FederationPromptPermissionKind::CallerGrant,
                        identity: FederationPromptPermissionIdentity {
                            id: Uuid::new_v4().to_string(),
                            generation: 1,
                        },
                        issuer: scope.source_host.clone(),
                        scope: scope.clone(),
                        creation_key: creation_key.clone(),
                        consent: Some(consent.clone()),
                        revoked: false,
                        receiver_observed_generation: None,
                    })
                },
            )
        })
        .await
    }
}
