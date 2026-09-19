// Agent readiness probing — for each locally probed agent (Codex, Cursor,
// Claude, Gemini, OpenCode) asks: is the CLI installed, reachable on PATH, and
// configured correctly for the current workdir? Produces an `AgentReadiness` record
// carrying a Ready/Warning/Unavailable status plus an optional
// warning-detail string surfaced in the UI.
//
// The results feed the agent-readiness cache in state.rs
// (`AgentReadinessCache`); see `snapshot_from_inner` for how staleness + TTL
// are reconciled. Session creation also force-refreshes a session's
// corresponding agent's readiness via `agent_readiness_for`.
//
// Extracted from runtime.rs into its own `include!()` fragment so runtime.rs
// stays focused on actual runtime processes.

/// Collects agent readiness.
fn collect_agent_readiness(default_workdir: &str) -> Vec<AgentReadiness> {
    collect_agent_readiness_with(default_workdir, agent_readiness_for)
}

fn collect_agent_readiness_with(
    workdir: &str,
    probe: impl Fn(Agent, &str) -> AgentReadiness,
) -> Vec<AgentReadiness> {
    [
        Agent::Codex,
        Agent::Claude,
        Agent::Cursor,
        Agent::Gemini,
        Agent::OpenCode,
    ]
    .into_iter()
    .map(|agent| probe(agent, workdir))
    .collect()
}

/// Validates agent session setup.
fn validate_agent_session_setup(agent: Agent, workdir: &str) -> std::result::Result<(), String> {
    validate_agent_session_setup_with(agent, workdir, agent_readiness_for)
}

fn validate_agent_session_setup_with(
    agent: Agent,
    workdir: &str,
    readiness_for: impl FnOnce(Agent, &str) -> AgentReadiness,
) -> std::result::Result<(), String> {
    let readiness = readiness_for(agent, workdir);
    if readiness.blocking {
        return Err(readiness.detail);
    }
    Ok(())
}

impl AppState {
    fn validate_agent_session_setup_for_state(
        &self,
        agent: Agent,
        workdir: &str,
    ) -> std::result::Result<(), String> {
        #[cfg(test)]
        if let Some(detail) = self.test_agent_setup_failure(agent) {
            return Err(detail);
        }

        // Lightweight states cannot launch local agent processes, so their
        // protocol tests do not depend on optional machine CLIs. Production
        // states always have runtime spawning enabled and take the real path.
        if !self.agent_runtime_spawning_enabled {
            return Ok(());
        }
        validate_agent_session_setup(agent, workdir)
    }

    #[cfg(test)]
    fn test_agent_setup_failure(&self, agent: Agent) -> Option<String> {
        self.test_agent_setup_failures
            .lock()
            .expect("test agent setup failures mutex poisoned")
            .iter()
            .find(|(candidate, _)| *candidate == agent)
            .map(|(_, detail)| detail.clone())
    }
}

/// Handles agent readiness for.
fn agent_readiness_for(agent: Agent, workdir: &str) -> AgentReadiness {
    match agent {
        Agent::Codex => codex_agent_readiness(),
        Agent::Claude => claude_agent_readiness_with(resolve_claude_executable),
        Agent::Cursor => cursor_agent_readiness(),
        Agent::Gemini => gemini_agent_readiness(workdir),
        Agent::OpenCode => opencode_agent_readiness(),
    }
}

fn claude_agent_readiness_with(resolve: impl FnOnce() -> Option<PathBuf>) -> AgentReadiness {
    let path = resolve().map(|path| display_path_for_user(&normalize_user_facing_path(&path)));
    AgentReadiness {
        agent: Agent::Claude,
        status: if path.is_some() {
            AgentReadinessStatus::Ready
        } else {
            AgentReadinessStatus::Missing
        },
        blocking: path.is_none(),
        detail: path
            .as_ref()
            .map(|path| {
                format!(
                    "Claude CLI is available at `{path}`; authentication is checked by its runtime."
                )
            })
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "Install the `claude` CLI native claude.exe on PATH; .cmd/.bat shims are not supported by this runtime."
                } else {
                    "Install the `claude` CLI and make sure it is executable on PATH."
                }.to_owned()
            }),
        warning_detail: None,
        command_path: path,
    }
}

/// Handles Codex agent readiness.
fn codex_agent_readiness() -> AgentReadiness {
    let command_path = resolve_codex_executable()
        .ok()
        .map(|path| path.display().to_string());
    match command_path {
        Some(command_path) => AgentReadiness {
            agent: Agent::Codex,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Codex CLI is available at `{command_path}`."),
            warning_detail: codex_windows_shell_warning(),
            command_path: Some(command_path),
        },
        None => AgentReadiness {
            agent: Agent::Codex,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail:
                "Install the `codex` CLI and make sure it is on PATH before creating Codex sessions."
                    .to_owned(),
            warning_detail: None,
            command_path: None,
        },
    }
}

