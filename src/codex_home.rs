// Codex home directory setup + shared runtime stderr formatting.
//
// Codex keeps per-user state under `$CODEX_HOME` (or
// `~/.codex` by default): auth tokens, persisted threads,
// settings, MCP configuration, etc. TermAl needs a *writable*
// Codex home that is isolated from the user's real `~/.codex`
// for two reasons:
//
// 1. TermAl sessions should not accidentally mutate the user's
//    directly-invoked `codex` state (e.g. the thread index).
// 2. Different scopes within TermAl (one per session workdir,
//    one per project, etc.) want their own thread history.
//
// The `resolve_termal_codex_home` path lives under TermAl's data
// dir with a scope-specific subdirectory, and
// `prepare_termal_codex_home` + `seed_termal_codex_home_from` copy
// the user's baseline Codex home (auth tokens, config) into it on
// first use. The `sync_codex_home_*` helpers are the recursive copy
// routines that preserve permissions and skip already-current files.
//
// `runtime_stderr_timestamp` + `format_runtime_stderr_prefix` are
// shared formatters used by the Codex + Claude + ACP stderr forwarders
// to tag every line with `[YYYY-MM-DDTHH:MM:SS label]` so interleaved
// subprocess output is navigable.

const TERMAL_CODEX_AGENTS_SECTION_START: &str =
    "<!-- BEGIN TERMAL MANAGED COORDINATION INSTRUCTIONS -->";
const TERMAL_CODEX_AGENTS_SECTION_END: &str =
    "<!-- END TERMAL MANAGED COORDINATION INSTRUCTIONS -->";
const TERMAL_CODEX_AGENTS_SECTION: &str = r#"<!-- BEGIN TERMAL MANAGED COORDINATION INSTRUCTIONS -->
## TermAl coordination

This Codex session is hosted by TermAl. Its environment provides
`TERMAL_SESSION_ID`, `TERMAL_BASE_URL`, and `TERMAL_CLI` (the absolute path to
the running TermAl executable). When the TermAl delegation MCP tools are not
available, invoke that executable from the shell (PowerShell:
`& $env:TERMAL_CLI`; POSIX: `"$TERMAL_CLI"`). The coordination CLI defaults
`--as-session` and `--base-url` from those environment values.

Use the durable mailbox protocol in this order:

1. Run `mailbox list --json` and record your participant's
   `processedThrough` cursor.
2. Run `mailbox read --mailbox-id <id> --after <processedThrough> --json`.
3. Process each body. Reply, when needed, with `mailbox send --to <session>
   --message ... --idempotency-key <stable-key> --json`. Derive a stable key
   from your session and the inbound message or task, and retry the exact same
   intent with the same key after an ambiguous failure.
4. After processing, run `mailbox acknowledge --mailbox-id <id> --expected
   <processedThrough> --through <last-sequence> --json`. On a CAS conflict,
   list again and continue from the newly observed cursor.

Prefer `--json` for automation. Exit code 0 means success, 2 means a usage
error before a request, and 1 means a request or response-contract failure.
The loopback CLI identity is a local misuse guard, not an authentication
boundary; do not claim another session id.
<!-- END TERMAL MANAGED COORDINATION INSTRUCTIONS -->"#;


/// Resolves source Codex home dir.
fn resolve_source_codex_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = resolve_home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".codex"))
}

/// Resolves TermAl data dir.
fn resolve_termal_data_dir(default_workdir: &str) -> PathBuf {
    let base = resolve_home_dir().unwrap_or_else(|| PathBuf::from(default_workdir));
    base.join(".termal")
}

/// Resolves home dir.
fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Returns the current stderr log timestamp.
fn runtime_stderr_timestamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// Formats a runtime stderr log prefix.
fn format_runtime_stderr_prefix(label: &str, timestamp: &str) -> String {
    format!("{label} stderr [{timestamp}]>")
}

/// Returns whether a runtime stderr line should be forwarded to TermAl stderr.
fn should_forward_runtime_stderr_line(label: &str, line: &str) -> bool {
    !(label == "codex" && is_codex_closed_stdin_tool_router_diagnostic(line))
}

fn is_codex_closed_stdin_tool_router_diagnostic(line: &str) -> bool {
    line.contains("ERROR codex_core::tools::router:")
        && line.contains("write_stdin failed:")
        && line.contains("stdin is closed for this session")
}

/// Resolves TermAl Codex home.
fn resolve_termal_codex_home(default_workdir: &str, scope: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir)
        .join("codex-home")
        .join(scope)
}

/// Prepares TermAl Codex home.
fn prepare_termal_codex_home(default_workdir: &str, scope: &str) -> Result<PathBuf> {
    let target_home = resolve_termal_codex_home(default_workdir, scope);
    fs::create_dir_all(&target_home)
        .with_context(|| format!("failed to create `{}`", target_home.display()))?;
    match resolve_source_codex_home_dir() {
        Ok(source_home) => seed_termal_codex_home_from(&source_home, &target_home)?,
        Err(_) => {
            let target_agents = target_home.join("AGENTS.md");
            write_termal_codex_agents_file(Some(&target_agents), &target_agents)?;
        }
    }
    Ok(target_home)
}

