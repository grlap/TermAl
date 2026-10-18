// Owns the per-pair loopback router and authenticated caller context.
// Does not mount local APIs or grant mailbox/session authority. New, not a split.

type FederationAuthorityCheck = Arc<dyn Fn() -> Result<(), ApiError> + Send + Sync>;

#[derive(Clone)]
struct FederationListenerState {
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
}

#[derive(Clone)]
struct FederationAuthenticatedPeer {
    // Selected exclusively by the dedicated listener, never by a request id.
    lease: Option<FederationLinkLease>,
}

fn federation_router(
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
) -> Router {
    let state = FederationListenerState {
        channel,
        ensure_current,
    };
    Router::new()
        .route("/federation/v1/hello", get(federation_hello))
        .route("/federation/v1/link", post(federation_open_link))
        .route("/federation/v1/health", get(federation_health))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            federation_authenticate,
        ))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(state)
}

async fn federation_authenticate(
    State(state): State<FederationListenerState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    let capabilities = request.headers().get_all(FEDERATION_CAPABILITY_HEADER);
    let mut supplied = capabilities.iter();
    let valid = supplied
        .next()
        .is_some_and(|value| state.channel.capability.matches(value.as_bytes()))
        && supplied.next().is_none();
    if !valid {
        return Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "federation pair authentication failed",
        ));
    }
    let mut authority_headers = request
        .headers()
        .get_all(FEDERATION_AUTHORITY_HEADER)
        .iter();
    let raw_authority = authority_headers
        .next()
        .filter(|value| value.as_bytes().len() <= 1024)
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    if authority_headers.next().is_some() {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let authority: FederationRequestAuthority = serde_json::from_slice(raw_authority.as_bytes())
        .map_err(|_| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    authority.ensure_expected(&state.channel.snapshot)?;
    (state.ensure_current)()?;
    let _permit = state.channel.bound.try_acquire()?;
    let mut incarnations = request
        .headers()
        .get_all(FEDERATION_INCARNATION_HEADER)
        .iter();
    let incarnation = incarnations
        .next()
        .and_then(|header| header.to_str().ok())
        .ok_or_else(federation_stale_link)?;
    if incarnations.next().is_some() || Uuid::parse_str(incarnation).is_err() {
        return Err(federation_stale_link());
    }
    let request_incarnation = incarnation.to_owned();
    let lease = if matches!(
        request.uri().path(),
        "/federation/v1/hello" | "/federation/v1/link"
    ) {
        None
    } else {
        Some(state.channel.lease(incarnation)?)
    };
    request
        .extensions_mut()
        .insert(FederationAuthenticatedPeer {
            lease: lease.clone(),
        });
    let response = match tokio::time::timeout(Duration::from_secs(5), next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            state.channel.invalidate_if_current(&FederationLinkLease {
                channel: state.channel.clone(),
                incarnation: request_incarnation,
            });
            let mut response = StatusCode::REQUEST_TIMEOUT.into_response();
            response.headers_mut().insert(
                axum::http::header::CONNECTION,
                HeaderValue::from_static("close"),
            );
            return Ok(response);
        }
    };
    (state.ensure_current)()?;
    if let Some(lease) = lease {
        lease.with_incarnation(|| Ok(()))?;
    }
    Ok(response)
}

async fn federation_hello(
    State(state): State<FederationListenerState>,
) -> Result<Json<FederationHello>, ApiError> {
    (state.ensure_current)()?;
    if state.channel.snapshot.local_role != FederationRole::Satellite {
        return Err(federation_stale_link());
    }
    Ok(Json(state.channel.challenge()))
}

async fn federation_open_link(
    State(state): State<FederationListenerState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<FederationOpenLink>,
) -> Result<Json<FederationHealth>, ApiError> {
    let _opening = state.channel.opening.try_lock().map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "federation link establishment already in progress",
        )
    })?;
    (state.ensure_current)()?;
    if headers
        .get(FEDERATION_INCARNATION_HEADER)
        .and_then(|header| header.to_str().ok())
        != Some(request.incarnation.as_str())
    {
        return Err(federation_stale_link());
    }
    let lease = state.channel.accept_link(&request)?;
    if let Err(error) = state.channel.record_connected(&lease).await {
        state.channel.invalidate();
        return Err(error);
    }
    federation_health_for(&lease).map(Json)
}

async fn federation_health(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
) -> Result<Json<FederationHealth>, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    federation_health_for(&lease).map(Json)
}

fn federation_health_for(lease: &FederationLinkLease) -> Result<FederationHealth, ApiError> {
    lease.with_incarnation(|| {
        Ok(FederationHealth {
            protocol_version: FEDERATION_PROTOCOL_VERSION,
            host_id: lease.channel.snapshot.local.host_id.clone(),
            host_generation: lease.channel.snapshot.local.host_generation,
            enrollment_generation: lease.channel.snapshot.enrollment_generation,
            link_incarnation: lease.incarnation.clone(),
        })
    })
}
