// Engram's per-session MCP stdio descriptor for local agent runtimes.
//
// Owns only selection, exact argv construction, and the child environment
// that carries the work-authority grant. Runtime-specific JSON composition
// remains in `delegation_mcp.rs`; the host control lifecycle and project
// settings validation remain in `engram_host_adapter.rs`.

const ENGRAM_MCP_SERVER_NAME: &str = "engram";
// Engram reads the MCP work-authority grant from this variable; the hash is a
// bearer secret and is never placed on the command line.
const ENGRAM_WORK_AUTHORITY_GRANT_ENV: &str = "ENGRAM_WORK_AUTHORITY_GRANT";

struct EngramMcpRuntimeConfig {
    stdio: TermalDelegationMcpStdioConfig,
    installed: EngramMcpInstalledDescriptor,
}

impl AppState {
    #[allow(dead_code)]
    fn engram_mcp_stdio_config_for_session(
        &self,
        session_id: &str,
    ) -> Option<TermalDelegationMcpStdioConfig> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        engram_mcp_stdio_config_for_session_locked(&inner, session_id)
    }

    fn engram_mcp_stdio_config_for_runtime(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
    ) -> Option<TermalDelegationMcpStdioConfig> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let runtime_config = engram_mcp_runtime_config_for_session_locked(&inner, session_id)?;
        let index = inner.find_session_index(session_id)?;
        if !inner.sessions[index]
            .runtime
            .matches_runtime_token(runtime_token)
        {
            return None;
        }
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .engram_mcp_installed = Some(runtime_config.installed);
        Some(runtime_config.stdio)
    }
}

#[allow(dead_code)]
fn engram_mcp_stdio_config_for_session_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<TermalDelegationMcpStdioConfig> {
    engram_mcp_runtime_config_for_session_locked(inner, session_id).map(|config| config.stdio)
}

fn engram_mcp_runtime_config_for_session_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<EngramMcpRuntimeConfig> {
    let session = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))?;
    if !session.is_local_session() {
        return None;
    }
    let project = engram_project_for_session_locked(inner, session_id)?;
    if project.remote_id != LOCAL_REMOTE_ID || inner.engram_project_resets.contains(&project.id) {
        return None;
    }
    let settings = project.engram.as_ref()?;
    if !settings.is_runtime_enabled() {
        return None;
    }
    let command = settings.binary_path.as_deref()?.to_owned();
    let home = settings.home.as_deref()?;
    let project_file = PathBuf::from(&project.root_path).join(".engram-project");
    let args = vec![
        "--project-file".to_owned(),
        project_file.to_string_lossy().into_owned(),
        "--home".to_owned(),
        home.to_owned(),
        "mcp".to_owned(),
        "--actor-id".to_owned(),
        engram_actor_id(session.session.agent),
        "--session-id".to_owned(),
        session_id.to_owned(),
    ];
    // The work-authority grant is a bearer secret. It must never appear on the
    // child command line, where every process listing can read it; Engram's
    // `mcp` subcommand reads it from this environment variable instead
    // (unset = grant-less, malformed = startup failure, never logged).
    let mut env = BTreeMap::new();
    if let Some(grant) = settings.work_authority_grant.as_ref() {
        env.insert(
            ENGRAM_WORK_AUTHORITY_GRANT_ENV.to_owned(),
            grant.to_owned(),
        );
    }
    let installed = EngramMcpInstalledDescriptor {
        binary_path: command.clone(),
        home: home.to_owned(),
        store_key: settings.authority_store_key.clone(),
        work_authority_grant: settings.work_authority_grant.clone(),
    };
    Some(EngramMcpRuntimeConfig {
        stdio: TermalDelegationMcpStdioConfig { command, args, env },
        installed,
    })
}
