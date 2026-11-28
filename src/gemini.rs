// Antigravity CLI boundary for the Gemini harness: native executable discovery
// and launch arguments only. The shared ACP adapter owns the protocol.
// Authentication and settings belong to the CLI; never inspect or rewrite them.

use super::*;

pub(crate) const GEMINI_PROVIDER_DEFAULT_EFFORT: &str = "auto";
const MAX_GEMINI_EFFORT_CHARS: usize = 128;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiReasoning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) inherited: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) current: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) options: Vec<SessionModelOption>,
}

impl GeminiReasoning {
    pub(crate) fn is_empty(&self) -> bool {
        self.requested.is_none()
            && self.inherited.is_none()
            && self.current.is_none()
            && self.options.is_empty()
    }

    pub(crate) fn clear_observation(&mut self) {
        self.current = None;
        self.options.clear();
    }
}

pub(crate) fn default_gemini_effort_preference() -> String {
    GEMINI_PROVIDER_DEFAULT_EFFORT.to_owned()
}

pub(crate) fn normalize_gemini_effort(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_GEMINI_EFFORT_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("Invalid Gemini reasoning effort"));
    }
    Ok(value.to_owned())
}

pub(crate) fn resolve_gemini_executable() -> Option<PathBuf> {
    resolve_gemini_executable_with(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("LOCALAPPDATA").as_deref().map(FsPath::new),
    )
}

pub(crate) fn resolve_gemini_executable_with(
    path: Option<&std::ffi::OsStr>,
    local_app_data: Option<&FsPath>,
) -> Option<PathBuf> {
    let name = if cfg!(windows) { "agy.exe" } else { "agy" };
    let mut directories: Vec<PathBuf> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    // The native Windows installer can update PATH after this host starts.
    if cfg!(windows) {
        if let Some(local_app_data) = local_app_data {
            directories.push(local_app_data.join("agy").join("bin"));
        }
    }
    directories.into_iter().find_map(|dir| {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if candidate.metadata().ok()?.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        fs::canonicalize(candidate).ok()
    })
}

pub(crate) fn gemini_acp_command(
    executable: Option<PathBuf>,
    approval_mode: Option<GeminiApprovalMode>,
) -> Result<Command> {
    let executable = executable.context(
        "Antigravity CLI was not found; install native agy on PATH (or in %LOCALAPPDATA%/agy/bin on Windows)",
    )?;
    let mut command = Command::new(executable);
    command.arg("acp");
    match approval_mode.unwrap_or(GeminiApprovalMode::Default) {
        GeminiApprovalMode::Default => {}
        GeminiApprovalMode::Yolo => {
            command.arg("--dangerously-skip-permissions");
        }
        GeminiApprovalMode::AutoEdit => {
            command.args(["--mode", "accept-edits"]);
        }
        GeminiApprovalMode::Plan => {
            command.args(["--mode", "plan"]);
        }
    }
    Ok(command)
}

// Cached sign-in is consumed by agy itself. Only noninteractive methods with
// explicit process environment and an exact advertisement may be selected.
pub(crate) fn agy_auth_method(initialize: &Value) -> Option<String> {
    let present = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    agy_auth_method_with(
        initialize,
        present("GEMINI_API_KEY"),
        present("GOOGLE_CLOUD_PROJECT") && present("GOOGLE_CLOUD_LOCATION"),
    )
}

pub(crate) fn agy_auth_method_with(
    initialize: &Value,
    api_key: bool,
    platform: bool,
) -> Option<String> {
    let methods = initialize.get("authMethods")?.as_array()?;
    [("gemini-api-key", api_key), ("agent-platform", platform)]
        .into_iter()
        .find(|(id, configured)| {
            *configured
                && methods
                    .iter()
                    .any(|method| method["id"].as_str() == Some(*id))
        })
        .map(|(id, _)| id.to_owned())
}

fn gemini_config_options(config: &Value) -> Option<&[Value]> {
    config
        .get("configOptions")
        .or_else(|| config.get("config_options"))?
        .as_array()
        .map(Vec::as_slice)
}