/// Handles cursor agent readiness.
fn cursor_agent_readiness() -> AgentReadiness {
    let command_path = find_command_on_path("cursor-agent").map(|path| path.display().to_string());
    match command_path {
        Some(command_path) => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Cursor Agent is available at `{command_path}`."),
            warning_detail: None,
            command_path: Some(command_path),
        },
        None => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail: "Install `cursor-agent` and make sure it is on PATH before creating Cursor sessions."
                .to_owned(),
            warning_detail: None,
            command_path: None,
        },
    }
}

/// Handles Gemini agent readiness.
fn gemini_agent_readiness(workdir: &str) -> AgentReadiness {
    let command_path = match find_command_on_path("gemini") {
        Some(path) => path,
        None => {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Missing,
                blocking: true,
                detail: "Install the `gemini` CLI and make sure it is on PATH before creating Gemini sessions."
                    .to_owned(),
                warning_detail: None,
                command_path: None,
            };
        }
    };
    let command_path_display = command_path.display().to_string();
    let warning_detail = gemini_interactive_shell_warning(workdir);
    let build_readiness =
        |status: AgentReadinessStatus, blocking: bool, detail: String| AgentReadiness {
            agent: Agent::Gemini,
            status,
            blocking,
            detail,
            warning_detail: warning_detail.clone(),
            command_path: Some(command_path_display.clone()),
        };

    if let Some(source) = gemini_api_key_source() {
        return build_readiness(
            AgentReadinessStatus::Ready,
            false,
            format!("Gemini CLI is ready with a Gemini API key from {source}."),
        );
    }

    let selected_auth_type = gemini_selected_auth_type(workdir);
    if selected_auth_type.as_deref() == Some("oauth-personal") {
        if let Some(path) = gemini_oauth_credentials_path().filter(|path| path.is_file()) {
            return build_readiness(
                AgentReadinessStatus::Ready,
                false,
                format!(
                    "Gemini CLI is ready with Google login credentials from {}.",
                    display_path_for_user(&path)
                ),
            );
        }
        return build_readiness(
            AgentReadinessStatus::NeedsSetup,
            true,
            format!(
                "Gemini is configured for Google login, but {} is missing.",
                gemini_oauth_credentials_path()
                    .as_deref()
                    .map(display_path_for_user)
                    .unwrap_or_else(|| "~/.gemini/oauth_creds.json".to_owned())
            ),
        );
    }

    if selected_auth_type.as_deref() == Some("gemini-api-key") {
        return build_readiness(
            AgentReadinessStatus::NeedsSetup,
            true,
            gemini_api_key_missing_detail(),
        );
    }

    if selected_auth_type.as_deref() == Some("vertex-ai") {
        if let Some(source) = gemini_vertex_auth_source(workdir) {
            return build_readiness(
                AgentReadinessStatus::Ready,
                false,
                format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
            );
        }
        return build_readiness(
            AgentReadinessStatus::NeedsSetup,
            true,
            "Gemini is configured for Vertex AI, but the required credentials are missing. Set `GOOGLE_API_KEY`, or set both `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`."
                .to_owned(),
        );
    }

    if selected_auth_type.as_deref() == Some("compute-default-credentials") {
        if let Some(source) = gemini_adc_source() {
            return build_readiness(
                AgentReadinessStatus::Ready,
                false,
                format!("Gemini CLI is ready with application default credentials from {source}."),
            );
        }
        return build_readiness(
            AgentReadinessStatus::NeedsSetup,
            true,
            "Gemini is configured for application default credentials, but no ADC file was found. Set `GOOGLE_APPLICATION_CREDENTIALS` or run `gcloud auth application-default login`."
                .to_owned(),
        );
    }

    if let Some(source) = gemini_vertex_auth_source(workdir) {
        return build_readiness(
            AgentReadinessStatus::Ready,
            false,
            format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
        );
    }

    build_readiness(
        AgentReadinessStatus::NeedsSetup,
        true,
        "Gemini CLI needs auth before TermAl can create sessions. Set `GEMINI_API_KEY`, configure Vertex AI env vars, or choose an auth type in `.gemini/settings.json`."
            .to_owned(),
    )
}

/// Returns a Windows warning for Codex shell limitations.
fn codex_windows_shell_warning() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }

    Some(
        "Codex CLI on Windows can still hit upstream PowerShell parser failures for some tool scripts. If you see immediate parser errors, run Codex from WSL for this repo."
            .to_owned(),
    )
}
