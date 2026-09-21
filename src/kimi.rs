// Kimi Code CLI discovery, launch, readiness and manual-approval admission.
// Owns the native CLI boundary, pre-prompt mode/thinking admission and atomic
// model+thinking observations, so one provider notification is one revision.
// Does not own credentials or the shared ACP lifecycle (acp.rs).
// Added for Kimi Code 2.0.2's `kimi acp` interface; never invoke a shell shim.

const MAX_KIMI_THINKING_CHARS: usize = 128;
const KIMI_MODEL_SET_TIMEOUT: Duration = Duration::from_secs(15);
const KIMI_THINKING_SET_TIMEOUT: Duration = Duration::from_secs(15);
// A fresh worker initializes and authenticates before handling refresh. Allow
// every bounded setup phase, model-dependent discovery, and response delivery.
const KIMI_MODEL_REFRESH_TIMEOUT: Duration = Duration::from_secs(
    ACP_INITIALIZE_TIMEOUT.as_secs() + ACP_AUTH_TIMEOUT.as_secs()
        + ACP_SESSION_SETUP_TIMEOUT.as_secs() + KIMI_MODEL_SET_TIMEOUT.as_secs() + 5,
);

fn kimi_thinking_options(config: &Value) -> Vec<SessionModelOption> {
    acp_session_config_options(config, "thinking", Some(MAX_KIMI_THINKING_CHARS))
        .into_iter()
        // Reserved host clear sentinel, never an explicit provider selection.
        .filter(|option| option.value != "auto")
        .collect()
}

/// Discover the thinking catalog for a valid requested model. Invalid saved
/// models still get model discovery, but must not borrow another model's efforts.
fn discover_kimi_thinking_config(
    writer: &mut impl Write,
    pending: &AcpPendingRequestMap,
    external_id: &str,
    requested_model: &str,
    config: &Value,
) -> Result<Option<Value>> {
    if matches!(requested_model.trim(), "" | "auto" | "default") {
        return Ok(Some(config.clone()));
    }
    let Some(model) = matching_acp_config_option_value(config, "model", requested_model) else {
        return Ok(None);
    };
    if current_acp_config_option_value(config, "model").as_deref() == Some(model.as_str()) {
        return Ok(Some(config.clone()));
    }
    let result = send_acp_json_rpc_request(
        writer,
        pending,
        "session/set_config_option",
        json!({"sessionId":external_id, "configId":"model", "value":model}),
        KIMI_MODEL_SET_TIMEOUT,
        AcpAgent::Kimi,
    )?;
    if current_acp_config_option_value(&result, "model").as_deref() != Some(model.as_str()) {
        bail!("Kimi did not acknowledge requested model `{model}` during reasoning discovery");
    }
    Ok(Some(result))
}

fn resolve_kimi_executable() -> Option<PathBuf> {
    resolve_kimi_executable_with(std::env::var_os("PATH").as_deref(), home_dir().as_deref())
}

