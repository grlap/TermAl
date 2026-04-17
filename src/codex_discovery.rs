// Codex thread discovery + import — scans the SQLite thread databases
// Codex CLI keeps under its home directory (`~/.codex/threads.db` plus
// per-install variants) and surfaces matching threads so the user can
// import historical conversations into TermAl as local sessions.
//
// Covers: `DiscoveredCodexThread` projection, scope/home candidate
// resolution, SQLite LIKE-pattern generation for cwd filtering, thread
// DB path resolution, discovered-field parsers (sandbox mode, approval
// policy, reasoning effort), cwd normalization, and the
// `apply_discovered_codex_thread` action that materializes a new session
// from the discovery payload.
//
// Extracted from state.rs into its own `include!()` fragment so state.rs
// stays focused on the core state model.

/// Represents discovered Codex thread.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredCodexThread {
    approval_policy: Option<CodexApprovalPolicy>,
    archived: bool,
    cwd: String,
    id: String,
    model: Option<String>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    title: String,
}

const MAX_DISCOVERED_CODEX_THREADS_PER_HOME: usize = 500;

/// Collects Codex discovery scopes.
fn collect_codex_discovery_scopes(default_workdir: &str, projects: &[Project]) -> Vec<PathBuf> {
    let mut scopes = Vec::new();
    let mut seen = HashSet::new();
    push_codex_home_candidate(
        &mut scopes,
        &mut seen,
        normalize_codex_discovery_path(FsPath::new(default_workdir)),
    );
    for project in projects {
        if project.remote_id == LOCAL_REMOTE_ID {
            push_codex_home_candidate(
                &mut scopes,
                &mut seen,
                normalize_codex_discovery_path(FsPath::new(&project.root_path)),
            );
        }
    }
    scopes
}

/// Handles discover Codex threads.
fn discover_codex_threads(
    default_workdir: &str,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let source_codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .map(|home| home.join(".codex"))
        })
        .or_else(|| Some(PathBuf::from(default_workdir).join(".codex")));
    let termal_codex_root = resolve_termal_codex_discovery_root(default_workdir);
    discover_codex_threads_from_sources(
        source_codex_home.as_deref(),
        &termal_codex_root,
        discovery_scopes,
    )
}

/// Handles discover Codex threads from sources.
fn discover_codex_threads_from_sources(
    source_codex_home: Option<&FsPath>,
    termal_codex_root: &FsPath,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let codex_homes = discover_codex_home_candidates(source_codex_home, termal_codex_root);
    discover_codex_threads_from_homes(&codex_homes, discovery_scopes)
}

/// Handles discover Codex home candidates.
fn discover_codex_home_candidates(
    source_codex_home: Option<&FsPath>,
    termal_codex_root: &FsPath,
) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let mut seen = HashSet::new();

    for scope in ["shared-app-server"] {
        push_codex_home_candidate(&mut homes, &mut seen, termal_codex_root.join(scope));
    }

    let mut extra_termal_homes = fs::read_dir(termal_codex_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| match entry.file_type() {
            Ok(file_type)
                if file_type.is_dir() && codex_home_scope_is_importable(&entry.path()) =>
            {
                Some(entry.path())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    extra_termal_homes.sort();
    for home in extra_termal_homes {
        push_codex_home_candidate(&mut homes, &mut seen, home);
    }

    push_codex_home_candidate(&mut homes, &mut seen, termal_codex_root.to_path_buf());

    if let Some(source_codex_home) = source_codex_home {
        push_codex_home_candidate(&mut homes, &mut seen, source_codex_home.to_path_buf());
    }

    homes
}

/// Handles Codex home scope is importable.
fn codex_home_scope_is_importable(path: &FsPath) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map_or(true, |scope| scope != "repl")
}

/// Pushes Codex home candidate.
fn push_codex_home_candidate(homes: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, home: PathBuf) {
    let key = normalize_codex_discovery_path(&home);
    if seen.insert(key) {
        homes.push(home);
    }
}

/// Resolves TermAl Codex discovery root.
fn resolve_termal_codex_discovery_root(default_workdir: &str) -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_workdir))
        .join(".termal")
        .join("codex-home")
}

/// Handles discover Codex threads from homes.
fn discover_codex_threads_from_homes(
    codex_homes: &[PathBuf],
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let mut threads = Vec::new();
    let mut seen_ids = HashSet::new();

    for codex_home in codex_homes {
        for thread in discover_codex_threads_from_home(codex_home, discovery_scopes)? {
            if seen_ids.insert(thread.id.clone()) {
                threads.push(thread);
            }
        }
    }

    Ok(threads)
}