fn gemini_effort_config_id(config: &Value) -> Option<&str> {
    gemini_config_options(config)?.iter().find_map(|entry| {
        let id = entry.get("id")?.as_str()?;
        let category = entry.get("category").and_then(Value::as_str);
        let label = entry.get("name").and_then(Value::as_str);
        (entry.get("type").and_then(Value::as_str) == Some("select")
            && id.len() <= MAX_GEMINI_EFFORT_CHARS
            && !id.is_empty()
            && !id.chars().any(char::is_control)
            && (category == Some("thought_level")
                || label.is_some_and(|label| label.eq_ignore_ascii_case("Thinking level"))))
        .then_some(id)
    })
}

fn gemini_effort_observation(config: &Value) -> (Option<String>, Vec<SessionModelOption>) {
    let Some(id) = gemini_effort_config_id(config) else {
        return (None, Vec::new());
    };
    let options = acp_session_config_options(config, id, Some(MAX_GEMINI_EFFORT_CHARS));
    let current = current_acp_config_option_value(config, id)
        .filter(|value| options.iter().any(|option| &option.value == value));
    (current, options)
}

fn gemini_model_is_provider_default(model: &str) -> bool {
    matches!(model, "" | "default" | "auto")
}

fn gemini_has_model_selector(config: &Value) -> bool {
    gemini_config_options(config)
        .is_some_and(|options| options.iter().any(|entry| entry["id"] == "model"))
}

impl AcpRuntimeState {
    pub(crate) fn accepts_gemini_config_update(&self, update: &Value) -> bool {
        has_acp_config_option_list(update, "model")
            || (self
                .gemini_config
                .as_ref()
                .is_some_and(|previous| !gemini_has_model_selector(previous))
                && !gemini_has_model_selector(update)
                && gemini_effort_config_id(update).is_some_and(|id| {
                    has_acp_config_option_list(update, id)
                        && current_acp_config_option_value(update, id).is_some()
                }))
    }

    pub(crate) fn store_gemini_config(&mut self, config: Value) {
        if !self.gemini_rejected_efforts.is_empty() {
            let (effort, choices) = gemini_effort_observation(&config);
            if self.gemini_config.as_ref().is_none_or(|previous| {
                current_acp_config_option_value(previous, "model")
                    != current_acp_config_option_value(&config, "model")
                    || gemini_effort_config_id(previous) != gemini_effort_config_id(&config)
                    || gemini_effort_observation(previous).1 != choices
            }) {
                self.gemini_rejected_efforts.clear();
            } else if let Some(effort) = effort {
                self.gemini_rejected_efforts.remove(&effort);
            }
        }
        self.gemini_config = Some(config);
    }

    pub(crate) fn receive_gemini_config_reply(
        &mut self,
        scope: GeminiConfigReply,
        reply: &Value,
    ) -> std::result::Result<(), AcpResponseError> {
        let reply_id = reply.get("sessionId").and_then(Value::as_str);
        let external_id = match scope {
            GeminiConfigReply::NewSession => Some(
                reply_id
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        AcpResponseError::Transport(
                            "Gemini ACP session/new did not return a session id".to_owned(),
                        )
                    })?
                    .to_owned(),
            ),
            GeminiConfigReply::Continuation(expected) => {
                if reply_id.is_some_and(|actual| actual != expected.as_str()) {
                    return Err(AcpResponseError::Transport(
                        "Gemini ACP continuation returned a different session id".to_owned(),
                    ));
                }
                Some(expected)
            }
            GeminiConfigReply::Setter(expected) => {
                if self.current_session_id.as_deref() != Some(expected.as_str())
                    || reply_id.is_some_and(|actual| actual != expected.as_str())
                {
                    return Err(AcpResponseError::Transport(
                        "Gemini ACP config reply belongs to another session".to_owned(),
                    ));
                }
                None
            }
        };
        if let Some(id) = external_id {
            // Bind before returning to the reader loop so the next notification
            // has an owner even if the writer hasn't consumed this reply.
            self.current_session_id = Some(id);
        }
        if gemini_config_options(reply).is_some() {
            self.store_gemini_config(reply.clone());
        }
        Ok(())
    }
}

