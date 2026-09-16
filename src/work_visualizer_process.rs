// New bounded Engram Work read transport. No shell command construction, no
// execution of receipt suggestions, no tracker mutations or store discovery.
// Uses existing process-tree ownership, a process deadline and bounded EOF grace.

fn validate_work_read_binary(connection: &EngramConnectionConfig) -> Result<(), ApiError> {
    // A configured batch script would reinterpret untrusted filters as shell
    // syntax. Reject Windows shell shims; test-only PowerShell fixtures use
    // the existing -File launcher and never a command string.
    let ext = connection
        .binary_path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    if matches!(ext.to_ascii_lowercase().as_str(), "bat" | "cmd")
        || (!cfg!(test) && ext.eq_ignore_ascii_case("ps1"))
    {
        return Err(ApiError::conflict(
            "Work reads require a native Engram executable, not a Windows shell shim",
        ));
    }
    Ok(())
}

fn run_work_read_command(
    connection: &EngramConnectionConfig,
    args: &[String],
) -> Result<Value, ApiError> {
    validate_work_read_binary(connection)?;
    let operation = match args.first().map(String::as_str) {
        Some("ls") => "engram work ls",
        Some("show") => "engram work show",
        Some("memories") => "engram work memories",
        _ => return Err(ApiError::bad_request("Unsupported Work read operation")),
    };
    let mut command = engram_command(&connection.binary_path);
    apply_engram_connection_environment(&mut command, connection);
    command
        .arg("--home")
        .arg(&connection.home)
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("work")
        .arg("--actor-id")
        .arg(&connection.actor_id)
        .arg("--session-id")
        .arg(&connection.session_id)
        .args(args)
        .current_dir(&connection.project_root);
    // Conservative Windows quoting bound, including configured paths and
    // launcher arguments, not just each independently validated filter.
    let command_units: usize = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| 2 * arg.to_string_lossy().encode_utf16().count() + 3)
        .sum();
    if command_units > 30_000 {
        return Err(ApiError::bad_request(
            "Work read command is too large; shorten filters or cursor",
        ));
    }
    let output = run_bounded_read_process(
        &mut command,
        std::time::Instant::now() + ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
        8 * 1024 * 1024,
        true,
    )
    .map_err(|e| ApiError::bad_gateway(format!("{operation}: {e:#}")))?;
    let std::process::Output {
        status,
        stdout,
        stderr,
    } = output;
    if !status.success() {
        let detail = if stderr.is_empty() {
            String::from_utf8_lossy(&stdout)
        } else {
            String::from_utf8_lossy(&stderr)
        };
        let message = format!("{operation}: {status}: {}", detail.trim());
        if detail.contains("work_catalog_cursor_invalid")
            || detail.contains("work_show_cursor_invalid")
        {
            return Err(ApiError::conflict(message));
        }
        return Err(ApiError::bad_gateway(message));
    }
    serde_json::from_slice(&stdout)
        .map_err(|e| ApiError::bad_gateway(format!("{operation}: invalid JSON: {e}")))
}
