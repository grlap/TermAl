// Bounded Git object/ancestry queries and the unquoted revision-peel exception.
// Owns argument roles and the read-only Git environment supplied at provider
// execution boundaries. General permission dispatch remains in claude.rs.

fn claude_git_cat_file_is_inspection(args: &[&str]) -> bool {
    match args {
        ["-t" | "-s" | "-e" | "-p", object] => !object.is_empty() && !object.starts_with('-'),
        ["-t" | "-s" | "-e" | "-p", "--", object] => !object.is_empty(),
        _ => false,
    }
}

fn claude_git_merge_base_is_query(args: &[&str]) -> bool {
    let mut mode = "normal";
    let mut all = false;
    let mut operands = 0;
    let mut end_options = false;
    for arg in args {
        match *arg {
            "--" if !end_options => end_options = true,
            "-a" | "--all" if !end_options => all = true,
            "--is-ancestor" | "--fork-point" | "--octopus" | "--independent"
                if !end_options && mode == "normal" =>
            {
                mode = arg;
            }
            _ if arg.is_empty() || (!end_options && arg.starts_with('-')) => return false,
            _ => operands += 1,
        }
    }
    match mode {
        "normal" | "--octopus" => operands >= 2,
        "--independent" => !all && operands >= 1,
        "--is-ancestor" => !all && operands == 2,
        "--fork-point" => !all && (1..=2).contains(&operands),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ClaudeGitRefValue {
    Revision,
    Format,
    Sort,
    Literal,
}

// Ref-filter can invoke a configured signature verifier while evaluating an
// atom, including a sort key. Admit known data fields rather than every atom
// supported by this or a future Git version.
fn claude_git_ref_atom_is_data(atom: &str) -> bool {
    let base = atom
        .strip_prefix('*')
        .unwrap_or(atom)
        .split(':')
        .next()
        .unwrap_or("");
    matches!(
        base,
        "refname"
            | "objectname"
            | "objecttype"
            | "objectsize"
            | "symref"
            | "HEAD"
            | "upstream"
            | "push"
            | "authorname"
            | "authoremail"
            | "authordate"
            | "committername"
            | "committeremail"
            | "committerdate"
            | "taggername"
            | "taggeremail"
            | "taggerdate"
            | "creator"
            | "creatordate"
            | "subject"
            | "body"
    )
}

fn claude_git_ref_format_is_data(mut format: &str) -> bool {
    while let Some(percent) = format.find('%') {
        format = &format[percent + 1..];
        if let Some(rest) = format.strip_prefix('%') {
            format = rest;
        } else if let Some(rest) = format.strip_prefix('(') {
            let Some(end) = rest.find(')') else {
                return false;
            };
            if !claude_git_ref_atom_is_data(&rest[..end]) {
                return false;
            }
            format = &rest[end + 1..];
        } else {
            // Git's other format escape is a literal byte, e.g. %09.
            if format.len() < 2 || !format.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            format = &format[2..];
        }
    }
    true
}

fn claude_git_ref_value_is_data(role: ClaudeGitRefValue, value: &str) -> bool {
    match role {
        ClaudeGitRefValue::Format => {
            !value.starts_with('-') && claude_git_ref_format_is_data(value)
        }
        ClaudeGitRefValue::Sort => {
            claude_git_ref_atom_is_data(value.strip_prefix('-').unwrap_or(value))
        }
        ClaudeGitRefValue::Revision => !value.is_empty() && !value.starts_with('-'),
        // Some Git options have optional values. Never consume a following
        // option as harmless data when Git might instead parse that option.
        ClaudeGitRefValue::Literal => !value.starts_with('-'),
    }
}

// The final query and the tokenizer's partial argument use the same exact
// option grammar. Unknown abbreviations fail closed rather than changing which
// token Git consumes as a format, sort key, pattern, or revision.
fn claude_git_ref_query_state(args: &[&str]) -> Option<(Option<ClaudeGitRefValue>, bool)> {
    use ClaudeGitRefValue::*;
    let mut pending = None;
    let mut end_options = false;
    for arg in args {
        if let Some(role) = pending.take() {
            if !claude_git_ref_value_is_data(role, arg) {
                return None;
            }
            continue;
        }
        if end_options {
            continue;
        }
        if matches!(*arg, "--" | "--end-of-options") {
            end_options = true;
            continue;
        }
        if !arg.starts_with('-') {
            continue;
        }
        let (option, value) = arg
            .split_once('=')
            .map_or((*arg, None), |(k, v)| (k, Some(v)));
        let role = match option {
            "--format" => Format,
            "--sort" => Sort,
            "--points-at" | "--contains" | "--no-contains" | "--merged" | "--no-merged" => Revision,
            "--count" | "--color" | "--exclude" | "--start-after" => Literal,
            "--shell"
            | "--perl"
            | "--python"
            | "--tcl"
            | "--ignore-case"
            | "--omit-empty"
            | "--include-root-refs"
            | "--stdin"
            | "--no-color"
                if value.is_none() =>
            {
                continue;
            }
            _ => return None,
        };
        if let Some(value) = value {
            if !claude_git_ref_value_is_data(role, value) {
                return None;
            }
        } else {
            pending = Some(role);
        }
    }
    Some((pending, end_options))
}

fn claude_git_for_each_ref_is_query(args: &[&str]) -> bool {
    matches!(claude_git_ref_query_state(args), Some((None, _)))
}

// Named pretty.<name> aliases are configuration, not literal format data.
// Exact built-ins cannot be replaced by that configuration. Inline formats
// use only known data placeholders; Git's optional magic prefix applies to
// the following placeholder and must not hide signature verification.
fn claude_git_pretty_selector_is_data(selector: &str) -> bool {
    if matches!(
        selector,
        "oneline"
            | "short"
            | "medium"
            | "full"
            | "fuller"
            | "reference"
            | "email"
            | "mboxrd"
            | "raw"
    ) {
        return true;
    }
    let format = if let Some(format) = selector
        .strip_prefix("format:")
        .or_else(|| selector.strip_prefix("tformat:"))
    {
        format
    } else if selector.is_empty() || selector.contains('%') {
        selector
    } else {
        return false;
    };
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        let Some(mut placeholder) = chars.next() else {
            return false;
        };
        if matches!(placeholder, '+' | '-' | ' ') {
            let Some(next) = chars.next() else {
                return false;
            };
            placeholder = next;
            // A failed magic escape can leave this '%' for Git's outer
            // scanner. Do not consume it as if it were a literal escape.
            if placeholder == '%' {
                return false;
            }
        }
        match placeholder {
            '%' | 'H' | 'h' | 'T' | 't' | 'P' | 'p' | 's' | 'f' | 'b' | 'B' | 'n' | 'm' | 'e' => {}
            'a' | 'c' => {
                if !matches!(
                    chars.next(),
                    Some(
                        'n' | 'N'
                            | 'e'
                            | 'E'
                            | 'l'
                            | 'L'
                            | 'd'
                            | 'D'
                            | 'r'
                            | 't'
                            | 'i'
                            | 'I'
                            | 's'
                            | 'h'
                    )
                ) {
                    return false;
                }
            }
            'x' => {
                for _ in 0..2 {
                    if !chars.next().is_some_and(|ch| ch.is_ascii_hexdigit()) {
                        return false;
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn claude_git_rev_list_formats_are_data(args: &[&str]) -> bool {
    args.iter().all(|arg| {
        if let Some(selector) = arg
            .strip_prefix("--pretty=")
            .or_else(|| arg.strip_prefix("--format="))
        {
            return claude_git_pretty_selector_is_data(selector);
        }
        // Refuse bare selectors and abbreviations rather than let an option
        // or the user's configuration supply a format we did not inspect.
        !["--pretty", "--format"]
            .iter()
            .any(|option| claude_git_long_option_abbreviates(arg, option))
    })
}

/// Called only at an unquoted `{`. A Git peel contains no shell expansion:
/// its exact type is checked by the tokenizer. The prefix must additionally
/// be in a known revision argument, never a command, format, pattern or path.
fn claude_git_peel_revision_position(tokens: &[String], prefix: &str) -> bool {
    if tokens.first().is_none_or(|token| token != "git") {
        return false;
    }
    let words: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let Some(index) = claude_git_subcommand_index(&words) else {
        return false;
    };
    let Some(subcommand) = words.get(index).copied() else {
        return false;
    };
    let args = &words[index + 1..];
    let Some(revision) = prefix.strip_suffix('^').filter(|value| !value.is_empty()) else {
        return false;
    };
    // In REV:path everything after ':' is a literal entry path. Only a peel
    // before that separator belongs to the revision grammar.
    if revision.contains(':') {
        return false;
    }
    if subcommand == "for-each-ref" {
        let Some((pending, false)) = claude_git_ref_query_state(args) else {
            return false;
        };
        if let Some(role) = pending {
            return role == ClaudeGitRefValue::Revision && !revision.starts_with('-');
        }
        let Some((option, value)) = revision.split_once('=') else {
            return false;
        };
        return matches!(
            option,
            "--points-at" | "--contains" | "--no-contains" | "--merged" | "--no-merged"
        ) && !value.is_empty()
            && !value.starts_with('-');
    }
    if subcommand == "branch" {
        let mut revision_value = None;
        for arg in args {
            if revision_value.take().is_some() {
                continue;
            }
            match *arg {
                "--" => return false,
                "--format" | "--sort" | "--count" => revision_value = Some(false),
                "--points-at" | "--contains" | "--no-contains" | "--merged" | "--no-merged" => {
                    revision_value = Some(true)
                }
                _ => {}
            }
        }
        if let Some(is_revision) = revision_value {
            return is_revision && !revision.starts_with('-');
        }
        return [
            "--points-at=",
            "--contains=",
            "--no-contains=",
            "--merged=",
            "--no-merged=",
        ]
        .iter()
        .any(|option| {
            revision
                .strip_prefix(*option)
                .is_some_and(|value| !value.is_empty())
        });
    }
    if revision.starts_with('-') {
        return false;
    }
    match subcommand {
        "cat-file" => matches!(
            args,
            ["-t" | "-s" | "-e" | "-p"] | ["-t" | "-s" | "-e" | "-p", "--"]
        ),
        "merge-base" => args.iter().all(|arg| {
            !arg.starts_with('-')
                || matches!(
                    *arg,
                    "--" | "-a"
                        | "--all"
                        | "--is-ancestor"
                        | "--fork-point"
                        | "--octopus"
                        | "--independent"
                )
        }),
        "rev-parse" | "show" | "diff" | "log" | "blame" | "describe" | "rev-list" | "ls-tree" => {
            claude_git_peel_follows_revisions(subcommand, args)
        }
        _ => false,
    }
}

fn claude_git_peel_follows_revisions(subcommand: &str, args: &[&str]) -> bool {
    let mut value_pending = false;
    let mut operands = 0;
    for arg in args {
        if value_pending {
            value_pending = false;
            continue;
        }
        if *arg == "--" {
            return false;
        }
        if !arg.starts_with('-') {
            operands += 1;
            continue;
        }
        if arg.contains('=') || (arg.starts_with("-n") && arg.len() > 2) {
            continue;
        }
        if matches!(
            *arg,
            "--format"
                | "--date"
                | "--encoding"
                | "--since"
                | "--until"
                | "--author"
                | "--committer"
                | "--grep"
                | "--max-count"
                | "-n"
                | "--prefix"
                | "--default"
                | "-G"
                | "-S"
        ) {
            value_pending = true;
            continue;
        }
        if !matches!(
            *arg,
            "--verify"
                | "--quiet"
                | "-q"
                | "--end-of-options"
                | "--revs-only"
                | "--symbolic"
                | "--symbolic-full-name"
                | "--abbrev-ref"
                | "--short"
                | "--name-only"
                | "--name-status"
                | "--raw"
                | "--stat"
                | "--numstat"
                | "--summary"
                | "--binary"
                | "--no-ext-diff"
                | "--no-textconv"
                | "--no-color"
                | "--no-renames"
                | "--no-patch"
                | "-s"
                | "-p"
                | "-r"
                | "-d"
                | "-t"
                | "-z"
                | "-l"
                | "--long"
                | "--object-only"
                | "--full-name"
                | "--full-tree"
                | "--first-parent"
                | "--all"
                | "--reverse"
                | "--topo-order"
                | "--date-order"
                | "--oneline"
                | "--no-walk"
                | "--pretty"
                | "--decorate"
                | "--no-decorate"
        ) {
            return false;
        }
    }
    !value_pending && (!matches!(subcommand, "ls-tree" | "blame") || operands == 0)
}
/// Read-only object inspection must not fetch missing promisor objects through
/// a repository-configured transport. Apply after project environment values.
fn read_only_git_environment(read_only_child: bool) -> BTreeMap<String, String> {
    if read_only_child {
        BTreeMap::from([
            ("GIT_NO_LAZY_FETCH".to_owned(), "1".to_owned()),
            // Independent transport denial also covers older Git versions.
            ("GIT_ALLOW_PROTOCOL".to_owned(), String::new()),
        ])
    } else {
        BTreeMap::new()
    }
}

fn apply_read_only_git_environment(read_only_child: bool, command: &mut Command) {
    for (name, value) in read_only_git_environment(read_only_child) {
        command.env(name, value);
    }
}

fn merge_read_only_git_environment(read_only_child: bool, config: &mut Value) -> Result<()> {
    let env = read_only_git_environment(read_only_child);
    if env.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = env.keys().map(String::as_str).collect();
    merge_owned_agent_shell_env_into_codex_config(config, &env, &names, &[], "Read-only Git")
}
