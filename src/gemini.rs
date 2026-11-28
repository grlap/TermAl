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

fn gemini_effort_config_id(config: &Value) -> Option<&str> {
    let entries = config
        .get("configOptions")
        .or_else(|| config.get("config_options"))?
        .as_array()?;
    entries.iter().find_map(|entry| {
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

impl AcpRuntimeState {
    pub(crate) fn replace_gemini_config(&mut self, config: Value) {
        self.gemini_config = Some(config);
        self.gemini_config_revision += 1;
    }
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
    // A setter can be followed or preceded by a config notification. If one
    // arrived while the RPC was in flight, reconcile from the reader's latest
    // snapshot rather than publishing the writer's older view over it.
    for _ in 0..4 {
        let mut cache = runtime.lock().expect("ACP runtime state mutex poisoned");
        let Some(config) = cache.gemini_config.as_ref() else {
            return Ok(());
        };
        let advertised_model = matching_acp_config_option_value(config, "model", model);
        let current_model = current_acp_config_option_value(config, "model");
        let mut model_matches = advertised_model.as_ref().map_or_else(
            || gemini_model_is_provider_default(model) || current_model.as_deref() == Some(model),
            |value| current_model.as_ref() == Some(value),
        );
        if let Some(value) = advertised_model.filter(|_| !model_matches) {
            let revision = cache.gemini_config_revision;
            drop(cache);
            let updated = set_gemini_acp_config(writer, pending, external_id, "model", &value)?;
            cache = runtime.lock().expect("ACP runtime state mutex poisoned");
            if cache.gemini_config_revision != revision {
                continue;
            }
            model_matches = current_acp_config_option_value(&updated, "model").as_deref()
                == Some(value.as_str());
            cache.replace_gemini_config(updated);
        }

        let config = cache
            .gemini_config
            .as_ref()
            .expect("Gemini config is present");
        // Publish model-dependent choices before attempting the effort setter,
        // so a refusal still leaves the choices Gemini actually advertised.
        state.observe_gemini_acp_config(session_id, token, Some(model), config, model_matches)?;
        if !apply_effort || !model_matches {
            return Ok(());
        }
        let Some(id) = gemini_effort_config_id(config) else {
            return Ok(());
        };
        let Some((requested, inherited)) = state.gemini_requested_effort(session_id, token) else {
            return Ok(());
        };
        let (current, options) = gemini_effort_observation(config);
        let find_effort = |value: &str| options.iter().find(|option| option.value == value);
        let selected = if requested.as_deref() == Some(GEMINI_PROVIDER_DEFAULT_EFFORT) {
            find_effort(GEMINI_PROVIDER_DEFAULT_EFFORT).or_else(|| find_effort("default"))
        } else {
            [requested.as_deref(), inherited.as_deref()]
                .into_iter()
                .flatten()
                .find_map(find_effort)
        };
        let Some(selected) = selected.filter(|option| Some(&option.value) != current.as_ref())
        else {
            return Ok(());
        };

        let id = id.to_owned();
        let previous_model = current_acp_config_option_value(config, "model");
        let revision = cache.gemini_config_revision;
        drop(cache);
        let updated = set_gemini_acp_config(writer, pending, external_id, &id, &selected.value)?;
        cache = runtime.lock().expect("ACP runtime state mutex poisoned");
        if cache.gemini_config_revision != revision {
            continue;
        }

        let updated_model = current_acp_config_option_value(&updated, "model");
        let same_model = previous_model == updated_model;
        let acknowledged = gemini_effort_observation(&updated).0;
        state.observe_gemini_acp_config(
            session_id,
            token,
            previous_model.as_deref().or(Some(model)),
            &updated,
            same_model,
        )?;
        cache.replace_gemini_config(updated);
        if !same_model || acknowledged.as_ref() != Some(&selected.value) {
            bail!("Gemini did not acknowledge the selected reasoning effort for this model");
        }
        return Ok(());
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
    ) -> Result<()> {
        let current_model = current_acp_config_option_value(config, "model");
        let has_model_choices = has_acp_config_option_list(config, "model");
        let models = acp_model_options(config, AcpAgent::Gemini);
        let (effort, choices) = if use_effort {
            gemini_effort_observation(config)
        } else {
            (None, Vec::new())
        };
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
        let model = current_model.filter(|_| use_effort && expected_model.is_some());
        let effort_matches = use_effort
            && (expected_model.is_some()
                || current_acp_config_option_value(config, "model").as_deref()
                    == Some(record.session.model.as_str()));
        if !effort_matches && expected_model.is_none() {
            return Ok(());
        }
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
