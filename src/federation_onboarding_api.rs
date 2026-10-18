// Owns operator-local enrollment, verification and diagnostic API entrypoints.
// Does not expose control actions through L1 or activate satellites implicitly.
// New O1 handlers composed into main.rs's existing local API router.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationEnrollmentRequest {
    record: FederationPeerRecord,
    #[serde(default)]
    engram_enabled: bool,
    engram_command: Option<FederationEngramProbeCommand>,
}

async fn enroll_federation_satellite(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<FederationEnrollmentRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.record.identity.host_id != id {
        return Err(ApiError::bad_request(
            "enrollment host identity does not match the route",
        ));
    }
    if let Some(command) = &request.engram_command {
        command.validate()?;
    }
    tokio::task::spawn_blocking(move || {
        app.enroll_federation_peer_with(request.record, |domain, installed| {
            retire_federation_probe(domain, &installed.identity.host_id)?;
            domain.onboarding.reports.remove(&id);
            if request.engram_enabled {
                domain.onboarding.engram_enabled.insert(id.clone());
            } else {
                domain.onboarding.engram_enabled.remove(&id);
            }
            if let Some(command) = request.engram_command {
                domain
                    .onboarding
                    .engram_commands
                    .insert(id.clone(), command);
            } else {
                domain.onboarding.engram_commands.remove(&id);
            }
            Ok(())
        })?;
        app.federation_registry_view().map(Json)
    })
    .await
    .map_err(|_| federation_transport_unavailable())?
}

async fn verify_federation_satellite_route(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<FederationOnboardingReport>, ApiError> {
    // Cancellation must not silently lose the final report after a remote
    // probe wake has been durably admitted.
    tokio::spawn(verify_federation_satellite(app, id))
        .await
        .map_err(|_| federation_transport_unavailable())?
        .map(Json)
}

async fn get_federation_onboarding_report(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<FederationOnboardingReport>, ApiError> {
    let inner = app.inner.lock().expect("state mutex poisoned");
    federation_snapshot_locked(&inner, &id)?;
    inner
        .federation
        .onboarding
        .reports
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            ApiError::from_status(StatusCode::NOT_FOUND, "no onboarding checklist recorded")
        })
}
