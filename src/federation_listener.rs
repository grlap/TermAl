// Owns the per-pair loopback router and authenticated caller context.
// Does not mount local APIs or grant mailbox/session authority. New, not a split.

use super::*;

pub(crate) type FederationAuthorityCheck = Arc<dyn Fn() -> Result<(), ApiError> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct FederationListenerState {
    pub(crate) channel: FederationChannel,
    pub(crate) ensure_current: FederationAuthorityCheck,
}

#[derive(Clone)]
pub(crate) struct FederationAuthenticatedPeer {
    // Selected exclusively by the dedicated listener, never by a request id.
    pub(crate) lease: Option<FederationLinkLease>,
    // A canceled mailbox handler retains its slot on the owned blocking job.
    pub(crate) mailbox_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
}

pub(crate) fn federation_router(
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
) -> Router {
    let state = FederationListenerState {
        channel,
        ensure_current,
    };
    Router::new()
        .route("/federation/v1/health", get(federation_health))
        .route(
            "/federation/v1/host-utilization",
            get(host_utilization::serve_pair),
        )
        .route(
            "/federation/v1/mailbox/native-binding",
            post(serve_federation_native_binding),
        )
        .route(
            "/federation/v1/mailbox/binding",
            post(serve_federation_binding_command),
        )
        .route(
            "/federation/v1/mailbox/participation",
            post(serve_federation_participation),
        )
        .merge(federation_onboarding_routes())
        .layer(DefaultBodyLimit::max(4096))
        .merge(federation_mailbox_routes())
        .merge(mailbox_relay_rpc_routes())
        .merge(federation_executor_routes())
        .merge(federation_executor_outcome_routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            federation_authenticate,
        ))
        .with_state(state)
        .layer(axum::middleware::from_fn(output_limit_http_guard))
        .layer(axum::middleware::from_fn(output_size_http_observer))
}

pub(crate) async fn federation_authenticate(
    State(state): State<FederationListenerState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    if !request.uri().path().starts_with("/federation/v1/") {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    federation_authenticate_message(state, request, next).await
}
pub(crate) async fn federation_health(
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
