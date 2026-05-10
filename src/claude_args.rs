// Claude CLI argument construction + inbound message shape parsing.
//
// Two concerns share this file because they both sit at the Claude
// protocol boundary but are too small to deserve separate modules:
//
// **Outbound argv construction** (called from `claude_spawn.rs`):
//
// - `claude_cli_model_arg` — picks the `--model <slug>` or passes
//   through None to let Claude choose
// - `push_claude_cli_common_args` — shared argv across oneshot /
//   persistent sessions
// - `push_claude_cli_permission_args` — maps our
//   `ClaudeApprovalMode` enum to the right `--permission-mode` flag
// - `claude_cli_oneshot_args` — argv for a one-turn invocation
// - `claude_cli_persistent_args` — argv for a long-lived stdio
//   session
//
// **Inbound message parsing** (called from `claude.rs` when a
// handshake / system message arrives):
//
// - `claude_model_options` — parses the list of models Claude
//   reports it can switch between; feeds the UI's model picker
// - `claude_agent_commands` — parses the slash-command catalog
//   Claude exposes; feeds the UI's command palette
// - `normalize_claude_agent_command_description` — splits
//   multi-line descriptions into summary + details for the UI
// - `claude_model_badges` — extracts pricing/availability badges
//   from a model entry
// - `parse_claude_effort_level` — string → `ClaudeEffortLevel`
//   enum mapping
//
// Parsing on the Claude side uses duck-typed `serde_json::Value`
// lookups (vs Codex's typed JSON-RPC structs) because the Claude
// CLI's message shape changes with CLI version and we want to
// degrade gracefully when new / unknown fields appear.


/// Returns the Claude CLI model argument, or None to use Claude's own default selector.
fn claude_cli_model_arg(model: &str) -> Option<&str> {
    let model = model.trim();
    if model.is_empty() || model.eq_ignore_ascii_case("default") {
        return None;
    }
    Some(model)
}

enum ClaudeCliSessionArg<'a> {
    Resume(&'a str),
    SessionId(&'a str),
}

fn push_claude_cli_common_args(
    args: &mut Vec<String>,
    model: &str,
) {
    if let Some(model) = claude_cli_model_arg(model) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args.extend([
        "-p".to_owned(),
        "--verbose".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
    ]);
}

fn push_claude_cli_permission_args(
    args: &mut Vec<String>,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
) {
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        args.extend(["--permission-mode".to_owned(), permission_mode.to_owned()]);
    }
    if let Some(effort) = effort.as_cli_value() {
        args.extend(["--effort".to_owned(), effort.to_owned()]);
    }
}

fn claude_cli_oneshot_args(
    model: &str,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    session_arg: ClaudeCliSessionArg<'_>,
) -> Vec<String> {
    let mut args = Vec::new();
    push_claude_cli_common_args(&mut args, model);
    args.push("--include-partial-messages".to_owned());
    push_claude_cli_permission_args(&mut args, approval_mode, effort);
    match session_arg {
        ClaudeCliSessionArg::Resume(session_id) => {
            args.extend(["--resume".to_owned(), session_id.to_owned()]);
        }
        ClaudeCliSessionArg::SessionId(session_id) => {
            args.extend(["--session-id".to_owned(), session_id.to_owned()]);
        }
    }
    args
}

fn claude_cli_persistent_args(
    model: &str,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    push_claude_cli_common_args(&mut args, model);
    args.extend([
        "--input-format".to_owned(),
        "stream-json".to_owned(),
        "--include-partial-messages".to_owned(),
        "--permission-prompt-tool".to_owned(),
        "stdio".to_owned(),
    ]);
    push_claude_cli_permission_args(&mut args, approval_mode, effort);
    if let Some(resume_session_id) = resume_session_id {
        args.extend(["--resume".to_owned(), resume_session_id.to_owned()]);
    }
    args
}

/// Handles Claude model options.
fn claude_model_options(message: &Value) -> Option<Vec<SessionModelOption>> {
    let models = message.pointer("/response/response/models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(|entry| {
                let value = entry
                    .get("value")
                    .or_else(|| entry.get("model"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?
                    .to_owned();
                let label = entry
                    .get("displayName")
                    .or_else(|| entry.get("label"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .unwrap_or(&value)
                    .to_owned();
                let description = entry
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(str::to_owned);
                Some(SessionModelOption {
                    label,
                    value,
                    description,
                    badges: claude_model_badges(entry),
                    supported_claude_effort_levels: entry
                        .get("supportedEffortLevels")
                        .and_then(Value::as_array)
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(Value::as_str)
                                .filter_map(parse_claude_effort_level)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    default_reasoning_effort: None,
                    supported_reasoning_efforts: Vec::new(),
                })
            })
            .collect(),
    )
}

/// Handles Claude agent commands.
fn claude_agent_commands(message: &Value) -> Option<Vec<AgentCommand>> {
    let commands = message.pointer("/response/response/commands")?.as_array()?;
    let parsed = commands
        .iter()
        .filter_map(|entry| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_owned();
            let raw_description = entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let (description, source) = normalize_claude_agent_command_description(raw_description);
            let argument_hint = entry
                .get("argumentHint")
                .or_else(|| entry.get("argument_hint"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            Some(AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: name.clone(),
                description,
                content: format!("/{name}"),
                source,
                argument_hint,
                resolver_frontmatter: None,
            })
        })
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        return None;
    }
    Some(dedupe_agent_commands(parsed))
}

/// Normalizes Claude agent command description.
fn normalize_claude_agent_command_description(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    for (suffix, source) in [
        ("(bundled)", "Claude bundled command"),
        ("(project)", "Claude project command"),
        ("(user)", "Claude user command"),
    ] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return (stripped.trim().to_owned(), source.to_owned());
        }
    }
    (trimmed.to_owned(), "Claude native command".to_owned())
}

/// Handles Claude model badges.
fn claude_model_badges(entry: &Value) -> Vec<String> {
    let mut badges = Vec::new();
    let display_name = entry
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if entry.get("value").and_then(Value::as_str) == Some("default")
        || display_name.contains("recommended")
    {
        badges.push("Recommended".to_owned());
    }
    if entry
        .get("supportsEffort")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || entry
            .get("supportedEffortLevels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty())
    {
        badges.push("Effort".to_owned());
    }
    if entry
        .get("supportsAdaptiveThinking")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        badges.push("Adaptive".to_owned());
    }
    if entry
        .get("supportsFastMode")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        badges.push("Fast".to_owned());
    }
    badges
}

/// Parses Claude effort level.
fn parse_claude_effort_level(value: &str) -> Option<ClaudeEffortLevel> {
    match value.trim() {
        "default" => Some(ClaudeEffortLevel::Default),
        "low" => Some(ClaudeEffortLevel::Low),
        "medium" => Some(ClaudeEffortLevel::Medium),
        "high" => Some(ClaudeEffortLevel::High),
        "max" => Some(ClaudeEffortLevel::Max),
        _ => None,
    }
}