fn gemini_unsupported_effort(error: &AcpJsonRpcError, id: &str, value: &str) -> bool {
    if error.code != Some(-32602) {
        return false;
    }
    let data = error.data.as_ref();
    if data.and_then(|data| data["configId"].as_str()) != Some(id)
        || data.and_then(|data| data["value"].as_str()) != Some(value)
    {
        return false;
    }
    let message = error.message.to_ascii_lowercase();
    let message = message
        .trim_start()
        .strip_prefix("invalid params: ")
        .unwrap_or(message.trim_start());
    let reason = data.and_then(|data| data.get("reason"));
    if reason.is_some_and(|reason| reason.as_str() != Some("unsupported_value")) {
        return false;
    }
    message.starts_with("unsupported value")
        || message.starts_with("unsupported config option value")
        || (message == "invalid params" && reason.is_some())
}

pub(crate) fn configure_gemini_acp_session(
    writer: &mut impl Write,
    pending: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    external_id: &str,
    runtime: &Arc<Mutex<AcpRuntimeState>>,
    token: Option<&RuntimeToken>,
    model: &str,
    apply_effort: bool,
) -> Result<()> {
    const MAX_SETTERS: usize = 4;
    let provider_default = gemini_model_is_provider_default(model);
    let mut model_set_to = None::<String>;
    for sent in 0..=MAX_SETTERS {
        if !apply_effort
            && let Some(token) = token
            && !state.gemini_refresh_is_current(session_id, token, model)
        {
            return Err(GeminiConfigRefreshDeferred.into());
        }
        let cache = runtime.lock().expect("ACP runtime state mutex poisoned");
        let config = cache.gemini_config.as_ref();
        let current_model =
            config.and_then(|config| current_acp_config_option_value(config, "model"));
        let requested_model = match &model_set_to {
            // An advertised default can resolve to a concrete provider model.
            Some(_) if provider_default => None,
            Some(value) => Some(value.clone()),
            None => config
                .and_then(|config| matching_acp_config_option_value(config, "model", model))
                .or_else(|| (!provider_default).then(|| model.to_owned())),
        };
        if let Some(value) = requested_model.filter(|value| current_model.as_ref() != Some(value)) {
            if model_set_to.is_some() {
                bail!("Gemini changed the model after acknowledging the selected model");
            }
            if sent == MAX_SETTERS {
                break;
            }
            drop(cache);
            let reply = set_gemini_acp_config(writer, pending, external_id, "model", &value)?;
            if !provider_default
                && current_acp_config_option_value(&reply, "model").as_deref()
                    != Some(value.as_str())
            {
                bail!("Gemini did not acknowledge the selected model");
            }
            model_set_to = Some(value);
            continue;
        }
        let Some(config) = config else {
            if !apply_effort {
                bail!("Gemini did not report a configuration catalog");
            }
            return Ok(());
        };

        let effort_scoped = current_model.is_some() || !gemini_has_model_selector(config);
        if apply_effort
            && effort_scoped
            && let Some(id) = gemini_effort_config_id(config)
            && let Some((requested, inherited)) = state.gemini_requested_effort(session_id, token)
        {
            let (current, options) = gemini_effort_observation(config);
            let find_effort = |value: &str| {
                options.iter().find(|option| {
                    option.value == value && !cache.gemini_rejected_efforts.contains(value)
                })
            };
            let selected = if requested.as_deref() == Some(GEMINI_PROVIDER_DEFAULT_EFFORT) {
                find_effort(GEMINI_PROVIDER_DEFAULT_EFFORT).or_else(|| find_effort("default"))
            } else {
                [requested.as_deref(), inherited.as_deref()]
                    .into_iter()
                    .flatten()
                    .find_map(find_effort)
            };
            if let Some(selected) =
                selected.filter(|option| Some(&option.value) != current.as_ref())
            {
                if sent == MAX_SETTERS {
                    break;
                }
                let id = id.to_owned();
                drop(cache);
                let result = set_gemini_acp_config(writer, pending, external_id, &id, &selected.value)
                    .and_then(|reply| {
                        if current_acp_config_option_value(&reply, "model") != current_model
                            || gemini_effort_observation(&reply).0.as_ref() != Some(&selected.value)
                        {
                            bail!("Gemini did not acknowledge the selected reasoning effort for this model");
                        }
                        Ok(())
                    });
                if let Err(error) = result {
                    let mut cache = runtime.lock().expect("ACP runtime state mutex poisoned");
                    if let Some(config) = &cache.gemini_config {
                        let same_model =
                            current_acp_config_option_value(config, "model") == current_model;
                        if same_model
                            && gemini_effort_config_id(config) == Some(id.as_str())
                            && let Some(refusal) = acp_json_rpc_response_error(&error)
                            && gemini_unsupported_effort(refusal, &id, &selected.value)
                        {
                            cache.gemini_rejected_efforts.insert(selected.value.clone());
                            continue;
                        }
                        state.observe_gemini_acp_config(
                            session_id,
                            token,
                            Some(model),
                            config,
                            same_model,
                            &cache.gemini_rejected_efforts,
                        )?;
                    }
                    return Err(error);
                }
                // The reader has already applied the reply. Read again in case a
                // later config notification superseded the confirmed selection.
                continue;
            }
        }

        return state.observe_gemini_acp_config(
            session_id,
            token,
            Some(model),
            config,
            effort_scoped,
            &cache.gemini_rejected_efforts,
        );
    }
    bail!("Gemini config changed repeatedly while preparing the session")
}

