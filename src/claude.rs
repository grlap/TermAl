// Claude Code CLI turn processing.
//
// Covers the Claude Code stdio protocol parser used by the long-lived
// `spawn_claude_runtime` process, the per-turn state machine
// (`ClaudeTurnState`, `ClaudeToolUse`, `ClaudeToolPermissionRequest`), event
// dispatch from the NDJSON protocol, tool-use bookkeeping, tool-result routing
// (bash vs file + task), approval handling, streamed assistant text
// reconciliation (delta + completed), thinking-line splitting, and the
// description/summary helpers used to render tool requests in the transcript.
//
// Extracted from turns.rs into its own `include!()` fragment so turns.rs
// stays focused on the TurnRecorder abstraction + shared helpers used
// across agents (error summarization, preview text, command language
// inference, prompt-image parsing, etc.).

/// Tracks Claude turn state.
#[derive(Default)]
struct ClaudeTurnState {
    approval_keys_this_turn: HashSet<String>,
    parallel_agent_group_key: Option<String>,
    parallel_agent_order: Vec<String>,
    parallel_agents: HashMap<String, ParallelAgentProgress>,
    permission_denied_this_turn: bool,
    pending_tools: HashMap<String, ClaudeToolUse>,
    streamed_assistant_text: String,
    saw_text_delta: bool,
}

/// Represents Claude tool use.
struct ClaudeToolUse {
    command: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    name: String,
    subagent_type: Option<String>,
}

/// Represents the Claude tool permission request payload.
struct ClaudeToolPermissionRequest {
    detail: String,
    permission_mode_for_session: Option<String>,
    request_id: String,
    title: String,
    tool_name: String,
    tool_input: Value,
}

/// Classifies Claude control request.
fn classify_claude_control_request(
    message: &Value,
    state: &mut ClaudeTurnState,
    approval_mode: ClaudeApprovalMode,
) -> Result<Option<ClaudeControlRequestAction>> {
    let Some(request) = parse_claude_tool_permission_request(message) else {
        return Ok(None);
    };

    let command = describe_claude_tool_request(&request);
    let key = format!("{}\n{}\n{}", request.request_id, request.title, command);
    if !state.approval_keys_this_turn.insert(key) {
        return Ok(None);
    }

    Ok(Some(match approval_mode {
        ClaudeApprovalMode::Ask => ClaudeControlRequestAction::QueueApproval {
            title: request.title,
            command,
            detail: request.detail,
            approval: ClaudePendingApproval {
                permission_mode_for_session: request.permission_mode_for_session,
                request_id: request.request_id,
                tool_input: request.tool_input,
            },
        },
        ClaudeApprovalMode::AutoApprove => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
                request_id: request.request_id,
                updated_input: request.tool_input,
            })
        }
        ClaudeApprovalMode::ReadOnlyAutoApprove => {
            ClaudeControlRequestAction::Respond(read_only_claude_permission_decision(request))
        }
        ClaudeApprovalMode::Plan => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
                request_id: request.request_id,
                message: "TermAl denied this tool request because Claude is in plan mode."
                    .to_owned(),
            })
        }
    }))
}

fn read_only_claude_permission_decision(
    request: ClaudeToolPermissionRequest,
) -> ClaudePermissionDecision {
    if claude_tool_permission_request_is_read_only(&request) {
        return ClaudePermissionDecision::Allow {
            request_id: request.request_id,
            updated_input: request.tool_input,
        };
    }

    ClaudePermissionDecision::Deny {
        request_id: request.request_id,
        message:
            "TermAl denied this tool request because this Claude reviewer delegation is read-only."
                .to_owned(),
    }
}

// Read-only Claude reviewer children need unattended review commands, but the
// parser is intentionally conservative: unsupported shell syntax denies by
// default, and only simple stderr-to-dev-null redirection is tolerated.
fn claude_tool_permission_request_is_read_only(request: &ClaudeToolPermissionRequest) -> bool {
    match request.tool_name.as_str() {
        "Read" | "LS" | "Glob" | "Grep" => true,
        "Bash" => request
            .tool_input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(claude_bash_command_is_read_only),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => false,
        _ => false,
    }
}