fn resolve_kimi_executable_with(
    path: Option<&std::ffi::OsStr>,
    home: Option<&FsPath>,
) -> Option<PathBuf> {
    let name = if cfg!(windows) { "kimi.exe" } else { "kimi" };
    let mut directories: Vec<PathBuf> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    if let Some(home) = home {
        // The native installer may update PATH after this host was started.
        directories.push(home.join(".kimi-code").join("bin"));
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

fn kimi_acp_command(executable: Option<PathBuf>) -> Result<Command> {
    let executable = executable.context(
        "Kimi Code CLI was not found on PATH or in ~/.kimi-code/bin; install Kimi Code CLI and run `kimi login`",
    )?;
    let mut command = Command::new(executable);
    command.arg("acp");
    Ok(command)
}

fn kimi_agent_readiness_with(resolve: impl FnOnce() -> Option<PathBuf>) -> AgentReadiness {
    let path = resolve().map(|path| display_path_for_user(&normalize_user_facing_path(&path)));
    AgentReadiness {
        agent: Agent::Kimi,
        status: if path.is_some() { AgentReadinessStatus::Ready } else { AgentReadinessStatus::Missing },
        blocking: path.is_none(),
        detail: path.as_ref().map(|path| format!(
            "Kimi Code CLI is available at `{path}`; authentication is checked by its runtime. Run `kimi login` in a terminal if setup is needed."
        )).unwrap_or_else(||
            "Install Kimi Code CLI on PATH or in ~/.kimi-code/bin, then run `kimi login`. Windows requires native kimi.exe; shell shims are not supported.".to_owned()
        ),
        warning_detail: None,
        command_path: path,
    }
}
/// A missing or contradictory ACK refuses the turn; inheriting Auto/YOLO is
/// not compatible with this adapter's manual-permission contract.
fn configure_kimi_manual_approvals(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    session_id: &str,
) -> Result<Value> {
    let result = send_acp_json_rpc_request(
        writer,
        pending_requests,
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": "mode", "value": "default"}),
        Duration::from_secs(15),
        AcpAgent::Kimi,
    )?;
    if current_acp_config_option_value(&result, "mode").as_deref() != Some("default") {
        bail!("Kimi did not acknowledge Default mode; refusing to prompt without manual approvals. Use a CLI compatible with the verified Kimi Code 2.0.2 ACP contract, then restart the session. No approval override is available");
    }
    Ok(result)
}

/// Apply the user's explicit thinking selection against the fresh post-model,
/// post-mode catalog. Input aliases are not accepted here: PATCH stores a live
/// canonical value. Every prompt rechecks it, even on an already-ready runtime.
fn configure_kimi_thinking(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    external_session_id: &str,
    requested: Option<&str>,
    config: &Value,
) -> Result<Value> {
    let Some(requested) = requested else {
        return Ok(config.clone());
    };
    let options = kimi_thinking_options(config);
    if !options.iter().any(|option| option.value == requested) {
        bail!("Kimi did not advertise requested reasoning effort `{requested}`; refresh its choices and select a supported effort before retrying");
    }
    if current_acp_config_option_value(config, "thinking").as_deref() == Some(requested) {
        return Ok(config.clone());
    }
    let result = send_acp_json_rpc_request(
        writer,
        pending_requests,
        "session/set_config_option",
        json!({"sessionId": external_session_id, "configId": "thinking", "value": requested}),
        KIMI_THINKING_SET_TIMEOUT,
        AcpAgent::Kimi,
    )?;
    if current_acp_config_option_value(&result, "thinking").as_deref() != Some(requested) {
        bail!("Kimi did not acknowledge requested reasoning effort `{requested}`; refusing to prompt with a different effort");
    }
    // The full 2.0.2 setter ACK must also retain manual approval mode.
    if current_acp_config_option_value(&result, "mode").as_deref() != Some("default") {
        bail!("Kimi thinking acknowledgment did not retain Default mode; refusing to prompt without manual approvals");
    }
    Ok(result)
}

impl AppState {
    /// Provider observations update only the effective value/catalog, never
    /// requested intent: a delayed refresh cannot undo a newer settings PATCH.
    #[cfg(test)]
    fn sync_kimi_thinking(&self, session_id: &str, config: &Value) -> Result<()> {
        self.sync_kimi_config_observation(session_id, config, None, None, false)
    }

    // Runtime identity and requested-model fences are checked under the same
    // lock as publication. A combined notification produces one revision.
    fn sync_kimi_config_observation(
        &self,
        session_id: &str,
        config: &Value,
        expected_model: Option<&str>,
        source_runtime: Option<&RuntimeToken>,
        include_models: bool,
    ) -> Result<()> {
        let has_thinking = has_acp_config_option_list(config, "thinking");
        let models = (include_models && has_acp_config_option_list(config, "model"))
            .then(|| acp_model_options(config, AcpAgent::Kimi));
        if !has_thinking && models.is_none() {
            return Ok(());
        }
        let options = kimi_thinking_options(config);
        let current = current_acp_config_option_value(config, "thinking")
            .filter(|value| options.iter().any(|option| option.value == *value));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &inner.sessions[index];
        if source_runtime.is_some_and(|token| {
            record.runtime_stop_in_progress || !record.runtime.matches_runtime_token(token)
        }) {
            return Ok(());
        }
        let session = &record.session;
        if expected_model.is_some_and(|expected| session.model != expected) {
            return Ok(());
        }
        // Late notifications from a pre-switch runtime cannot repopulate the
        // cleared catalog. Explicit refresh publication uses its request fence.
        let mut admit_thinking = has_thinking;
        if expected_model.is_none()
            && inner.sessions[index].runtime_reset_required
            && !matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Approval
            )
        {
            admit_thinking = false;
        }
        if !matches!(session.model.as_str(), "" | "auto" | "default") {
            if let Some(observed) = current_acp_config_option_value(config, "model") {
                if matching_acp_config_option_value(config, "model", &session.model).as_deref()
                    != Some(observed.as_str())
                {
                    admit_thinking = false;
                }
            } else if include_models && expected_model.is_none() {
                // An asynchronous thinking-only event cannot prove which model
                // produced it, even from the current runtime (e.g. discovery of
                // an unavailable saved model leaves the CLI on another model).
                // Preserve existing observations until a model-bearing event or
                // a request-fenced refresh/setter result supplies that evidence.
                admit_thinking = false;
            }
        }
        let thinking_changed = admit_thinking
            && (session.kimi_effort_options != options || session.kimi_current_effort != current);
        let models_changed = models.as_ref().is_some_and(|models| session.model_options != *models);
        if session.agent != Agent::Kimi || (!thinking_changed && !models_changed) {
            return Ok(());
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if thinking_changed {
            record.session.kimi_effort_options = options;
            record.session.kimi_current_effort = current;
        }
        if let Some(models) = models.filter(|_| models_changed) {
            record.session.model_options = models;
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn admit_kimi_thinking(
        &self,
        writer: &mut impl Write,
        pending: &AcpPendingRequestMap,
        session_id: &str,
        external_session_id: &str,
        config: &Value,
        source_runtime: Option<&RuntimeToken>,
    ) -> Result<()> {
        // Production prompt dispatch already marked the session Active; settings
        // PATCH cannot race this snapshot. No global lock spans the ACP round trip.
        let requested = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            inner.sessions[index].session.kimi_effort.clone()
        };
        self.sync_kimi_config_observation(session_id, config, None, source_runtime, false)?;
        let result = configure_kimi_thinking(
            writer,
            pending,
            external_session_id,
            requested.as_deref(),
            config,
        )?;
        if result != *config {
            self.sync_kimi_config_observation(session_id, &result, None, source_runtime, false)?;
        }
        Ok(())
    }
}
