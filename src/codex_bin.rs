// Codex executable discovery + web-search event formatters.
//
// **Binary discovery** (`codex_command`, `resolve_codex_executable`,
// `find_command_on_path`, `home_dir`, `display_path_for_user`,
// `resolve_codex_native_binary`, `codex_target_triple`):
//
// Finding the `codex` executable across all supported platforms is
// non-trivial. TermAl tries in order:
//
// 1. The `CODEX_PATH` env var if set (trusted opt-out).
// 2. A native binary bundled under the npm-installed `codex` package
//    in `<npm-prefix>/lib/node_modules/@openai/codex/bin/<triple>/codex`
//    (way faster than the Node shim when present).
// 3. `codex` on `PATH` — the normal case.
//
// `codex_command` wraps the resolved path in a `Command` with the
// right shim handling (npm installs ship `.cmd` / `.bat` shims on
// Windows that must run through `cmd.exe`). `display_path_for_user`
// contracts `~/…` back out of absolute paths for friendlier error
// messages.
//
// **Web-search event formatters**
// (`describe_codex_app_server_web_search_command` /
// `summarize_codex_app_server_web_search_output`):
//
// When Codex's `item/started` or `item/completed` events describe a
// web-search tool call, these format the query + result into the
// `"command started"` / `"command completed"` display strings the
// session sidebar expects. Called from `codex_app_requests.rs`.


/// Handles Codex command.
fn codex_command() -> Result<Command> {
    let exe = resolve_codex_executable()?;

    // On Windows, .cmd/.bat shims (from npm) must be run through cmd.exe.
    #[cfg(windows)]
    {
        if let Some(ext) = exe.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
                let mut cmd = Command::new("cmd.exe");
                cmd.args(["/C", &exe.to_string_lossy()]);
                return Ok(cmd);
            }
        }
    }

    Ok(Command::new(exe))
}

/// Resolves Codex executable.
fn resolve_codex_executable() -> Result<PathBuf> {
    let launcher =
        find_command_on_path("codex").ok_or_else(|| anyhow!("`codex` was not found on PATH"))?;
    Ok(resolve_codex_native_binary(&launcher).unwrap_or(launcher))
}

/// Finds command on path.
fn find_command_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let extensions: &[&str] = &[".exe", ".cmd", ".bat", ""];

    #[cfg(not(windows))]
    let extensions: &[&str] = &[""];

    for dir in std::env::split_paths(&path) {
        for ext in extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}



fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn display_path_for_user(path: &FsPath) -> String {
    if let Some(home) = home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            };
        }
    }
    path.display().to_string()
}

/// Resolves Codex native binary.
fn resolve_codex_native_binary(launcher: &PathBuf) -> Option<PathBuf> {
    let launcher = fs::canonicalize(launcher)
        .ok()
        .unwrap_or_else(|| launcher.clone());
    let package_root = launcher.parent()?.parent()?;
    let node_modules_dir = package_root.join("node_modules").join("@openai");
    let target_triple = codex_target_triple()?;
    let binary_name = if cfg!(windows) { "codex.exe" } else { "codex" };

    let entries = fs::read_dir(node_modules_dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if !name.starts_with("codex-") {
            continue;
        }
        let candidate = entry
            .path()
            .join("vendor")
            .join(target_triple)
            .join("codex")
            .join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Handles Codex target triple.
fn codex_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// Describes Codex app server web search command.
fn describe_codex_app_server_web_search_command(item: &Value) -> String {
    let query = item
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("open_page") => item
            .pointer("/action/url")
            .and_then(Value::as_str)
            .map(|url| format!("Open page: {url}"))
            .unwrap_or_else(|| "Open page".to_owned()),
        Some("find_in_page") => item
            .pointer("/action/pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("Find in page: {pattern}"))
            .unwrap_or_else(|| "Find in page".to_owned()),
        _ => query
            .map(|value| format!("Web search: {value}"))
            .unwrap_or_else(|| "Web search".to_owned()),
    }
}

/// Summarizes Codex app server web search output.
fn summarize_codex_app_server_web_search_output(item: &Value) -> String {
    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("search") => {
            let queries = item
                .pointer("/action/queries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !queries.is_empty() {
                return queries.join("\n");
            }
        }
        Some("open_page") => {
            if let Some(url) = item.pointer("/action/url").and_then(Value::as_str) {
                return format!("Opened {url}");
            }
        }
        Some("find_in_page") => {
            let pattern = item.pointer("/action/pattern").and_then(Value::as_str);
            let url = item.pointer("/action/url").and_then(Value::as_str);
            return match (pattern, url) {
                (Some(pattern), Some(url)) => format!("Searched for `{pattern}` in {url}"),
                (Some(pattern), None) => format!("Searched for `{pattern}`"),
                (None, Some(url)) => format!("Searched within {url}"),
                (None, None) => "Find in page completed".to_owned(),
            };
        }
        _ => {}
    }

    item.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Web search completed")
        .to_owned()
}