fn set_gemini_acp_config(
    writer: &mut impl Write,
    pending: &AcpPendingRequestMap,
    external_id: &str,
    id: &str,
    value: &str,
) -> Result<Value> {
    send_acp_json_rpc_request(
        writer,
        pending,
        "session/set_config_option",
        json!({"sessionId": external_id, "configId": id, "value": value}),
        Duration::from_secs(15),
        AcpAgent::Gemini,
    )
}

impl AppState {
    pub(crate) fn gemini_refresh_is_current(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        model: &str,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.find_session_index(session_id).is_some_and(|index| {
            let record = &inner.sessions[index];
            record.session.agent == Agent::Gemini
                && record.runtime.matches_runtime_token(token)
                && matches!(
                    record.session.status,
                    SessionStatus::Idle | SessionStatus::Error
                )
                && !record.runtime_stop_in_progress
                && !record.runtime_reset_required
                && record.session.model == model
        })
    }

    fn gemini_requested_effort(
        &self,
        session_id: &str,
        token: Option<&RuntimeToken>,
    ) -> Option<(Option<String>, Option<String>)> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if token.is_some_and(|token| !record.runtime.matches_runtime_token(token)) {
            return None;
        }
        let reasoning = &record.session.gemini_reasoning;
        Some((reasoning.requested.clone(), reasoning.inherited.clone()))
    }

    pub(crate) fn observe_gemini_acp_config(
        &self,
        session_id: &str,
        token: Option<&RuntimeToken>,
        expected_model: Option<&str>,
        config: &Value,
        use_effort: bool,
        rejected_efforts: &HashSet<String>,
    ) -> Result<()> {
        let current_model = current_acp_config_option_value(config, "model");
        let has_model_choices = has_acp_config_option_list(config, "model");
        let models = acp_model_options(config, AcpAgent::Gemini);
        let (mut effort, mut choices) = if use_effort {
            gemini_effort_observation(config)
        } else {
            (None, Vec::new())
        };
        choices.retain(|choice| !rejected_efforts.contains(&choice.value));
        effort = effort.filter(|effort| !rejected_efforts.contains(effort));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let record = &inner.sessions[index];
        if record.session.agent != Agent::Gemini
            || token.is_some_and(|token| !record.runtime.matches_runtime_token(token))
            || expected_model.is_some_and(|model| model != record.session.model)
        {
            return Ok(());
        }
        let effort_matches = use_effort
            && (expected_model.is_some()
                || current_model.as_deref() == Some(record.session.model.as_str())
                || (gemini_model_is_provider_default(&record.session.model)
                    && (current_model.is_some() || !gemini_has_model_selector(config))));
        let model = current_model.filter(|_| {
            use_effort
                && expected_model.is_some_and(|model| !gemini_model_is_provider_default(model))
        });
        let (effort, choices) = if effort_matches {
            (effort, choices)
        } else {
            (None, Vec::new())
        };
        let session = &record.session;
        if model.as_ref().is_none_or(|model| model == &session.model)
            && (!has_model_choices || models == session.model_options)
            && effort == session.gemini_reasoning.current
            && choices == session.gemini_reasoning.options
        {
            return Ok(());
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("valid Gemini session index");
        if let Some(model) = model {
            record.session.model = model;
        }
        if has_model_choices {
            record.session.model_options = models;
        }
        record.session.gemini_reasoning.current = effort;
        record.session.gemini_reasoning.options = choices;
        self.commit_locked(&mut inner)?;
        Ok(())
    }
}
