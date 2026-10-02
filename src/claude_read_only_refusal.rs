// Owns the text of the refusal a read-only Claude child gets when the
// read-only gate denies a request: the rule the request broke and the form
// that is allowed instead, for Bash commands, the PowerShell tool, file edits,
// the tracker and any other tool. Does not own the decision itself: which
// requests are allowed is decided in claude.rs
// (`read_only_claude_permission_decision`, `claude_bash_command_is_read_only`
// and the git checks), and this file only explains a denial already made.
// New file: the denial text it replaces was one generic sentence inline in
// claude.rs, so this is new logic, not a code move.

/// The refusal a read-only Claude child gets for a request the gate denied:
/// the rule it broke and what it may do instead, so it can adapt in one step
/// rather than guess. It says "delegation", not "reviewer": the gate is shared
/// by reviewers and acceptance evaluators and does not know which it serves.
///
/// This only explains a decision already taken; it never decides. The Bash
/// rule comes from [`claude_bash_refusal_rule`], which runs after
/// [`claude_bash_command_is_read_only`] has denied the command.
fn read_only_claude_denial(
    request: &ClaudeToolPermissionRequest,
    cwd: &str,
    is_tracker_tool: bool,
) -> String {
    const READ_ONLY_TOOLS: &str = "Allowed: the Read, Grep, Glob, LS, ToolSearch and Skill \
         tools, and read-only commands through the Bash tool.";
    let tool = request.tool_name.as_str();
    let (subject, rule, allowed) = if is_tracker_tool {
        (
            "tracker",
            "this delegation has no tracker access; the parent session reads and writes the \
             tracker."
                .to_owned(),
            READ_ONLY_TOOLS.to_owned(),
        )
    } else {
        match tool {
            "Bash" => (
                "Bash",
                request
                    .tool_input
                    .get("command")
                    .and_then(Value::as_str)
                    .map_or_else(
                        || "a Bash request without a command is refused.".to_owned(),
                        |command| claude_bash_refusal_rule(command, cwd),
                    ),
                claude_read_only_bash_allowed_form(cwd),
            ),
            "PowerShell" => (
                "PowerShell",
                "the PowerShell tool is always refused in a read-only delegation.".to_owned(),
                claude_read_only_bash_allowed_form(cwd),
            ),
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => (
                "file edit",
                "a read-only delegation may not write or edit files.".to_owned(),
                READ_ONLY_TOOLS.to_owned(),
            ),
            _ => (
                "tool",
                format!(
                    "`{}` is not one of the tools a read-only delegation may use.",
                    claude_refusal_quote_text(tool)
                ),
                READ_ONLY_TOOLS.to_owned(),
            ),
        }
    };
    format!(
        "TermAl denied this {subject} request because this Claude delegation is read-only. \
         Rule: {rule} {allowed}"
    )
}

/// The command form a read-only child may use, said with every Bash and
/// PowerShell refusal.
fn claude_read_only_bash_allowed_form(cwd: &str) -> String {
    let workspace = if cwd.is_empty() {
        "the workspace".to_owned()
    } else {
        format!("the workspace ({})", claude_refusal_quote_text(cwd))
    };
    format!(
        "Allowed: one read-only command per call through the Bash tool, run in {workspace} \
         as it is, or several read-only commands joined with `&&` or `|`, for example \
         `git diff --cached --stat` or `git diff HEAD -- . | sha256sum`; read file content \
         with the Read, Grep and Glob tools."
    )
}

/// Text from the request quoted back in a refusal, kept to one short line:
/// control characters, line and paragraph separators, and invisible format
/// characters (bidirectional overrides, zero-width marks) become spaces.
fn claude_refusal_quote_text(text: &str) -> String {
    const MAX_CHARS: usize = 200;
    let single_line: String = text
        .chars()
        .map(|character| {
            if character.is_control() || claude_refusal_invisible_format_character(character) {
                ' '
            } else {
                character
            }
        })
        .collect();
    if single_line.chars().count() > MAX_CHARS {
        let cut: String = single_line.chars().take(MAX_CHARS).collect();
        format!("{cut}…")
    } else {
        single_line
    }
}