fn claude_bash_command_is_read_only(command: &str) -> bool {
    // Detect background separators on the ORIGINAL command, before the
    // `2>/dev/null` strip below. That strip is a naive text replace, so on input
    // like `echo x \2>/dev/null& touch y` it would delete `2>/dev/null` and leave
    // `\&`, making the real background `&` look escaped to the (escape-aware)
    // separator gate. Bash escapes the `2`, not the `&`.
    if claude_bash_command_has_background_separator(command) {
        return false;
    }

    let normalized = command
        .replace("2> /dev/null", "")
        .replace("2>/dev/null", "");
    if normalized.contains('\n')
        || normalized.contains('\r')
        || normalized.contains(';')
        || normalized.contains('>')
        || normalized.contains('<')
        || normalized.contains('`')
        || normalized.contains("$(")
    {
        return false;
    }

    let pipe_normalized = normalized.replace("&&", "|").replace("||", "|");
    let segments: Vec<&str> = pipe_normalized.split('|').map(str::trim).collect();

    // `cd <dir>` retargets git exactly like `-C`/`--git-dir` do: a reviewed repo
    // fixture's on-disk `.git/config` can carry `core.fsmonitor`/`diff.external`
    // exec sinks. A command that both changes directory and runs git therefore
    // fails closed, mirroring the git global-option denial.
    let changes_directory = segments
        .iter()
        .any(|segment| *segment == "cd" || segment.starts_with("cd "));
    let runs_git = segments
        .iter()
        .any(|segment| *segment == "git" || segment.starts_with("git "));
    if changes_directory && runs_git {
        return false;
    }

    for segment in segments {
        if segment.is_empty() {
            return false;
        }
        if segment == "true" || segment == ":" {
            continue;
        }
        if segment == "pwd" || segment.starts_with("cd ") {
            continue;
        }

        let Some(tokens) = claude_bash_segment_tokens(segment) else {
            return false;
        };
        let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        if !claude_bash_tokens_are_read_only(&tokens) {
            return false;
        }
    }

    true
}

fn claude_bash_command_has_background_separator(command: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut characters = command.chars().peekable();

    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            // Double quotes still process `\`; single quotes do not. Either way a
            // `\"` / `\\` must not be mistaken for the closing quote.
            if active_quote == '"' && character == '\\' {
                characters.next();
                continue;
            }
            if character == active_quote {
                quote = None;
            }
            continue;
        }

        match character {
            // A backslash escapes the next character, so `\"` / `\&` are literals
            // and must not open a quote or count as a background separator. The
            // tokenizer already de-escapes; this gate must agree, or an escaped
            // quote hides a trailing `& <command>` from the read-only check.
            '\\' => {
                characters.next();
            }
            '\'' | '"' => quote = Some(character),
            '&' => {
                if characters.peek() == Some(&'&') {
                    characters.next();
                } else {
                    return true;
                }
            }
            _ => {}
        }
    }

    false
}

fn claude_bash_tokens_are_read_only(tokens: &[&str]) -> bool {
    let Some(command) = tokens.first().copied() else {
        return false;
    };

    let read_only_commands = ["cat", "echo", "grep", "head", "ls", "nl", "pwd", "tail", "wc"];
    if read_only_commands.contains(&command) {
        return true;
    }

    if command == "date" {
        return claude_date_tokens_are_read_only(tokens);
    }

    if command == "rg" {
        return claude_rg_tokens_are_read_only(tokens);
    }

    if command == "find" {
        return claude_find_tokens_are_read_only(tokens);
    }

    if command == "sed" {
        return claude_sed_tokens_are_read_only(tokens);
    }

    if command == "git" {
        return claude_git_tokens_are_read_only(tokens);
    }

    false
}

