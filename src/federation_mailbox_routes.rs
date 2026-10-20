// Owns the local agent/API facade and authenticated domain-only mailbox route.
// Does not persist mailbox data on a satellite or automatically retry mutations.
// New routing beside mailboxes.rs; the bridge/CLI still use their own host.

async fn negotiate_federation_mailboxes(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<FederationMailboxProtocol>, ApiError> {
    state
        .federation_mailbox_operation(&session, FederationMailboxOperation::Protocol)
        .await
        .map(Json)
}

async fn join_federation_mailboxes(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<FederationMailboxParticipant>, ApiError> {
    run_blocking_api(move || state.join_federation_mailbox_session(&session))
        .await
        .map(Json)
}

async fn list_federation_mailbox_participants(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<FederationMailboxParticipant>>, ApiError> {
    state
        .federation_mailbox_operation(&session, FederationMailboxOperation::Directory)
        .await
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateFederationMailboxRequest {
    target_session_id: String,
}

async fn get_or_create_federation_mailbox(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<CreateFederationMailboxRequest>,
) -> Result<Json<MailboxSummary>, ApiError> {
    if !matches!(mailbox_reference_handle(&request.target_session_id), Ok(None)) {
        return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
    }
    state
        .federation_mailbox_operation(
            &session,
            FederationMailboxOperation::GetOrCreate {
                target: request.target_session_id,
            },
        )
        .await
        .map(Json)
}

impl AppState {
    async fn federation_mailbox_operation<T: DeserializeOwned>(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
    ) -> Result<T, ApiError> {
        if let FederationMailboxOperation::Send { target, .. }
            | FederationMailboxOperation::GetOrCreate { target } = &operation
        {
            if !matches!(mailbox_reference_handle(target), Ok(None)) {
                return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
            }
        }
        if let FederationMailboxOperation::Send { .. } = &operation {
            let delivery = self.prepare_federation_delivery(session, operation).await?;
            let value = self.send_prepared_federation_delivery(session, delivery).await?;
            return serde_json::from_value(serde_json::to_value(value)
                .map_err(|_| ApiError::internal("delivery response encoding failed"))?)
                .map_err(|_| ApiError::bad_gateway("invalid delivery response schema"));
        }
        self.dispatch_federation_mailbox_operation(session, operation).await
    }

    async fn dispatch_federation_mailbox_operation<T: DeserializeOwned>(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
    ) -> Result<T, ApiError> {
        if matches!(&operation, FederationMailboxOperation::SendExact { .. }
            | FederationMailboxOperation::CancelExact { .. }) {
            let capability = self.negotiate_federation_delivery_capability(session).await?;
            if !capability.supported {
                let delivery = match &operation {
                    FederationMailboxOperation::SendExact { delivery }
                    | FederationMailboxOperation::CancelExact { delivery } => delivery,
                    _ => unreachable!("exact operation checked above"),
                };
                return Err(self.refuse_unsupported_federation_delivery(session, delivery, capability).await?);
            }
            return self.dispatch_federation_delivery_operation(session, operation, &capability).await;
        }
        self.dispatch_federation_mailbox_operation_with_capability(session, operation, None)
            .await.map(|(value, _)| value)
    }

    // Return the lease that authenticated this response, rather than selecting
    // a second link when a caller later uses negotiated capability evidence.
    async fn dispatch_federation_mailbox_operation_with_capability<T: DeserializeOwned>(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
        capability: Option<&FederationDeliveryCapability>,
    ) -> Result<(T, Option<FederationLinkLease>), ApiError> {
        let (settings, actor) = {
            let actor = self.federation_mailbox_actor(session)?;
            let inner = self.inner.lock().expect("state mutex poisoned");
            (inner.preferences.federation.clone(), actor)
        };
        if !settings.enabled || settings.role == FederationRole::Standalone {
            return Err(ApiError::not_found(
                "domain mailbox participation is disabled",
            ));
        }
        if actor.is_none() && !matches!(operation, FederationMailboxOperation::Protocol) {
            return Err(ApiError::conflict(
                "session has no explicit central mailbox binding",
            ));
        }
        let central = settings
            .central
            .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
        if capability.is_some_and(|proof| proof.central != central
            || (proof.lease.is_some() != (settings.role == FederationRole::Satellite))) {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        let expected = actor.as_ref().map(|actor| actor.binding.clone());
        let acknowledged_mailbox = match &operation {
            FederationMailboxOperation::Acknowledge { mailbox_id, .. } => Some(mailbox_id.clone()),
            _ => None,
        };
        let request = FederationMailboxRequest {
            protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
            schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
            central: central.clone(),
            actor,
            operation,
        };
        let (response, lease): (FederationMailboxResponse, _) = if settings.role == FederationRole::Central {
            let app = self.clone();
            (run_blocking_api(move || app.execute_federation_mailbox(request, None)).await?, None)
        } else {
            let peer = self
                .federation_peer_handle(&central.host_id)
                .ok_or_else(federation_mailbox_unavailable)?;
            let lease = if let Some(proof) = capability {
                let lease = proof.lease.as_ref()
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                if !Arc::ptr_eq(&lease.channel.state, &peer.channel.state) {
                    return Err(federation_stale_link());
                }
                lease.with_current(self, |_, _| Ok(()))?;
                lease.clone()
            } else {
                peer.channel.current_lease().map_err(|_| federation_mailbox_unavailable())?
            };
            let response = peer.rpc
                .request(FederationRpcRoute::Mailbox(lease.clone(), request)).await?;
            (response, Some(lease))
        };
        if response.protocol_version != FEDERATION_MAILBOX_WIRE_VERSION
            || response.schema_version != FEDERATION_MAILBOX_SCHEMA_VERSION
            || response.central != central
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        // A delayed reply cannot be attached to a recreated local session or
        // to a binding replaced while the off-lock transport was in flight.
        if self
            .federation_mailbox_actor(session)?
            .map(|actor| actor.binding)
            != expected
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        if let (Some(mailbox), Some(binding), Some(lease)) =
            (acknowledged_mailbox, expected, lease.as_ref())
        {
            let summary: MailboxSummary = serde_json::from_value(response.value.clone())
                .map_err(|_| ApiError::bad_gateway("invalid central ACK response"))?;
            if summary.id != mailbox { return Err(ApiError::conflict("central ACK mailbox mismatch")); }
            let app = self.clone();
            let lease = lease.clone();
            run_blocking_api(move || app.retain_federation_executor_mailbox_ack(&binding, &summary, &lease)).await?;
        }
        serde_json::from_value(response.value)
            .map(|value| (value, lease))
            .map_err(|_| ApiError::bad_gateway("invalid central mailbox response schema"))
    }
}

fn federation_mailbox_routes() -> Router<FederationListenerState> {
    Router::new().route(
        "/federation/v1/mailbox",
        post(serve_federation_mailbox)
            .layer(DefaultBodyLimit::max(FEDERATION_MAILBOX_REQUEST_BYTES)),
    )
}

async fn serve_federation_mailbox(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationMailboxRequest>,
) -> Result<Json<FederationMailboxResponse>, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let permit = peer
        .mailbox_permit
        .ok_or_else(federation_mailbox_unavailable)?;
    let app = lease
        .channel
        .app
        .clone()
        .ok_or_else(federation_mailbox_unavailable)?;
    run_blocking_api(move || {
        let _permit = permit;
        lease.with_current(&app, |inner, _| {
            federation_mailbox_central_locked(inner).map(|_| ())
        })?;
        let response = app.execute_federation_mailbox(request, Some(lease.clone()))?;
        lease.channel.control.observe("mailboxExecuted", 0);
        let current = lease.with_current(&app, |inner, _| {
            federation_mailbox_central_locked(inner).map(|_| ())
        });
        lease
            .channel
            .control
            .observe("mailboxPostcheck", u64::from(current.is_err()));
        // Execution has already returned a receipt. A failure observed here
        // cannot prove refusal, even if authority settles before middleware
        // revalidation. Preserve same-key recovery at this exact boundary.
        current.map_err(|_| {
            federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "mailbox authority changed after execution",
            )
        })?;
        Ok(response)
    })
    .await
    .map(Json)
}
