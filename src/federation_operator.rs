// Owns local operator HTTP configuration and the federation registry read view.
// Does not enroll peers, provision transport or bypass domain persistence.
// New adapter over federation_mutations.rs and federation_views.rs.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateFederationSettingsRequest {
    enabled: bool,
    role: FederationRole,
    central: Option<FederationHostIdentity>,
}

async fn get_federation(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let view = run_blocking_api(move || state.federation_registry_view()).await?;
    Ok(Json(view))
}

async fn update_federation_settings(
    State(state): State<AppState>,
    request: Result<Json<UpdateFederationSettingsRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("federation settings", rejection))?;
    if request.role == FederationRole::Satellite {
        return Err(ApiError::bad_request(
            "satellite configuration requires the enrollment workflow",
        ));
    }
    let view = run_blocking_api(move || {
        state.configure_federation(FederationSettings {
            enabled: request.enabled,
            role: request.role,
            central: request.central,
        })?;
        state.federation_registry_view()
    })
    .await?;
    Ok(Json(view))
}