fn claude_bash_segment_tokens(segment: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    // The permission checker MUST tokenize the way bash will, including backslash
    // escaping. Otherwise a reviewer command like `git diff --out\put=/x`
    // tokenizes here as the harmless literal `--out\put=/x` (which no deny-list
    // matches) yet bash strips the backslash and runs the denied `--output=/x`.
    // Every read-only check downstream depends on this equivalence.
    let mut characters = segment.chars();
    while let Some(character) = characters.next() {
        if let Some(active_quote) = quote {
            // Inside double quotes bash still unescapes `"`, `\`, `$`, `` ` ``;
            // every other backslash stays literal. Single quotes escape nothing.
            if active_quote == '"' && character == '\\' {
                match characters.next() {
                    Some(next @ ('"' | '\\' | '$' | '`')) => current.push(next),
                    Some(next) => {
                        current.push('\\');
                        current.push(next);
                    }
                    None => return None,
                }
                continue;
            }
            // Parameter/command expansion stays active inside double quotes; we
            // cannot predict the expanded argv, so fail closed.
            if active_quote == '"' && matches!(character, '$' | '`') {
                return None;
            }
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }

        match character {
            // Outside quotes, `\<char>` is the literal char (bash drops the
            // backslash) and `\<newline>` is a line continuation.
            '\\' => match characters.next() {
                Some('\n') => {}
                Some(next) => current.push(next),
                None => return None,
            },
            '\'' | '"' => quote = Some(character),
            // Shell expansion / subshell / glob syntax the checker does not
            // emulate: `$'\x2d'` (ANSI-C), `$VAR`/`${..}`/`$(..)` expansion,
            // backtick command substitution, `{a,b}` brace expansion, `(` /
            // process substitution, and unquoted globs (`*`/`?`/`[`, which can
            // expand to a committed filename that looks like an option, e.g.
            // `git diff *` with a tracked `--output=x` file). All rewrite the
            // argv before execution, so classifying the literal text would be
            // unsound. Fail closed instead.
            '$' | '`' | '{' | '(' | '*' | '?' | '[' => return None,
            character if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }

    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn claude_date_tokens_are_read_only(tokens: &[&str]) -> bool {
    tokens
        .iter()
        .skip(1)
        .all(|token| token.starts_with('+') || matches!(*token, "-u" | "--utc"))
}

fn claude_rg_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens
        .iter()
        .skip(1)
        .any(|token| *token == "--pre" || token.starts_with("--pre="))
}

fn claude_find_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().any(|token| {
        matches!(
            *token,
            "-delete" | "-exec" | "-execdir" | "-fls" | "-fprint" | "-fprint0" | "-fprintf"
                | "-ok" | "-okdir"
        )
    })
}

fn claude_sed_tokens_are_read_only(tokens: &[&str]) -> bool {
    let mut saw_script_option = false;
    let mut positional_script_checked = false;

    for (index, token) in tokens.iter().enumerate().skip(1) {
        if *token == "-i"
            || *token == "--in-place"
            || token.starts_with("-i")
            || token.starts_with("--in-place=")
            || *token == "-f"
            || *token == "--file"
            || token.starts_with("-f")
            || token.starts_with("--file=")
        {
            return false;
        }

        if let Some(script) = token.strip_prefix("-e") {
            saw_script_option = true;
            let script = if script.is_empty() {
                tokens.get(index + 1).copied().unwrap_or_default()
            } else {
                script
            };
            if claude_sed_script_can_write(script) {
                return false;
            }
            continue;
        }

        if token.starts_with('-') {
            continue;
        }

        if !saw_script_option && !positional_script_checked {
            positional_script_checked = true;
            if claude_sed_script_can_write(token) {
                return false;
            }
        }
    }

    true
}

fn claude_sed_script_can_write(script: &str) -> bool {
    let mut command_start = true;
    let mut escaped = false;
    let mut characters = script.chars().peekable();

    while let Some(character) = characters.next() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character.is_whitespace() && command_start {
            continue;
        }
        if character == ';' || character == '\n' {
            command_start = true;
            continue;
        }

        if !command_start {
            continue;
        }

        match character {
            'w' | 'W' | 'e' => return true,
            '0'..='9' | ',' | '$' => continue,
            '/' => {
                if !claude_skip_sed_address_regex(&mut characters, '/') {
                    return false;
                }
                continue;
            }
            's' | 'y' => {
                let Some(delimiter) = characters.next() else {
                    return false;
                };
                if delimiter == '\\' || delimiter == '\n' {
                    return false;
                }
                let separator_count = if character == 's' { 2 } else { 1 };
                if claude_skip_sed_delimited_sections(&mut characters, delimiter, separator_count)
                    && character == 's'
                    && claude_sed_substitution_flags_can_write(&mut characters)
                {
                    return true;
                }
                command_start = false;
            }
            _ => command_start = false,
        }
    }

    false
}

fn claude_skip_sed_address_regex<I>(
    characters: &mut std::iter::Peekable<I>,
    delimiter: char,
) -> bool
where
    I: Iterator<Item = char>,
{
    let mut escaped = false;
    for character in characters.by_ref() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == delimiter {
            return true;
        }
    }
    false
}

fn claude_skip_sed_delimited_sections<I>(
    characters: &mut std::iter::Peekable<I>,
    delimiter: char,
    separator_count: usize,
) -> bool
where
    I: Iterator<Item = char>,
{
    let mut escaped = false;
    let mut seen_separators = 0;
    for character in characters.by_ref() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == delimiter {
            seen_separators += 1;
            if seen_separators == separator_count {
                return true;
            }
        }
    }
    false
}

