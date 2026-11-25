use super::*;
use axum::http::{HeaderMap, header};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum AccountRequest {
    Status,
    Login,
    Logout,
    UseStandalone,
    Cancel {
        #[serde(rename = "attemptId")]
        attempt_id: Uuid,
    },
}

#[derive(Serialize)]
struct AccountResponse {
    status: AccountView,
    #[serde(skip_serializing_if = "Option::is_none")]
    challenge: Option<operation::BrowserChallenge>,
}

pub(crate) async fn codex_account_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AccountRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_account_origin(&state, &headers)?;
    let action = match request {
        AccountRequest::Status => None,
        AccountRequest::Login => Some(operation::Action::Login),
        AccountRequest::Logout => Some(operation::Action::Logout),
        AccountRequest::UseStandalone => Some(operation::Action::UseStandalone),
        AccountRequest::Cancel { attempt_id } => {
            let admission = state
                .codex_account
                .admission
                .lock()
                .expect("Codex account mutex");
            let active = admission
                .operation
                .as_ref()
                .filter(|active| active.id == attempt_id)
                .ok_or_else(|| {
                    ApiError::conflict("This Codex browser sign-in is no longer active.")
                })?;
            active
                .signal
                .send(CodexAccountSignal::Cancel)
                .map_err(|_| {
                    ApiError::conflict("Codex sign-in is already settling. Check its status.")
                })?;
            None
        }
    };
    let challenge = match action {
        Some(action) => operation::start_operation(&state, action)?
            .await
            .map_err(|_| {
                ApiError::internal("TermAl did not receive the Codex account action result.")
            })?
            .map_err(ApiError::conflict)?,
        None => None,
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(AccountResponse {
            status: state.codex_account.view(),
            challenge,
        }),
    ))
}

fn require_account_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let listener = state
        .local_http_base_url
        .lock()
        .expect("local HTTP base URL mutex poisoned");
    let authority = listener
        .as_deref()
        .and_then(|url| url.strip_prefix("http://127.0.0.1:"));
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok());
    if let (Some(port), Some(host), Some(origin)) = (authority, host, origin)
        && (host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}"))
        && origin == format!("http://{host}")
    {
        return Ok(());
    }
    Err(ApiError {
        message: "Open the local TermAl UI to change the Codex account.".into(),
        status: StatusCode::FORBIDDEN,
        kind: None,
        durability_deadline: None,
    })
}
