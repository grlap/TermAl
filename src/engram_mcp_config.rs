// Engram's per-session MCP stdio descriptor for local agent runtimes.
//
// Owns only selection and exact argv construction. Runtime-specific JSON
// composition remains in `delegation_mcp.rs`; the host control lifecycle and
// project settings validation remain in `engram_host_adapter.rs`.

const ENGRAM_MCP_SERVER_NAME: &str = "engram";

impl AppState {
    fn engram_mcp_stdio_config_for_session(
        &self,
        session_id: &str,
    ) -> Option<TermalDelegationMcpStdioConfig> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        engram_mcp_stdio_config_for_session_locked(&inner, session_id)
    }
}

fn engram_mcp_stdio_config_for_session_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<TermalDelegationMcpStdioConfig> {
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
    let mut args = vec![
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
    if let Some(grant) = settings.work_authority_grant.as_ref() {
        args.extend([
            "--work-authority-grant".to_owned(),
            grant.to_owned(),
        ]);
    }
    Some(TermalDelegationMcpStdioConfig {
        command,
        args,
        env: BTreeMap::new(),
    })
}