fn claude_sed_substitution_flags_can_write<I>(characters: &mut std::iter::Peekable<I>) -> bool
where
    I: Iterator<Item = char>,
{
    while let Some(character) = characters.peek().copied() {
        if character == ';' || character == '\n' {
            return false;
        }
        if character.is_whitespace() {
            characters.next();
            continue;
        }
        if matches!(character, 'w' | 'W' | 'e') {
            return true;
        }
        characters.next();
    }
    false
}

fn claude_git_tokens_are_read_only(tokens: &[&str]) -> bool {
    // Skip a conservative allow-list of leading git global options (e.g.
    // `git --no-pager log`) before reading the subcommand. Unknown leading
    // options fail closed, so this never widens the allowed subcommand set.
    //
    // Every option that can make git load configuration from an
    // attacker-influenced source is deliberately excluded, because several
    // config keys (`diff.external`, `core.fsmonitor`, `core.pager`) execute
    // external programs during otherwise read-only subcommands:
    //
    // * `-c <name>=<value>` injects such a key inline.
    // * `-C`/`--git-dir`/`--work-tree`/`--namespace` retarget the repository.
    //   Only the literal `.git` name is un-committable, so a reviewed change
    //   can track a bare-repo fixture and `git --git-dir=fixture.git
    //   --work-tree=. status` would run that config's `core.fsmonitor` --
    //   reviewed content executing code inside this read-only sandbox.
    // * `--paginate`/`-p` force the pager, making `core.pager` an exec sink.
    // * `--exec-path[=...]` repoints git at another binary.
    //
    // Re-enabling `-C` requires TermAl to neutralize those sinks in the
    // reviewer child's git environment; the checker only approves the agent's
    // verbatim command and cannot do it here.
    let mut index = 1;
    while let Some(option) = tokens.get(index).copied() {
        match option {
            // Flag-only global options that cannot retarget the repository,
            // force a pager, or relocate the git binary.
            "--no-pager" | "-P" | "--no-optional-locks" => index += 1,
            // First non-option token is the subcommand.
            _ if !option.starts_with('-') => break,
            // Any other leading option is unknown -> fail closed.
            _ => return false,
        }
    }

    // Rebuild `[git, subcommand, args...]` so the downstream helpers, which use
    // `.skip(2)`, keep their positional assumptions. A value-taking option with
    // no following subcommand yields just `["git"]`, so `get(1)` denies below.
    let normalized: Vec<&str> = std::iter::once("git")
        .chain(tokens.get(index..).unwrap_or_default().iter().copied())
        .collect();

    let Some(subcommand) = normalized.get(1).copied() else {
        return false;
    };

    // `--help` / `-h` dispatch through `git help`, which launches a configured
    // man/browser viewer (a shell-evaluated exec sink) even on read-only
    // subcommands like `git blame --help`. Deny it, and its abbreviations,
    // everywhere.
    if normalized
        .iter()
        .skip(2)
        .any(|token| *token == "-h" || claude_git_long_option_abbreviates(token, "--help"))
    {
        return false;
    }

    match subcommand {
        // `shortlog` (and defensively the other listings) accept `--output
        // <path>`, which writes/truncates an arbitrary file; route every listing
        // subcommand through the same output/exec-sink denial as diff/log/show.
        "diff" | "log" | "show" | "blame" | "describe" | "ls-files" | "rev-parse" | "shortlog"
        | "status" => claude_git_output_tokens_are_read_only(&normalized),
        "grep" => claude_git_grep_tokens_are_read_only(&normalized),
        // Only the listing form of `remote`; `add`/`remove`/`set-url`/`prune`
        // and friends are non-flag tokens and fail this check.
        "remote" => normalized
            .iter()
            .skip(2)
            .all(|token| matches!(*token, "-v" | "--verbose")),
        // A read-only `git branch` is only ever a listing. A deny-list cannot be
        // sound here: git parses clustered short options (`-quorigin/main` is
        // `-q -u origin/main`, which sets the upstream) and expands unambiguous
        // long-option abbreviations (`--uns` -> `--unset-upstream`, `--edi` ->
        // `--edit-description`). Allow an exact listing-only set instead, so any
        // cluster, abbreviation, branch name, or unknown option fails closed.
        "branch" => normalized.iter().skip(2).all(|token| {
            matches!(
                *token,
                "-a" | "--all"
                    | "-r" | "--remotes"
                    | "-v" | "-vv" | "--verbose"
                    | "-l" | "--list"
                    | "--show-current"
                    | "--no-color"
            ) || token.starts_with("--sort=")
                || token.starts_with("--format=")
                || token.starts_with("--contains=")
                || token.starts_with("--no-contains=")
                || token.starts_with("--merged=")
                || token.starts_with("--no-merged=")
                || token.starts_with("--points-at=")
        }),
        _ => false,
    }
}

