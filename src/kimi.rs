// Kimi Code CLI discovery, launch, readiness and manual-approval admission.
// Owns the native CLI boundary and pre-prompt mode ACK, not credentials,
// model catalogs or the shared ACP lifecycle (acp.rs).
// Added for Kimi Code 2.0.2's `kimi acp` interface; never invoke a shell shim.

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
) -> Result<()> {
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
    Ok(())
}