/// Invisible format characters a refusal must not echo: bidirectional
/// controls, zero-width marks, the line and paragraph separators, and the
/// byte-order mark.
fn claude_refusal_invisible_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Names the rule a denied Bash command broke, for its refusal. It follows the
/// order in which [`claude_bash_command_is_read_only`] checks, including that
/// check's line-wide `cd` guard on a line that runs git, so it names the first
/// rule that check applied. It never decides: it runs only after that check
/// has denied the command, and its fallback names no specific rule.
fn claude_bash_refusal_rule(command: &str, cwd: &str) -> String {
    if claude_bash_command_has_background_separator(command) {
        // `2>&1`, `>&2` and `&>` hold an `&` the background scan sees; the
        // child wrote a redirection, so name that.
        if command.contains(">&") || command.contains("<&") || command.contains("&>") {
            return "redirection is refused, including `2>&1`, `>&2` and `&>`; only \
                    `2>/dev/null` is allowed."
                .to_owned();
        }
        return "a background `&` is refused.".to_owned();
    }
    if command.trim_start().starts_with("for ") {
        return "the only loop allowed is `for NAME in literal values; do read-only commands; \
                done`, and every command in its body must pass on its own; run a body command \
                alone to see the rule it breaks."
            .to_owned();
    }
    let normalized = command
        .replace("2> /dev/null", "")
        .replace("2>/dev/null", "");
    if normalized.contains(['\n', '\r', ';']) {
        return "`;` and line breaks are refused anywhere on the line, even inside quotes, so \
                commands cannot be chained that way."
            .to_owned();
    }
    if normalized.contains(['>', '<']) {
        return "redirection is refused: `>` and `<` may not appear anywhere on the line, even \
                inside quotes, except in `2>/dev/null`."
            .to_owned();
    }
    if normalized.contains('`') || normalized.contains("$(") {
        return "command substitution is refused: `$(` and backticks may not appear anywhere on \
                the line, even inside quotes."
            .to_owned();
    }

    let pipe_normalized = normalized.replace("&&", "|").replace("||", "|");
    let segments: Vec<&str> = pipe_normalized.split('|').map(str::trim).collect();
    let runs_git = segments
        .iter()
        .any(|&segment| claude_segment_invokes_git(segment));
    // The check vets every `cd` of a git line before it reads any command, so
    // a `cd` elsewhere is the rule it applied even when an earlier command on
    // the line is refused too.
    if runs_git {
        for &segment in &segments {
            let Some(tokens) = claude_bash_segment_tokens(segment) else {
                continue;
            };
            if tokens.first().is_some_and(|token| token == "cd")
                && !claude_cd_segment_targets_cwd(segment, cwd)
            {
                return if tokens.len() == 2 {
                    "a `cd` away from the workspace is refused on a line that runs git."
                } else {
                    "a bare `cd`, or a `cd` with more than one argument, is refused on a line \
                     that runs git."
                }
                .to_owned();
            }
        }
    }
    for &segment in &segments {
        if segment.is_empty() {
            return "an empty command between `&&`, `||` or `|` is refused.".to_owned();
        }
        if segment == "true" || segment == ":" {
            continue;
        }
        let Some(tokens) = claude_bash_segment_tokens(segment) else {
            return "this shell syntax is refused: variables, globs, subshells, braces and \
                    unbalanced quotes cannot be checked."
                .to_owned();
        };
        let tokens = tokens.iter().map(String::as_str).collect::<Vec<_>>();
        match tokens.as_slice() {
            // A `cd` to one place is inert here; on a git line the guard above
            // has already vetted it.
            ["pwd"] | ["cd", _] => {}
            ["cd", ..] => {
                return "a bare `cd`, or a `cd` with more than one argument, is refused."
                    .to_owned();
            }
            ["git", ..] => {
                if let Some(rule) = claude_git_refusal_rule(&tokens) {
                    return rule;
                }
            }
            [head, ..] => {
                if !claude_bash_tokens_are_read_only(&tokens) {
                    let head = claude_refusal_quote_text(head);
                    return if CLAUDE_READ_ONLY_BASH_COMMANDS.contains(&head.as_str())
                        || CLAUDE_OPTION_CHECKED_BASH_COMMANDS.contains(&head.as_str())
                    {
                        format!("`{head}` is allowed, but one of its arguments is refused.")
                    } else {
                        format!(
                            "`{head}` is not one of the read-only commands this delegation \
                             may run."
                        )
                    };
                }
            }
            [] => return "an empty command is refused.".to_owned(),
        }
    }
    "the command line is not one the read-only gate can verify.".to_owned()
}

