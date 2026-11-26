use super::operation::{BrowserChallenge, StartReply};
use super::*;
use tokio::sync::oneshot;

const ACCOUNT_RPC_TIMEOUT: Duration = Duration::from_secs(12);
const BROWSER_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) enum CodexAccountSignal {
    Cancel,
    LoginCompleted { login_id: Uuid, success: bool },
    Exited,
}

/// Uses the normal app-server stdio, request IDs and pending-reply map.
/// Account-only servers have no session events, and forward no raw diagnostics.
#[derive(Default)]
pub(crate) struct CodexAccountWire(Option<Sender<CodexAccountSignal>>);

impl CodexAccountWire {
    pub(super) fn for_login(signal: Sender<CodexAccountSignal>) -> Self {
        Self(Some(signal))
    }

    pub(crate) fn receive(&self, message: &Value, pending: &CodexPendingRequestMap) {
        if let Some(id) = message.get("id")
            && let Some(result) = message.get("result").or_else(|| message.get("error"))
        {
            if let Some(sender) = pending
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&codex_request_id_key(id))
            {
                let reply = if message.get("error").is_some() {
                    Err(CodexResponseError::JsonRpc(
                        "Codex refused the account request".into(),
                    ))
                } else {
                    Ok(result.clone())
                };
                let _ = sender.send(reply);
            }
            return;
        }
        if message.get("method").and_then(Value::as_str) != Some("account/login/completed") {
            return;
        }
        if let (Some(signal), Some(login_id), Some(success)) = (
            &self.0,
            message
                .pointer("/params/loginId")
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok()),
            message.pointer("/params/success").and_then(Value::as_bool),
        ) {
            let _ = signal.send(CodexAccountSignal::LoginCompleted { login_id, success });
        }
    }

    pub(crate) fn exited(&self) {
        if let Some(signal) = &self.0 {
            let _ = signal.send(CodexAccountSignal::Exited);
        }
    }
}

pub(super) fn account_rpc(
    state: &AppState,
    server: &SharedCodexRuntime,
    method: &str,
    params: Value,
) -> Result<Value, &'static str> {
    state
        .perform_codex_json_rpc_request_on_runtime(server, method, params, ACCOUNT_RPC_TIMEOUT)
        .map_err(|error| match error {
            CodexResponseError::JsonRpc(_) => {
                "Codex refused the account request. Check its sign-in policy and retry."
            }
            _ => "Codex did not confirm the account request. Retry the account action.",
        })
}

pub(super) fn check_account_config(
    state: &AppState,
    server: &SharedCodexRuntime,
) -> Result<(), &'static str> {
    let config = account_rpc(state, server, "config/read", json!({"includeLayers":false}))?;
    let values = config
        .get("config")
        .filter(|value| value.is_object())
        .ok_or("Codex did not report its configuration. Use a supported Codex version.")?;
    if values
        .get("model_provider")
        .filter(|value| !value.is_null())
        .is_some_and(|value| value.as_str() != Some("openai"))
        || values
            .get("cli_auth_credentials_store")
            .filter(|value| !value.is_null())
            .is_some_and(|value| value.as_str() != Some("file"))
        || values.get("forced_login_method").and_then(Value::as_str) == Some("api")
    {
        return Err(
            "TermAl account switching requires file-backed ChatGPT sign-in with the OpenAI provider.",
        );
    }
    Ok(())
}

/// Kept only for the duration of a switch and never logged or serialized.
#[derive(PartialEq, Eq)]
pub(super) struct AccountIdentity {
    user_id: String,
    workspace_id: Option<String>,
}

pub(super) fn read_account(
    state: &AppState,
    server: &SharedCodexRuntime,
) -> Result<Option<AccountIdentity>, &'static str> {
    let read = account_rpc(state, server, "account/read", json!({"refreshToken":false}))?;
    match (
        read.get("account"),
        read.get("requiresOpenaiAuth").and_then(Value::as_bool),
    ) {
        (Some(account), Some(true)) if account.is_null() => Ok(None),
        (Some(account), Some(true))
            if account.get("type").and_then(Value::as_str) == Some("chatgpt") =>
        {
            let user_id = read.get("userId").and_then(Value::as_str).filter(|id| !id.is_empty())
                .ok_or("Codex did not report the signed-in account identity. Retry with a supported Codex version.")?;
            let workspace_id = read
                .pointer("/workspaceRouting/chatgptAccountId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok(Some(AccountIdentity {
                user_id: user_id.to_owned(),
                workspace_id,
            }))
        }
        (Some(_), Some(_)) => {
            Err("This Codex provider does not use supported ChatGPT account sign-in.")
        }
        _ => Err("Codex did not report an effective account. Check the Codex version and retry."),
    }
}

pub(super) enum Completion {
    LoginConfirmed(AccountIdentity),
    LogoutAcknowledged,
    Cancelled,
    Unconfirmed,
}

pub(super) fn run_browser_login(
    state: &AppState,
    server: &SharedCodexRuntime,
    attempt_id: Uuid,
    signals: mpsc::Receiver<CodexAccountSignal>,
    reply: &mut Option<oneshot::Sender<StartReply>>,
) -> Result<Completion, &'static str> {
    let started = account_rpc(
        state,
        server,
        "account/login/start",
        json!({"type":"chatgpt"}),
    )?;
    if started.get("type").and_then(Value::as_str) != Some("chatgpt") {
        return Err(
            "Codex did not start a browser sign-in. Retry /login with a supported Codex version.",
        );
    }
    let login_id = started
        .get("loginId")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or("Codex did not return a browser login identifier. Retry /login.")?;
    let authorization_url = started
        .get("authUrl")
        .and_then(Value::as_str)
        .and_then(|uri| reqwest::Url::parse(uri).ok())
        .filter(|uri| {
            uri.scheme() == "https"
                && uri.username().is_empty()
                && uri.password().is_none()
                && matches!(uri.host_str(), Some("auth.openai.com" | "chatgpt.com"))
                && uri.port().is_none()
        })
        .ok_or("Codex did not return a supported browser sign-in URL. Retry /login.")?;
    if let Some(reply) = reply.take()
        && reply
            .send(Ok(Some(BrowserChallenge {
                attempt_id,
                authorization_url: authorization_url.into(),
            })))
            .is_err()
    {
        return Ok(Completion::Cancelled);
    }
    let deadline = std::time::Instant::now() + BROWSER_LOGIN_TIMEOUT;
    loop {
        match signals.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(CodexAccountSignal::LoginCompleted {
                login_id: id,
                success,
            }) if id == login_id => {
                return Ok(if success {
                    Completion::LoginConfirmed(read_account(state, server)?.ok_or(
                        "Codex completed sign-in without an effective account. Retry /login.",
                    )?)
                } else {
                    Completion::Unconfirmed
                });
            }
            Ok(CodexAccountSignal::LoginCompleted { .. }) => {}
            Ok(CodexAccountSignal::Cancel) => {
                let _ = account_rpc(
                    state,
                    server,
                    "account/login/cancel",
                    json!({"loginId":login_id}),
                );
                return Ok(Completion::Cancelled);
            }
            Ok(CodexAccountSignal::Exited) | Err(_) => return Ok(Completion::Unconfirmed),
        }
    }
}
