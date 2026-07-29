// OpenCode-specific executable discovery and readiness diagnostics.
//
// The ACP protocol and OpenCode config-authority reconciliation stay in
// `src/acp.rs` because both participate in the ordered handshake that gates
// prompt dispatch. This fragment owns only the OpenCode CLI boundary
// (`opencode acp`) and executable discovery. Provider credentials are
// intentionally not inspected here: OpenCode can source them from environment,
// enterprise providers, plugins, or `opencode auth login`, and its ACP
// `authenticate` method does not prove that a usable provider credential
// exists.

const OPENCODE_CONFIG_AUTO: &str = "auto";
const MAX_OPENCODE_MODE_CHARS: usize = 128;
const MAX_OPENCODE_MODEL_CHARS: usize = 200;

fn normalize_opencode_model(value: &str) -> Result<String> {
    let model = value.trim();
    if model.is_empty() {
        bail!("OpenCode model cannot be empty");
    }
    if model.chars().count() > MAX_OPENCODE_MODEL_CHARS {
        bail!(
            "OpenCode model must be at most {MAX_OPENCODE_MODEL_CHARS} characters"
        );
    }
    if model.chars().any(char::is_control) {
        bail!("OpenCode model cannot contain control characters");
    }
    Ok(model.to_owned())
}

/// Normalizes a persisted/user-supplied OpenCode mode. Dynamic primary-agent
/// ids are intentionally strings, but remain bounded and single-line because
/// they cross persistence, API, and UI boundaries.
fn normalize_opencode_mode(value: &str) -> Result<String> {
    let mode = value.trim();
    if mode.is_empty() {
        bail!("OpenCode mode cannot be empty");
    }
    if mode.chars().count() > MAX_OPENCODE_MODE_CHARS {
        bail!(
            "OpenCode mode must be at most {MAX_OPENCODE_MODE_CHARS} characters"
        );
    }
    if mode.chars().any(char::is_control) {
        bail!("OpenCode mode cannot contain control characters");
    }
    Ok(mode.to_owned())
}

/// Resolves the OpenCode CLI from PATH.
fn resolve_opencode_executable() -> Option<PathBuf> {
    find_command_on_path("opencode")
}

/// Builds an OpenCode CLI command with npm shim handling on Windows.
fn opencode_command(executable: &FsPath) -> Command {
    #[cfg(windows)]
    {
        if let Some(extension) = executable.extension().and_then(|value| value.to_str()) {
            if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
                let mut command = Command::new("cmd.exe");
                command.args(["/C", &executable.to_string_lossy()]);
                return command;
            }
        }
    }

    Command::new(executable)
}

/// Builds the non-blocking OpenCode readiness record. Provider authentication
/// remains an ordinary Ready detail because the only reliable proof is a real
/// ACP session/prompt; an always-on warning badge would falsely label healthy
/// installations.
fn opencode_agent_readiness() -> AgentReadiness {
    let Some(command_path) = resolve_opencode_executable() else {
        return AgentReadiness {
            agent: Agent::OpenCode,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail:
                "Install the `opencode` CLI and make sure it is on PATH before creating OpenCode sessions."
                    .to_owned(),
            warning_detail: None,
            command_path: None,
        };
    };

    let command_path_display = command_path.display().to_string();
    AgentReadiness {
        agent: Agent::OpenCode,
        status: AgentReadinessStatus::Ready,
        blocking: false,
        detail: format!(
            "OpenCode CLI is available at `{command_path_display}`. Provider access is verified when a real session sends its first prompt; if authentication fails, run `opencode auth login` or configure your provider credentials."
        ),
        warning_detail: None,
        command_path: Some(command_path_display),
    }
}
