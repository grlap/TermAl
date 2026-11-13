// Antigravity CLI boundary for the Gemini harness: native executable discovery
// and launch arguments only. The shared ACP adapter owns the protocol.
// Authentication and settings belong to the CLI; never inspect or rewrite them.

fn resolve_gemini_executable() -> Option<PathBuf> {
    resolve_gemini_executable_with(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("LOCALAPPDATA").as_deref().map(FsPath::new),
    )
}

fn resolve_gemini_executable_with(
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

fn gemini_acp_command(
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
fn agy_auth_method(initialize: &Value) -> Option<String> {
    let present = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    agy_auth_method_with(
        initialize,
        present("GEMINI_API_KEY"),
        present("GOOGLE_CLOUD_PROJECT") && present("GOOGLE_CLOUD_LOCATION"),
    )
}

fn agy_auth_method_with(initialize: &Value, api_key: bool, platform: bool) -> Option<String> {
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
