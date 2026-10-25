// Owns authenticated metadata-only probe routes on L1's domain listener.
// Does not expose local APIs or accept commands/prompts. New O1 routes composed
// into federation_listener.rs before its pair/authority/lease middleware.

#[derive(Clone)]
struct FederationOnboardingHost {
    app: AppState,
    rpc: FederationRpcClient,
    io: FederationOnboardingIo,
}

fn federation_onboarding_routes() -> Router<FederationListenerState> {
    Router::new()
        .route(
            "/federation/v1/onboarding/identity",
            get(federation_probe_identity),
        )
        .route(
            "/federation/v1/onboarding/reverse",
            post(federation_probe_reverse),
        )
        .route(
            "/federation/v1/onboarding/probe",
            post(federation_probe_prepare),
        )
        .route(
            "/federation/v1/onboarding/wake",
            post(federation_probe_wake),
        )
        .route(
            "/federation/v1/onboarding/engram",
            get(federation_probe_engram),
        )
}

fn federation_onboarding_context(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    peer: FederationAuthenticatedPeer,
) -> Result<(FederationOnboardingHost, FederationLinkLease), ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let host = host.ok_or_else(federation_transport_unavailable)?.0;
    lease.with_current(&host.app, |_, _| Ok(()))?;
    Ok((host, lease))
}

async fn federation_probe_identity(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
) -> Result<Json<FederationOnboardingIdentity>, ApiError> {
    let (host, lease) = federation_onboarding_context(host, peer)?;
    let measure = host.io.profile.clone();
    let observe = host.io.image.clone();
    let (profile, running_image) =
        tokio::task::spawn_blocking(move || Ok::<_, ApiError>((measure()?, observe())))
            .await
            .map_err(|_| federation_transport_unavailable())??;
    let identity = lease.with_current(&host.app, |_, current| {
        Ok(FederationOnboardingIdentity {
            host: current.local.clone(),
            central: current.central.clone(),
            enrollment_generation: current.enrollment_generation,
            link_incarnation: lease.incarnation.clone(),
            platform: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            profile,
            running_image,
        })
    })?;
    Ok(Json(identity))
}

async fn federation_probe_reverse(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationProbeRequest>,
) -> Result<Json<FederationReverseProbe>, ApiError> {
    let (host, lease) = federation_onboarding_context(host, peer)?;
    validate_federation_probe_id(&request.attempt_id)?;
    if lease.channel.snapshot.local_role != FederationRole::Satellite {
        return Err(ApiError::bad_request(
            "only the satellite performs the reverse probe",
        ));
    }
    let identity: FederationOnboardingIdentity = host
        .rpc
        .request(FederationRpcRoute::Onboarding(
            lease.clone(),
            FederationOnboardingRpc::Identity,
        ))
        .await?;
    lease.with_current(&host.app, |inner, current| {
        if identity.host != current.central
            || identity.central != current.central
            || identity.enrollment_generation != current.enrollment_generation
            || identity.link_incarnation != lease.incarnation
            || inner
                .federation
                .peers
                .get(&current.peer.host_id)
                .is_none_or(|record| !record.profile.compatible_with(&identity.profile))
            || identity.profile.protocol_version != u32::from(FEDERATION_PROTOCOL_VERSION)
            || identity.profile.schema_version != FEDERATION_PROFILE_SCHEMA_VERSION
        {
            return Err(federation_stale_link());
        }
        Ok(())
    })?;
    Ok(Json(FederationReverseProbe {
        identity,
        attempt_id: request.attempt_id,
    }))
}

async fn federation_probe_prepare(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationProbeRequest>,
) -> Result<Json<FederationProbeSession>, ApiError> {
    let (host, lease) = federation_onboarding_context(host, peer)?;
    let expected = lease.with_current(&host.app, |_, current| Ok(current.clone()))?;
    let incarnation = lease.incarnation.clone();
    tokio::task::spawn_blocking(move || {
        host.app
            .prepare_federation_probe(&expected, &incarnation, &request.attempt_id)
    })
    .await
    .map_err(|_| federation_transport_unavailable())?
    .map(Json)
}

async fn federation_probe_wake(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<FederationProbeWakeRequest>,
) -> Result<Json<FederationProbeWakeOutcome>, ApiError> {
    let (host, lease) = federation_onboarding_context(host, peer)?;
    let expected = lease.with_current(&host.app, |_, current| Ok(current.clone()))?;
    let incarnation = lease.incarnation.clone();
    // The owned task survives an HTTP cancellation. Acceptance is persisted
    // before the child runs; duplicate/unknown wakes never launch another child.
    tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let (mut outcome, fresh) =
                host.app
                    .accept_federation_probe_wake(&expected, &incarnation, &request)?;
            if !fresh {
                return Ok(Json(outcome));
            }
            // Recheck immediately before local runtime apply, after the
            // acceptance durability wait. A stale admitted wake remains
            // visibly unresolved and cannot start a replacement probe.
            lease.with_current(&host.app, |_, current| {
                if current != &expected {
                    return Err(federation_stale_link());
                }
                Ok(())
            })?;
            match (host.io.platform)() {
                Ok(platform) => {
                    outcome.platform = Some(platform);
                    outcome.state = FederationProbeWakeState::Delivered;
                }
                Err(_) => {
                    outcome.state = FederationProbeWakeState::Failed;
                }
            }
            host.app
                .finish_federation_probe_wake(&expected, &incarnation, outcome.clone())?;
            Ok(Json(outcome))
        })
        .await
        .map_err(|_| federation_transport_unavailable())?
    })
    .await
    .map_err(|_| federation_transport_unavailable())?
}

async fn federation_probe_engram(
    host: Option<axum::Extension<FederationOnboardingHost>>,
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
) -> Result<Json<FederationEngramOnboardingOutcome>, ApiError> {
    let (host, lease) = federation_onboarding_context(host, peer)?;
    let command = lease.with_current(&host.app, |inner, current| {
        if !inner
            .federation
            .onboarding
            .engram_enabled
            .contains(&current.peer.host_id)
        {
            return Err(ApiError::conflict(
                "Engram sync is not enabled for this host profile",
            ));
        }
        Ok(inner
            .federation
            .onboarding
            .engram_commands
            .get(&current.peer.host_id)
            .cloned())
    })?;
    let Some(command) = command else {
        return Ok(Json(FederationEngramOnboardingOutcome::unavailable()));
    };
    let verify = host.io.engram.clone();
    let result = tokio::task::spawn_blocking(move || verify(&command))
        .await
        .map_err(|_| federation_transport_unavailable())?;
    Ok(Json(result))
}