/// Returns whether `token` is `option`, its `option=value` form, or any prefix
/// git would expand to `option`. Git accepts unambiguous long-option
/// abbreviations (`--ext` -> `--ext-diff`, `--textc` -> `--textconv`), so an
/// exact-match deny-list is unsound; every abbreviation must fail closed too.
fn claude_git_long_option_abbreviates(token: &str, option: &str) -> bool {
    if !token.starts_with("--") {
        return false;
    }
    let name = token.split('=').next().unwrap_or(token);
    name.len() > 2 && option.starts_with(name)
}

fn claude_git_output_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().skip(2).any(|token| {
        // `--text` is an exact, read-only diff option (force text on binary).
        // git resolves exact option names before abbreviations, so it must not
        // be denied as a `--textconv` abbreviation.
        if *token == "--text" {
            return false;
        }
        ["--output", "--ext-diff", "--textconv"]
            .iter()
            .any(|option| claude_git_long_option_abbreviates(token, option))
    })
}

fn claude_git_grep_tokens_are_read_only(tokens: &[&str]) -> bool {
    !tokens.iter().skip(2).any(|token| {
        // `-O[<pager>]`/`--open-files-in-pager` opens matches in a pager (an exec
        // sink). git bundles short options, so `-nOcat` parses as
        // `-n --open-files-in-pager=cat`; scan the cluster and deny any `O` that
        // git reads as this option. An earlier value-taking option (`-e`/`-f`,
        // context `-A`/`-B`/`-C`, `-m`) consumes the rest as its value, so an
        // `O` after one of those is a value character, not the sink.
        if token.starts_with('-') && !token.starts_with("--") {
            for flag in token.bytes().skip(1) {
                match flag {
                    b'O' => return true,
                    b'e' | b'f' | b'A' | b'B' | b'C' | b'm' => break,
                    _ => {}
                }
            }
            false
        } else {
            // `--text` (git grep -a: treat binary as text) is an exact read-only
            // option; git resolves it before the `--textconv` abbreviation.
            // `--textconv` runs a config-driven filter program (an exec sink, as
            // for diff/log/show) — denied so grep matches the other read paths.
            *token != "--text"
                && (claude_git_long_option_abbreviates(token, "--open-files-in-pager")
                    || claude_git_long_option_abbreviates(token, "--textconv"))
        }
    })
}

/// Parses Claude tool permission request.
fn parse_claude_tool_permission_request(message: &Value) -> Option<ClaudeToolPermissionRequest> {
    if message.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }

    let request = message.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }

    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = request.get("tool_name").and_then(Value::as_str)?;
    let tool_input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let permission_mode_for_session = request
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .and_then(|suggestions| {
            suggestions.iter().find_map(|suggestion| {
                (suggestion.get("type").and_then(Value::as_str) == Some("setMode")
                    && suggestion.get("destination").and_then(Value::as_str) == Some("session"))
                .then(|| suggestion.get("mode").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
            })
        });

    let detail = describe_claude_permission_detail(
        tool_name,
        &tool_input,
        request.get("decision_reason").and_then(Value::as_str),
    );

    Some(ClaudeToolPermissionRequest {
        detail,
        permission_mode_for_session,
        request_id,
        title: "Claude needs approval".to_owned(),
        tool_name: tool_name.to_owned(),
        tool_input,
    })
}

/// Records Claude assistant text delta.
fn record_claude_assistant_text_delta(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let delta = if state.saw_text_delta {
        text
    } else {
        text.trim_start_matches('\n')
    };
    if delta.is_empty() {
        return Ok(());
    }

    recorder.text_delta(delta)?;
    state.saw_text_delta = true;
    state.streamed_assistant_text.push_str(delta);
    Ok(())
}

/// Records Claude completed assistant text.
fn record_claude_completed_assistant_text(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if !state.saw_text_delta {
        state.streamed_assistant_text.clear();
        state.streamed_assistant_text.push_str(trimmed);
        return recorder.push_text(trimmed);
    }

    match next_completed_codex_text_update(&mut state.streamed_assistant_text, trimmed) {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }
}

