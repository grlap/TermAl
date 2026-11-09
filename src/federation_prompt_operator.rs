// Owns local operator adapters and derived caller reference views.
// Does not mount production controls or grant authority to caller references.
// Recovered selectively from the retained operator policy component.
// Local operator adapters over the existing federation mutation owner. These
// use the ordinary local operator/OS trust boundary, not the own-caller agent
// facade or the peer listener. References below are derived views, not records.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptConsentRequest {
    expected_version: u64,
    creation_key: String,
    scope: FederationPromptScope,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptGrantRequest {
    expected_version: u64,
    creation_key: String,
    scope: FederationPromptScope,
    consent: FederationPromptPermissionIdentity,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptRevokeRequest {
    expected_version: u64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct FederationPromptPageQuery {
    after: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_operator_routes() -> Router<AppState> {
    // Foundation only: production registration waits for the qualified vertical.
    Router::new()
        .route(
            "/api/federation/prompt-permissions",
            get(get_federation_prompt_permissions),
        )
        .route(
            "/api/federation/prompt-consents",
            post(issue_federation_prompt_consent_api),
        )
        .route(
            "/api/federation/prompt-grants",
            post(issue_federation_prompt_grant_api),
        )
        .route(
            "/api/federation/prompt-permissions/{id}/revoke",
            post(revoke_federation_prompt_permission_api),
        )
        .route(
            "/api/federation/prompt-permissions/{id}/confirm-revocation",
            post(confirm_federation_prompt_revocation_api),
        )
        .route(
            "/api/sessions/{id}/remote-prompt-references",
            get(get_remote_prompt_references_api),
        )
        .layer(DefaultBodyLimit::max(
            FEDERATION_PROMPT_POLICY_REQUEST_BYTES,
        ))
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_operator_decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    if bytes.len() > FEDERATION_PROMPT_POLICY_REQUEST_BYTES {
        return Err(ApiError::from_status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "prompt policy request exceeds metadata bound",
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|_| ApiError::bad_request("invalid prompt policy operator request"))
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_operator_response<T: Serialize>(value: &T) -> Result<Response, ApiError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ApiError::internal("prompt policy response unavailable"))?;
    if bytes.len() > REMOTE_PROMPT_STATUS_BYTES {
        return Err(ApiError::internal("prompt policy response exceeds bound"));
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

#[cfg_attr(not(test), allow(dead_code))]
async fn issue_federation_prompt_consent_api(
    State(app): State<AppState>,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: FederationPromptConsentRequest = federation_prompt_operator_decode(&bytes)?;
    let record = run_blocking_api(move || {
        app.issue_federation_prompt_consent(
            request.expected_version,
            request.creation_key,
            request.scope,
        )
    })
    .await?;
    federation_prompt_operator_response(&record)
}

#[cfg_attr(not(test), allow(dead_code))]
async fn issue_federation_prompt_grant_api(
    State(app): State<AppState>,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: FederationPromptGrantRequest = federation_prompt_operator_decode(&bytes)?;
    let record = app
        .issue_federation_prompt_grant(
            request.expected_version,
            request.creation_key,
            request.scope,
            request.consent,
        )
        .await?;
    federation_prompt_operator_response(
        &json!({"targetReference":format!("prompt-reference-{}",record.identity.id),"permission":record}),
    )
}

#[cfg_attr(not(test), allow(dead_code))]
async fn revoke_federation_prompt_permission_api(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let request: FederationPromptRevokeRequest = federation_prompt_operator_decode(&bytes)?;
    let record = run_blocking_api(move || {
        app.revoke_federation_prompt_permission(request.expected_version, &id)
    })
    .await?;
    federation_prompt_operator_response(&record)
}

#[cfg_attr(not(test), allow(dead_code))]
async fn confirm_federation_prompt_revocation_api(
    State(app): State<AppState>,
    AxumPath(id): AxumPath<String>,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    // This explicit operator action only propagates an already committed
    // revocation. Empty JSON rejects hidden authority or a replacement scope.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Confirm {}
    let _: Confirm = federation_prompt_operator_decode(&bytes)?;
    let record = app.confirm_remote_prompt_revocation(&id).await?;
    federation_prompt_operator_response(&record)
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_prompt_page_start(
    records: &[FederationPromptPermission],
    after: Option<&str>,
) -> Result<usize, ApiError> {
    match after {
        None => Ok(0),
        Some(after) => {
            federation_prompt_uuid(after)?;
            records
                .iter()
                .position(|record| record.identity.id == after)
                .map(|index| index + 1)
                .ok_or_else(|| ApiError::bad_request("prompt policy page cursor not found"))
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
async fn get_federation_prompt_permissions(
    State(app): State<AppState>,
    Query(query): Query<FederationPromptPageQuery>,
) -> Result<Response, ApiError> {
    let value = run_blocking_api(move || {
        let inner = app.inner.lock().expect("state mutex poisoned");
        let records = inner
            .federation
            .prompt_permissions
            .as_ref()
            .map(|p| p.records.as_slice())
            .unwrap_or_default();
        let start = federation_prompt_page_start(records, query.after.as_deref())?;
        let page = records.iter().skip(start).take(4).collect::<Vec<_>>();
        let next = (start + page.len() < records.len())
            .then(|| page.last().map(|record| record.identity.id.clone()))
            .flatten();
        Ok(json!({"federationVersion":inner.federation.version,
        "records":page,"nextAfter":next}))
    })
    .await?;
    federation_prompt_operator_response(&value)
}

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    fn remote_prompt_references_for_caller(
        &self,
        caller: &str,
        after: Option<&str>,
    ) -> Result<Value, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let native = FederationMutationContext::locked(&inner).prompt_native_root(caller)?;
        let records = inner
            .federation
            .prompt_permissions
            .as_ref()
            .map(|p| p.records.as_slice())
            .unwrap_or_default();
        // Cursor lookup is also caller-bound; another caller's grant cannot
        // disclose a reference or be used to probe an unrelated page.
        let own = records
            .iter()
            .filter(|record| {
                record.kind == FederationPromptPermissionKind::CallerGrant
                    && record.scope.source == native
            })
            .cloned()
            .collect::<Vec<_>>();
        let start = federation_prompt_page_start(&own, after)?;
        let mut page = Vec::new();
        let mut next = None;
        for (index, record) in own.iter().enumerate().skip(start) {
            if let Some(consent) = record.consent.as_ref().filter(|consent| {
                federation_prompt_dispatch_policy_locked(
                    &inner,
                    &record.scope,
                    &record.identity,
                    consent,
                    FederationRole::Central,
                )
                .is_ok()
            }) {
                page.push(json!({"targetReference":format!("prompt-reference-{}",record.identity.id),
                    "targetHost":record.scope.target_host,"target":record.scope.target,
                    "grantGeneration":record.identity.generation,"consentGeneration":consent.generation}));
                if page.len() == 4 {
                    if index + 1 < own.len() {
                        next = Some(record.identity.id.clone());
                    }
                    break;
                }
            }
        }
        // No proof refresh, reference registration, permission mutation or
        // dispatch is performed. A later send obtains fresh native proof.
        Ok(json!({"protocolVersion":1,"references":page,"nextAfter":next}))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
async fn get_remote_prompt_references_api(
    State(app): State<AppState>,
    AxumPath(caller): AxumPath<String>,
    Query(query): Query<FederationPromptPageQuery>,
) -> Result<Response, ApiError> {
    let value = run_blocking_api(move || {
        app.remote_prompt_references_for_caller(&caller, query.after.as_deref())
    })
    .await?;
    federation_prompt_operator_response(&value)
}