/// Handles discover Codex threads from home.
fn discover_codex_threads_from_home(
    codex_home: &FsPath,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let Some(database_path) = resolve_codex_threads_database_path(codex_home) else {
        return Ok(Vec::new());
    };
    if discovery_scopes.is_empty() {
        return Ok(Vec::new());
    }

    let connection = rusqlite::Connection::open_with_flags(
        &database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .with_context(|| format!("failed to open `{}`", database_path.display()))?;
    let query_scopes = collect_codex_discovery_query_scope_strings(discovery_scopes);
    let query_scope_patterns = query_scopes
        .iter()
        .flat_map(|scope| codex_discovery_scope_query_patterns(scope))
        .collect::<Vec<_>>();
    let normalized_scopes = discovery_scopes
        .iter()
        .map(|scope| normalize_codex_discovery_path(scope))
        .collect::<Vec<_>>();
    let scope_sql = query_scope_patterns
        .iter()
        .map(|_| "(cwd = ? OR cwd LIKE ? ESCAPE '\\')")
        .collect::<Vec<_>>()
        .join(" OR ");
    let query = format!(
        "select id, cwd, title, sandbox_policy, approval_mode, archived, model, reasoning_effort
         from threads
         where {scope_sql}
         order by updated_at desc
         limit ?"
    );
    let mut statement = connection.prepare(&query)?;
    let mut params = Vec::with_capacity((query_scope_patterns.len() * 2) + 1);
    for (scope, like_pattern) in &query_scope_patterns {
        params.push(rusqlite::types::Value::from(scope.clone()));
        params.push(rusqlite::types::Value::from(like_pattern.clone()));
    }
    params.push(rusqlite::types::Value::from(
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME as i64,
    ));
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        let sandbox_policy: Option<String> = row.get(3)?;
        let approval_mode: Option<String> = row.get(4)?;
        let model: Option<String> = row.get(6)?;
        let reasoning_effort: Option<String> = row.get(7)?;
        Ok(DiscoveredCodexThread {
            approval_policy: approval_mode
                .as_deref()
                .and_then(parse_discovered_codex_approval_policy),
            archived: row.get::<_, i64>(5)? != 0,
            cwd: row.get(1)?,
            id: row.get(0)?,
            model: model
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            reasoning_effort: reasoning_effort
                .as_deref()
                .and_then(parse_discovered_codex_reasoning_effort),
            sandbox_mode: sandbox_policy
                .as_deref()
                .and_then(parse_discovered_codex_sandbox_mode),
            title: row.get::<_, String>(2)?,
        })
    })?;

    let mut threads = Vec::new();
    for row in rows {
        let thread = row?;
        if thread.id.trim().is_empty() || thread.cwd.trim().is_empty() {
            continue;
        }
        if !normalized_scopes.iter().any(|scope| {
            codex_discovery_scope_contains(
                scope.to_string_lossy().as_ref(),
                FsPath::new(&thread.cwd),
            )
        }) {
            continue;
        }
        threads.push(thread);
    }
    Ok(threads)
}

/// Collects Codex discovery query scope strings.
fn collect_codex_discovery_query_scope_strings(discovery_scopes: &[PathBuf]) -> Vec<String> {
    let mut scopes = Vec::new();
    let mut seen = HashSet::new();
    for scope in discovery_scopes {
        let raw = scope.to_string_lossy().to_string();
        if seen.insert(raw.clone()) {
            scopes.push(raw);
        }
        let normalized = normalize_codex_discovery_path(scope)
            .to_string_lossy()
            .to_string();
        if seen.insert(normalized.clone()) {
            scopes.push(normalized);
        }
    }
    scopes
}

/// Handles Codex discovery scope query patterns.
fn codex_discovery_scope_query_patterns(scope: &str) -> Vec<(String, String)> {
    let mut patterns = Vec::new();
    let mut seen = HashSet::new();
    let mut candidates = vec![scope.to_owned()];
    if scope.contains('/') {
        candidates.push(scope.replace('/', "\\"));
    }
    if scope.contains('\\') {
        candidates.push(scope.replace('\\', "/"));
    }

    for candidate in candidates {
        if seen.insert(candidate.clone()) {
            patterns.push((candidate.clone(), codex_discovery_like_pattern(&candidate)));
        }
    }

    patterns
}