/// Seeds TermAl Codex home from.
fn seed_termal_codex_home_from(source_home: &FsPath, target_home: &FsPath) -> Result<()> {
    fs::create_dir_all(target_home)
        .with_context(|| format!("failed to create `{}`", target_home.display()))?;
    let source_exists = source_home.exists();
    let source_home = fs::canonicalize(source_home).unwrap_or_else(|_| source_home.to_path_buf());
    let target_home = fs::canonicalize(target_home).unwrap_or_else(|_| target_home.to_path_buf());

    if source_exists && source_home != target_home {
        for name in [
            "auth.json",
            "config.toml",
            "models_cache.json",
            ".codex-global-state.json",
        ] {
            sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
        }

        for name in ["rules", "memories", "skills"] {
            sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
        }
    }

    let target_agents = target_home.join("AGENTS.md");
    let source_agents = source_exists
        .then(|| source_home.join("AGENTS.md"))
        .unwrap_or_else(|| target_agents.clone());
    write_termal_codex_agents_file(Some(&source_agents), &target_agents)?;

    Ok(())
}

fn strip_termal_codex_agents_section(contents: &str) -> String {
    let mut remaining = contents;
    let mut stripped = String::with_capacity(contents.len());
    while let Some(start) = remaining.find(TERMAL_CODEX_AGENTS_SECTION_START) {
        stripped.push_str(&remaining[..start]);
        let section_tail = &remaining[start + TERMAL_CODEX_AGENTS_SECTION_START.len()..];
        let Some(end_offset) = section_tail.find(TERMAL_CODEX_AGENTS_SECTION_END) else {
            stripped.push_str(&remaining[start..]);
            return stripped;
        };
        if section_tail
            .find(TERMAL_CODEX_AGENTS_SECTION_START)
            .is_some_and(|next_start| next_start < end_offset)
        {
            stripped.push_str(TERMAL_CODEX_AGENTS_SECTION_START);
            remaining = section_tail;
            continue;
        }
        remaining = &section_tail[end_offset + TERMAL_CODEX_AGENTS_SECTION_END.len()..];
    }
    stripped.push_str(remaining);
    stripped
}

fn write_termal_codex_agents_file(source: Option<&FsPath>, target: &FsPath) -> Result<()> {
    let source_contents = match source {
        Some(source) if source == target => fs::read_to_string(target).unwrap_or_default(),
        Some(source) => match fs::read_to_string(source) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read `{}`", source.display()));
            }
        },
        None => String::new(),
    };
    let user_contents = strip_termal_codex_agents_section(&source_contents);
    let user_contents = user_contents.trim_end_matches(['\r', '\n']);
    let contents = if user_contents.is_empty() {
        format!("{TERMAL_CODEX_AGENTS_SECTION}\n")
    } else {
        format!("{user_contents}\n\n{TERMAL_CODEX_AGENTS_SECTION}\n")
    };

    if fs::read_to_string(target).ok().as_deref() == Some(contents.as_str()) {
        return Ok(());
    }
    if target.is_dir() {
        fs::remove_dir_all(target)
            .with_context(|| format!("failed to remove `{}`", target.display()))?;
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    fs::write(target, contents)
        .with_context(|| format!("failed to write `{}`", target.display()))?;
    Ok(())
}

/// Syncs Codex home entry.
fn sync_codex_home_entry(source: &FsPath, target: &FsPath) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }

    let metadata =
        fs::metadata(source).with_context(|| format!("failed to read `{}`", source.display()))?;

    if metadata.is_dir() {
        sync_codex_home_directory(source, target)
    } else if metadata.is_file() {
        sync_codex_home_file(source, target, &metadata)
    } else {
        Ok(())
    }
}

/// Syncs Codex home directory.
fn sync_codex_home_directory(source: &FsPath, target: &FsPath) -> Result<()> {
    if target.is_file() {
        fs::remove_file(target)
            .with_context(|| format!("failed to remove `{}`", target.display()))?;
    }

    fs::create_dir_all(target)
        .with_context(|| format!("failed to create `{}`", target.display()))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read `{}`", source.display()))?
    {
        let entry = entry?;
        sync_codex_home_entry(&entry.path(), &target.join(entry.file_name()))?;
    }

    Ok(())
}

/// Syncs Codex home file.
fn sync_codex_home_file(
    source: &FsPath,
    target: &FsPath,
    source_metadata: &fs::Metadata,
) -> Result<()> {
    let should_copy = match fs::metadata(target) {
        Ok(target_metadata) => {
            if target_metadata.is_dir() {
                fs::remove_dir_all(target)
                    .with_context(|| format!("failed to remove `{}`", target.display()))?;
                true
            } else if source_metadata.len() != target_metadata.len() {
                true
            } else {
                match (
                    source_metadata.modified().ok(),
                    target_metadata.modified().ok(),
                ) {
                    (Some(source_modified), Some(target_modified)) => {
                        source_modified > target_modified
                    }
                    _ => false,
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read `{}`", target.display()));
        }
    };

    if !should_copy {
        return Ok(());
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            source.display(),
            target.display()
        )
    })?;
    fs::set_permissions(target, source_metadata.permissions())
        .with_context(|| format!("failed to update permissions on `{}`", target.display()))?;
    Ok(())
}