/// Names the rule a denied git command broke, or `None` when the git command
/// itself is read-only (the denial then came from elsewhere on the line).
/// Mirrors the leading-option walk of [`claude_git_tokens_are_read_only`].
fn claude_git_refusal_rule(tokens: &[&str]) -> Option<String> {
    if claude_git_tokens_are_read_only(tokens) {
        return None;
    }
    let mut index = 1;
    while let Some(option) = tokens.get(index).copied() {
        match option {
            "--no-pager" | "-P" | "--no-optional-locks" => index += 1,
            _ if !option.starts_with('-') => break,
            _ => {
                let name = option.split('=').next().unwrap_or(option);
                if name == "-h" || claude_git_long_option_abbreviates(name, "--help") {
                    return Some("git help is refused: it starts a viewer.".to_owned());
                }
                let reason = match name {
                    "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                        "it points git at another repository"
                    }
                    "-c" | "--config-env" => "it sets git configuration on the command line",
                    "--exec-path" => "it points git at another git program",
                    "-p" | "--paginate" => "it starts a pager",
                    _ => {
                        "only `--no-pager`, `-P` and `--no-optional-locks` may come before the \
                          subcommand"
                    }
                };
                return Some(format!(
                    "git's `{}` option is refused: {reason}. Run git in the workspace without it.",
                    claude_refusal_quote_text(name)
                ));
            }
        }
    }
    let Some(subcommand) = tokens.get(index).copied() else {
        return Some("a git command without a subcommand is refused.".to_owned());
    };
    let subcommand = claude_refusal_quote_text(subcommand);
    if tokens
        .iter()
        .skip(index + 1)
        .any(|token| *token == "-h" || claude_git_long_option_abbreviates(token, "--help"))
    {
        return Some("git help is refused: it starts a viewer.".to_owned());
    }
    Some(if subcommand == "remote" {
        "`git remote` is allowed only as a listing (`git remote` or `git remote -v`); \
             adding, removing or changing a remote is refused."
            .to_owned()
    } else if subcommand == "branch" {
        "`git branch` is allowed only as a listing, with options such as `-a`, `-r`, `-v`, \
             `--list` or `--show-current`; a branch name, or any option that changes a branch, \
             is refused."
            .to_owned()
    } else if CLAUDE_READ_ONLY_GIT_SUBCOMMANDS.contains(&subcommand.as_str()) {
        format!(
            "`git {subcommand}` is allowed, but one of its options is refused (an output \
                 file, an external diff or text conversion, a pager, or a value it cannot check)."
        )
    } else {
        format!("`git {subcommand}` is not one of the read-only commands this delegation may run.")
    })
}

/// The git subcommands [`claude_git_tokens_are_read_only`] can allow, named in
/// a refusal so that a refused option is not mistaken for a refused command.
const CLAUDE_READ_ONLY_GIT_SUBCOMMANDS: &[&str] = &[
    "diff",
    "log",
    "show",
    "blame",
    "describe",
    "ls-files",
    "rev-parse",
    "shortlog",
    "status",
    "patch-id",
    "grep",
    "remote",
    "branch",
];
