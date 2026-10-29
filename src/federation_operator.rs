// Owns local operator HTTP configuration, including configuring this host as a
// central or as a satellite of a named central, and the federation registry
// read view. Does not enroll peers, provision transport or bypass domain
// persistence. New adapter over federation_mutations.rs and federation_views.rs.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateFederationSettingsRequest {
    enabled: bool,
    role: FederationRole,
    central: Option<FederationHostIdentity>,
    /// The domain `version` the operator read; required for a change from or
    /// to the satellite role, and honoured whenever it is given.
    #[serde(default)]
    expected_version: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateFederationSshHostRequest {
    expected_version: u64,
    ssh_host: String,
}

async fn update_federation_ssh_host_route(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    request: Result<Json<UpdateFederationSshHostRequest>, JsonRejection>,
) -> Result<Json<Value>, Response> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("federation SSH hostname", rejection).into_response())?;
    run_blocking_api(move || {
        state.update_federation_ssh_host(&id, request.expected_version, request.ssh_host)?;
        state.federation_registry_view().map(Json)
    })
    .await
    .map_err(federation_settings_refusal)
}

async fn get_federation(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let view = run_blocking_api(move || state.federation_registry_view()).await?;
    Ok(Json(view))
}

async fn update_federation_settings(
    State(state): State<AppState>,
    request: Result<Json<UpdateFederationSettingsRequest>, JsonRejection>,
) -> Result<Json<Value>, Response> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("federation settings", rejection).into_response())?;
    let view = run_blocking_api(move || {
        state.configure_federation_fenced(
            FederationSettings {
                enabled: request.enabled,
                role: request.role,
                central: request.central,
            },
            request.expected_version,
            FederationSettingsFence::Operator,
        )?;
        state.federation_registry_view()
    })
    .await
    .map_err(federation_settings_refusal)?;
    Ok(Json(view))
}

/// A typed settings refusal carries its stable `code` beside `error`, so an
/// operator or client can tell a stale read from a stale central without
/// parsing the message. Every other error keeps the plain `{error}` shape.
fn federation_settings_refusal(error: ApiError) -> Response {
    match error.kind.and_then(ApiErrorKind::federation_operator_code) {
        Some(code) => (
            error.status,
            Json(json!({"error": error.message, "code": code})),
        )
            .into_response(),
        None => error.into_response(),
    }
}