/// Handles Codex discovery like pattern.
fn codex_discovery_like_pattern(scope: &str) -> String {
    let escaped_scope = scope
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let separator = if scope.contains('\\') { '\\' } else { '/' };
    if escaped_scope.ends_with(separator) {
        format!("{escaped_scope}%")
    } else {
        format!("{escaped_scope}{separator}%")
    }
}

/// Resolves Codex threads database path.
fn resolve_codex_threads_database_path(codex_home: &FsPath) -> Option<PathBuf> {
    let primary = codex_home.join("state.db");
    if primary
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() > 0)
        .is_some()
    {
        return Some(primary);
    }

    let mut best_candidate: Option<(u64, PathBuf)> = None;
    let entries = fs::read_dir(codex_home).ok()?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let version = name
            .strip_prefix("state_")
            .and_then(|value| value.strip_suffix(".sqlite"))
            .and_then(|value| value.parse::<u64>().ok());
        let Some(version) = version else {
            continue;
        };
        if !path.is_file() {
            continue;
        }

        match &best_candidate {
            Some((current_version, _)) if *current_version >= version => {}
            _ => {
                best_candidate = Some((version, path));
            }
        }
    }

    best_candidate.map(|(_, path)| path)
}

/// Parses discovered Codex sandbox mode.
fn parse_discovered_codex_sandbox_mode(value: &str) -> Option<CodexSandboxMode> {
    let payload: Value = serde_json::from_str(value).ok()?;
    match payload.get("type").and_then(Value::as_str) {
        Some("read-only") => Some(CodexSandboxMode::ReadOnly),
        Some("workspace-write") => Some(CodexSandboxMode::WorkspaceWrite),
        Some("danger-full-access") => Some(CodexSandboxMode::DangerFullAccess),
        _ => None,
    }
}

/// Parses discovered Codex approval policy.
fn parse_discovered_codex_approval_policy(value: &str) -> Option<CodexApprovalPolicy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "untrusted" => Some(CodexApprovalPolicy::Untrusted),
        "on-failure" => Some(CodexApprovalPolicy::OnFailure),
        "on-request" => Some(CodexApprovalPolicy::OnRequest),
        "never" => Some(CodexApprovalPolicy::Never),
        _ => None,
    }
}

/// Parses discovered Codex reasoning effort.
fn parse_discovered_codex_reasoning_effort(value: &str) -> Option<CodexReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(CodexReasoningEffort::None),
        "minimal" => Some(CodexReasoningEffort::Minimal),
        "low" => Some(CodexReasoningEffort::Low),
        "medium" => Some(CodexReasoningEffort::Medium),
        "high" => Some(CodexReasoningEffort::High),
        "xhigh" => Some(CodexReasoningEffort::XHigh),
        _ => None,
    }
}

/// Handles Codex discovery scope contains.
fn codex_discovery_scope_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_codex_discovery_path(FsPath::new(root_path));
    let candidate = normalize_codex_discovery_path(candidate_path);
    candidate == root || candidate.starts_with(root)
}

/// Normalizes Codex discovery path.
fn normalize_codex_discovery_path(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    fs::canonicalize(&resolved).unwrap_or(resolved)
}

/// Applies discovered Codex thread.
fn apply_discovered_codex_thread(
    record: &mut SessionRecord,
    thread: &DiscoveredCodexThread,
    overwrite_prompt_settings: bool,
) {
    set_record_external_session_id(record, Some(thread.id.clone()));
    set_record_codex_thread_state(
        record,
        if thread.archived {
            CodexThreadState::Archived
        } else {
            CodexThreadState::Active
        },
    );

    if overwrite_prompt_settings {
        if let Some(model) = thread.model.as_ref() {
            record.session.model = model.clone();
        }
        if let Some(sandbox_mode) = thread.sandbox_mode {
            record.codex_sandbox_mode = sandbox_mode;
            record.session.sandbox_mode = Some(sandbox_mode);
        }
        if let Some(approval_policy) = thread.approval_policy {
            record.codex_approval_policy = approval_policy;
            record.session.approval_policy = Some(approval_policy);
        }
        if let Some(reasoning_effort) = thread.reasoning_effort {
            record.codex_reasoning_effort = reasoning_effort;
            record.session.reasoning_effort = Some(reasoning_effort);
        }
    }

    if record.session.messages.is_empty() && matches!(record.session.status, SessionStatus::Idle) {
        record.session.preview = if thread.archived {
            "Archived Codex thread ready to reopen.".to_owned()
        } else {
            "Ready to continue this Codex thread.".to_owned()
        };
    }
}