/// Finishes Claude assistant text stream.
fn finish_claude_assistant_text_stream<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    recorder.finish_streaming_text()?;
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
    Ok(())
}

/// Clears Claude turn-local state.
fn clear_claude_turn_state(state: &mut ClaudeTurnState) {
    state.approval_keys_this_turn.clear();
    state.parallel_agent_group_key = None;
    state.parallel_agent_order.clear();
    state.parallel_agents.clear();
    state.permission_denied_this_turn = false;
    state.pending_tools.clear();
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
}

/// Resets Claude turn-local parser and recorder state.
fn reset_claude_turn_state<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    finish_claude_assistant_text_stream(state, recorder)?;
    clear_claude_turn_state(state);
    recorder.reset_turn_state()
}

/// Handles Claude event.
fn handle_claude_event(
    message: &Value,
    session_id: &mut Option<String>,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        return Ok(());
    };

    match event_type {
        "system" => {
            if message.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(found_session_id) = message.get("session_id").and_then(Value::as_str) {
                    *session_id = Some(found_session_id.to_owned());
                    recorder.note_external_session(found_session_id)?;
                }
            }
        }
        "stream_event" => {
            let Some(stream_type) = message.pointer("/event/type").and_then(Value::as_str) else {
                return Ok(());
            };

            match stream_type {
                "content_block_delta" => {
                    if !state.permission_denied_this_turn {
                        if let Some(text) = message
                            .pointer("/event/delta/text")
                            .or_else(|| message.pointer("/event/delta/text_delta"))
                            .and_then(Value::as_str)
                        {
                            record_claude_assistant_text_delta(state, recorder, text)?;
                        }
                    }
                }
                "message_stop" => {
                    // Claude can emit the final assistant payload after `message_stop`.
                    // Keep the current text bubble open so any unseen suffix lands in it.
                }
                _ => {}
            }
        }
        "assistant" => {
            if let Some(contents) = message
                .pointer("/message/content")
                .and_then(Value::as_array)
            {
                for content in contents {
                    let Some(content_type) = content.get("type").and_then(Value::as_str) else {
                        continue;
                    };

                    match content_type {
                        "text" => {
                            if let Some(text) = content.get("text").and_then(Value::as_str) {
                                if state.permission_denied_this_turn {
                                    continue;
                                }
                                record_claude_completed_assistant_text(state, recorder, text)?;
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = content.get("thinking").and_then(Value::as_str)
                            {
                                finish_claude_assistant_text_stream(state, recorder)?;
                                let lines = split_thinking_lines(thinking);
                                recorder.push_thinking("Thinking", lines)?;
                            }
                        }
                        "tool_use" => {
                            finish_claude_assistant_text_stream(state, recorder)?;
                            register_claude_tool_use(content, state, recorder)?;
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            handle_claude_tool_result(message, state, recorder)?;
        }
        "result" => {
            reset_claude_turn_state(state, recorder)?;

            if message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                recorder.error(&summarize_error(message))?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Registers Claude tool use.
fn register_claude_tool_use(
    content: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(tool_id) = content.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(name) = content.get("name").and_then(Value::as_str) else {
        return Ok(());
    };

    let input = content.get("input");
    let command = input
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let description = input
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let file_path = input
        .and_then(|value| value.get("file_path").or_else(|| value.get("filePath")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let subagent_type = input
        .and_then(|value| {
            value
                .get("subagent_type")
                .or_else(|| value.get("subagentType"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned);

    state.pending_tools.insert(
        tool_id.to_owned(),
        ClaudeToolUse {
            command: command.clone(),
            description: description.clone(),
            file_path,
            name: name.to_owned(),
            subagent_type: subagent_type.clone(),
        },
    );

    match name {
        "Bash" => {
            let command_label = command
                .as_deref()
                .or(description.as_deref())
                .unwrap_or("Bash");
            recorder.command_started(tool_id, command_label)?;
        }
        "Task" => {
            if state.parallel_agent_group_key.is_none() {
                state.parallel_agent_group_key = Some(format!("claude-task-group-{tool_id}"));
            }
            if !state.parallel_agents.contains_key(tool_id) {
                state.parallel_agent_order.push(tool_id.to_owned());
            }
            state.parallel_agents.insert(
                tool_id.to_owned(),
                ParallelAgentProgress {
                    detail: Some("Initializing...".to_owned()),
                    id: tool_id.to_owned(),
                    source: ParallelAgentSource::Tool,
                    status: ParallelAgentStatus::Initializing,
                    title: describe_claude_task_tool(
                        description.as_deref(),
                        subagent_type.as_deref(),
                    ),
                },
            );
            sync_claude_parallel_agents(state, recorder)?;
        }
        _ => {}
    }

    Ok(())
}
/// Handles Claude tool result.
fn handle_claude_tool_result(
    message: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(contents) = message
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for content in contents {
        if content.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let Some(tool_use_id) = content.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(tool_use) = state.pending_tools.remove(tool_use_id) else {
            continue;
        };

        let is_error = content
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let detail = extract_claude_tool_result_text(message, content);

        match tool_use.name.as_str() {
            "Bash" => handle_claude_bash_result(
                tool_use_id,
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Task" => handle_claude_task_result(
                tool_use_id,
                &tool_use,
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Write" | "Edit" => handle_claude_file_result(
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            _ => {
                if is_error {
                    recorder.error(&detail)?;
                }
            }
        }
    }

    Ok(())
}
/// Handles Claude task result.
fn handle_claude_task_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let title = describe_claude_task_tool(
        tool_use.description.as_deref(),
        tool_use.subagent_type.as_deref(),
    );
    let summarized_detail = summarize_claude_task_detail(detail, is_error);
    let status = if is_error {
        ParallelAgentStatus::Error
    } else {
        ParallelAgentStatus::Completed
    };

    if let Some(agent) = state.parallel_agents.get_mut(tool_use_id) {
        agent.detail = Some(summarized_detail.clone());
        if agent.source != ParallelAgentSource::Tool {
            eprintln!(
                "claude task warning> resetting non-tool parallel agent source for `{tool_use_id}`"
            );
            agent.source = ParallelAgentSource::Tool;
        }
        agent.status = status;
        if agent.title.trim().is_empty() {
            agent.title = title.clone();
        }
    } else {
        state.parallel_agent_order.push(tool_use_id.to_owned());
        state.parallel_agents.insert(
            tool_use_id.to_owned(),
            ParallelAgentProgress {
                detail: Some(summarized_detail.clone()),
                id: tool_use_id.to_owned(),
                source: ParallelAgentSource::Tool,
                status,
                title: title.clone(),
            },
        );
    }

    sync_claude_parallel_agents(state, recorder)?;

    let trimmed = detail.trim();
    let result_summary = if trimmed.is_empty() {
        if is_error {
            Some(summarized_detail.as_str())
        } else {
            None
        }
    } else {
        Some(trimmed)
    };
    if let Some(summary) = result_summary {
        recorder.push_subagent_result(&title, summary, None, None)?;
    }

    Ok(())
}

/// Syncs Claude parallel agents.
fn sync_claude_parallel_agents(
    state: &ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(key) = state.parallel_agent_group_key.as_deref() else {
        return Ok(());
    };

    let agents = state
        .parallel_agent_order
        .iter()
        .filter_map(|agent_id| state.parallel_agents.get(agent_id).cloned())
        .collect::<Vec<_>>();
    if agents.is_empty() {
        return Ok(());
    }

    recorder.upsert_parallel_agents(key, &agents)
}

/// Describes Claude task tool.
fn describe_claude_task_tool(description: Option<&str>, subagent_type: Option<&str>) -> String {
    let trimmed_description = description.unwrap_or("").trim();
    if !trimmed_description.is_empty() {
        return trimmed_description.to_owned();
    }

    let trimmed_subagent_type = subagent_type.unwrap_or("").trim();
    if !trimmed_subagent_type.is_empty() {
        return format!("{} agent", trimmed_subagent_type.replace('-', " "));
    }

    "Task agent".to_owned()
}

/// Summarizes Claude task detail.
fn summarize_claude_task_detail(detail: &str, is_error: bool) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return if is_error {
            "Task failed.".to_owned()
        } else {
            "Completed.".to_owned()
        };
    }

    make_preview(trimmed)
}
/// Handles Claude bash result.
fn handle_claude_bash_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error && is_permission_denial(detail) {
        state.permission_denied_this_turn = true;
        record_claude_approval(
            state,
            recorder,
            "Claude needs approval",
            tool_use.command.as_deref().unwrap_or("Bash"),
            detail,
        )?;
        return Ok(());
    }

    let stdout = tool_use_result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = tool_use_result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let interrupted = tool_use_result
        .and_then(|value| value.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    if output.trim().is_empty() && !detail.is_empty() {
        output.push_str(detail);
    }

    let status = if is_error || interrupted {
        CommandStatus::Error
    } else {
        CommandStatus::Success
    };
    let command = tool_use.command.as_deref().unwrap_or("Bash");
    recorder.command_completed(tool_use_id, command, output.trim_end(), status)
}

/// Handles Claude file result.
fn handle_claude_file_result(
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error {
        if is_permission_denial(detail) {
            state.permission_denied_this_turn = true;
            record_claude_approval(
                state,
                recorder,
                "Claude needs approval",
                &describe_claude_tool_action(tool_use),
                detail,
            )?;
        } else {
            recorder.error(detail)?;
        }
        return Ok(());
    }

    let Some(tool_use_result) = tool_use_result else {
        return Ok(());
    };

    let tool_kind = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .or(tool_use.file_path.as_deref())
    else {
        return Ok(());
    };

    match tool_kind {
        "create" => {
            let content = tool_use_result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let diff = content
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            recorder.push_diff(
                file_path,
                &format!("Created {}", short_file_name(file_path)),
                &diff,
                ChangeType::Create,
            )?;
        }
        "update" => {
            let diff = tool_use_result
                .get("structuredPatch")
                .and_then(Value::as_array)
                .map(|patches| flatten_structured_patch(patches.as_slice()))
                .filter(|diff| !diff.trim().is_empty())
                .unwrap_or_else(|| {
                    fallback_file_diff(
                        tool_use_result
                            .get("originalFile")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        tool_use_result
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                });
            recorder.push_diff(
                file_path,
                &format!("Updated {}", short_file_name(file_path)),
                &diff,
                ChangeType::Edit,
            )?;
        }
        _ => {}
    }

    Ok(())
}

/// Extracts Claude tool result text.
fn extract_claude_tool_result_text(message: &Value, content: &Value) -> String {
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(parts) = content.get("content").and_then(Value::as_array) {
        let combined = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !combined.trim().is_empty() {
            return combined;
        }
    }
    if let Some(text) = message.get("tool_use_result").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = message
        .get("tool_use_result")
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }

    "Claude tool call failed.".to_owned()
}
/// Returns whether permission denial.
fn is_permission_denial(detail: &str) -> bool {
    detail.contains("requested permissions")
}

/// Records Claude approval.
fn record_claude_approval(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    title: &str,
    command: &str,
    detail: &str,
) -> Result<()> {
    let key = format!("{title}\n{command}\n{detail}");
    if state.approval_keys_this_turn.insert(key) {
        recorder.push_approval(title, command, detail)?;
    }

    Ok(())
}

/// Describes Claude tool request.
fn describe_claude_tool_request(request: &ClaudeToolPermissionRequest) -> String {
    describe_claude_tool_action_from_parts(&request.tool_name, &request.tool_input)
}

/// Describes Claude tool action.
fn describe_claude_tool_action(tool_use: &ClaudeToolUse) -> String {
    match (
        tool_use.name.as_str(),
        tool_use.file_path.as_deref(),
        tool_use.command.as_deref(),
    ) {
        ("Write" | "Edit", Some(file_path), _) => format!("{} {}", tool_use.name, file_path),
        (_, _, Some(command)) => command.to_owned(),
        _ => tool_use.name.clone(),
    }
}

/// Describes Claude tool action from parts.
fn describe_claude_tool_action_from_parts(tool_name: &str, tool_input: &Value) -> String {
    match tool_name {
        "Write" | "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("{tool_name} {file_path}"))
            .unwrap_or_else(|| tool_name.to_owned()),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| tool_name.to_owned()),
        _ => tool_name.to_owned(),
    }
}

/// Describes Claude permission detail.
fn describe_claude_permission_detail(
    tool_name: &str,
    tool_input: &Value,
    decision_reason: Option<&str>,
) -> String {
    let specific = match tool_name {
        "Write" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to write to {file_path}.")),
        "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to edit {file_path}.")),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("Claude requested permission to run `{command}`.")),
        _ => None,
    };

    match (
        specific,
        decision_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty()),
    ) {
        (Some(specific), Some(reason)) => format!("{specific} Reason: {reason}."),
        (Some(specific), None) => specific,
        (None, Some(reason)) => format!("Claude requested approval. Reason: {reason}."),
        (None, None) => "Claude requested approval.".to_owned(),
    }
}

fn split_thinking_lines(thinking: &str) -> Vec<String> {
    let lines = thinking
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if lines.is_empty() && !thinking.trim().is_empty() {
        vec![thinking.trim().to_owned()]
    } else {
        lines
    }
}
