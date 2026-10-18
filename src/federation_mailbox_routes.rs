// Owns the local agent/API facade and authenticated domain-only mailbox route.
// Does not persist mailbox data on a satellite or retry application mutations.
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
        let expected = actor.as_ref().map(|actor| actor.binding.clone());
        let request = FederationMailboxRequest {
            protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
            schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
            central: central.clone(),
            actor,
            operation,
        };
        let response: FederationMailboxResponse = if settings.role == FederationRole::Central {
            let app = self.clone();
            run_blocking_api(move || app.execute_federation_mailbox(request, None)).await?
        } else {
            let peer = self
                .federation_peer_handle(&central.host_id)
                .ok_or_else(federation_mailbox_unavailable)?;
            let lease = peer
                .channel
                .current_lease()
                .map_err(|_| federation_mailbox_unavailable())?;
            peer.rpc
                .request(FederationRpcRoute::Mailbox(lease, request))
                .await?
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
        serde_json::from_value(response.value)
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
        lease.with_current(&app, |inner, _| {
            federation_mailbox_central_locked(inner).map(|_| ())
        })?;
        Ok(response)
    })
    .await
    .map(Json)
}
