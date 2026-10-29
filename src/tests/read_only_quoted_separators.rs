// Owns the tests that the read-only Bash checker keeps a '|' or '&' inside a
// quoted argument (a grep alternation) only for an executor proven to run
// bash, Claude's Bash tool, while every other caller, Kimi's read-only gate
// included, keeps master's split at every separator. On the bash path only a
// pattern reader (grep, rg, git grep, git log --grep) may hold one, and still
// passes its own checks; any other command holding one is refused. A real
// separator still splits, each segment is still checked, and the shapes where
// bash and PowerShell disagree about where a quote ends or whether a separator
// is escaped (`\"`, `\'` outside quotes, typographic quotes, `--%`) stay
// refused. Also owns that a `2>/dev/null` is stripped only as a word of its
// own, so the strip cannot change where a quote or separator lies. Does not
// own which commands a segment may run (claude.rs and
// claude_permission_boundary.rs tests) or the refusal's wording
// (read_only_refusal_rules.rs). New file.
use super::*;

const WORKSPACE: &str = "/work/repo";

/// The strict check every caller not proven to run bash uses.
fn strict_allows(command: &str) -> bool {
    claude_bash_command_is_read_only(command, WORKSPACE)
}

/// The read-only permission decision for a Bash `command` of a read-only
/// Claude child spawned with no shell-changing setting: whether it is
/// allowed. This adapter is the only way to the bash checker mode.
fn claude_allows(command: &str) -> bool {
    claude_tool_allows("Bash", command, ClaudeBashToolShell::ClaudeCodeDefault)
}

/// As `claude_allows`, for a runtime whose Bash shell is not proven.
fn unproven_claude_allows(command: &str) -> bool {
    claude_tool_allows("Bash", command, ClaudeBashToolShell::Unproven)
}

/// The read-only permission decision for `command` sent through Claude's
/// `tool` of a read-only Claude child whose Bash tool runs in `shell`:
/// whether it is allowed.
fn claude_tool_allows(tool: &str, command: &str, shell: ClaudeBashToolShell) -> bool {
    let action = classify_claude_control_request(
        &json!({"type":"control_request", "request_id":"quoted-separators",
            "request":{"subtype":"can_use_tool", "tool_name":tool,
                "input":{"command": command}}}),
        &mut ClaudeTurnState::default(),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        WORKSPACE,
        ClaudeHostAdmission {
            bash_tool_shell: shell,
            ..ClaudeHostAdmission::default()
        },
    )
    .unwrap()
    .expect("the request reaches the host's gate");
    match action {
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. }) => true,
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { .. }) => false,
        _ => panic!("a read-only child's request is answered at once"),
    }
}

/// The read-only gate's decision for a Bash `command` of a read-only Kimi
/// child, from a complete streamed request: whether it is allowed.
fn kimi_allows(command: &str) -> bool {
    let summary = if command.chars().count() > 50 {
        format!(
            "Requesting approval to Running: {}\u{2026}",
            command.chars().take(40).collect::<String>()
        )
    } else {
        format!("Requesting approval to Running: {command}")
    };
    let params = json!({ "sessionId": "session_x", "options": [
            { "optionId": "approve_once", "name": "Approve once", "kind": "allow_once" },
            { "optionId": "reject", "name": "Reject", "kind": "reject_once" }],
        "toolCall": { "toolCallId": "b", "title": "Bash", "content": [
            { "type": "content", "content": { "type": "text", "text": summary } }] } });
    let observation = KimiToolCallObservation {
        title: Some("Bash".to_owned()),
        streamed_args: Some(json!({ "command": command }).to_string()),
        oversized: false,
        approved_args: None,
    };
    matches!(
        kimi_read_only_permission_decision(&params, Some(&observation), WORKSPACE, &|_| true),
        KimiReadOnlyDecision::Allow { .. }
    )
}

/// Refused on every path: strict, and the Claude (bash mode) and Kimi gates.
fn assert_refused_everywhere(command: &str) {
    assert!(!strict_allows(command), "strict: {command}");
    assert!(!claude_allows(command), "claude: {command}");
    assert!(!kimi_allows(command), "kimi: {command}");
}

#[test]
fn a_separator_inside_quotes_stays_inside_its_argument_on_the_bash_path() {
    for command in [
        // The reported case: a grep alternation in one quoted argument.
        r#"git grep -n "fn a\|fn b" HEAD -- src"#,
        "grep -E 'a|b' file",
        r#"rg "x|y" src"#,
        // A quoted alternation, then a real pipe into a reader.
        "grep 'a|b' f | head -5",
        r#"git diff | grep "a|b""#,
        // '&' inside quotes is an argument too.
        r#"grep "a&&b" f"#,
    ] {
        assert!(claude_allows(command), "{command}");
    }
}

#[test]
fn only_the_bash_path_keeps_a_quoted_separator() {
    // The reported command: allowed to Claude's Bash tool, refused, exactly
    // as on master, to Kimi and to every other caller.
    let reported = r#"git grep "a\|b""#;
    assert!(claude_allows(reported));
    assert!(!strict_allows(reported));
    assert!(!kimi_allows(reported));
    for command in [
        "grep 'a|b' f",
        r#"rg "x|y" src"#,
        r#"grep "a&&b" f"#,
        r#"grep "a||b" f"#,
    ] {
        assert!(claude_allows(command), "{command}");
        assert!(!strict_allows(command), "{command}");
        assert!(!kimi_allows(command), "{command}");
    }
}

#[test]
fn a_sed_address_delimited_by_a_quoted_pipe_is_refused_on_every_path() {
    // `\|re|` is a sed address with '|' as its delimiter, so the write or
    // exec command after it is real. sed is no pattern reader, so the bash
    // path refuses its quoted separator; master's split leaves an
    // unterminated quote.
    for command in [
        r"sed -n '\|foo|w victim.txt' input.txt",
        r"sed -n '\|foo|e touch victim.txt' input.txt",
        r#"sed -n "\|foo|w victim.txt" input.txt"#,
        r"grep -n x f | sed -n '\|foo|w victim.txt'",
    ] {
        assert_refused_everywhere(command);
        assert!(!unproven_claude_allows(command), "unproven: {command}");
    }
}

#[test]
fn only_a_pattern_reader_keeps_a_quoted_separator_on_the_bash_path() {
    // Kept: a pattern reader, known by its parsed command word and git's
    // subcommand, each segment on its own.
    for command in [
        r#"git grep "a\|b""#,
        r#"git --no-pager grep -n "a|b" HEAD"#,
        r#"git log --oneline --grep="a|b""#,
        r#"git log -i --grep "a|b" -5 HEAD -- src"#,
        r#"'grep' "a|b" f"#,
        "rg -n 'x|y' src",
        r#"git log --grep="a|b" | grep "c|d""#,
    ] {
        assert!(claude_allows(command), "{command}");
    }
    // Refused: every other command holding a quoted separator, a pattern
    // reader in name only, in a later word or in another segment, git with
    // another subcommand, and `git log` whose separator is not a `--grep`
    // pattern the parse can place.
    for command in [
        "cat 'a|b'",
        "head -n 5 'a|b'",
        "wc -l 'a|b'",
        "ls 'a|b'",
        "find . -name 'a|b'",
        "sed -n '/a|b/p' f",
        r#"egrep "a|b" f"#,
        r#"fgrep "a|b" f"#,
        r#"git show "HEAD:a|b""#,
        r#"git diff "a|b""#,
        r#"git -c x=y grep "a|b""#,
        r#"xgrep "a|b" f"#,
        r#"echo grep "a|b""#,
        r#"grep -n x f | cat "a|b""#,
        r#"cat "a|b" | grep x"#,
        r#"git log --format="%H|%s""#,
        r#"git log --grep=x --format="%H|%s""#,
        r#"git log -- "--grep=a|b""#,
        r#"git log --grep=x -- "a|b""#,
        r#"git log --author --grep "a|b""#,
        r#"git log --grep --grep "a|b""#,
        "for f in a b; do cat 'x|y' $f; done",
    ] {
        assert!(!claude_allows(command), "claude: {command}");
        assert!(!strict_allows(command), "strict: {command}");
    }
    // A lone quoted '&' was never a separator to master, which accepts these;
    // the bash path refuses them outside a pattern reader, so it accepts
    // nothing master refused.
    for command in ["cat 'a & b'", "ls 'a & b'"] {
        assert!(!claude_allows(command), "claude: {command}");
        assert!(strict_allows(command), "strict: {command}");
    }
}

#[test]
fn a_pattern_reader_with_a_quoted_separator_still_passes_every_other_check() {
    // Eligibility only lets the separator stay in its argument: the reader's
    // own write, exec and pager denials still apply.
    for command in [
        "rg --pre=cat 'a|b' src",
        r#"rg --pre cat "a|b" src"#,
        r#"git grep -O "a|b""#,
        r#"git grep --open-files-in-pager "a|b""#,
        r#"git log --grep="a|b" --output=out"#,
        r#"git --paginate log --grep="a|b""#,
        r#"git -c core.pager=x grep "a|b""#,
        r#"grep "a|b" f | tee out"#,
    ] {
        assert_refused_everywhere(command);
    }
}

#[test]
fn no_other_route_acquires_the_bash_mode() {
    // Claude's PowerShell tool is refused whatever its command.
    for command in [r#"git grep "a\|b""#, "git status"] {
        assert!(
            !claude_tool_allows(
                "PowerShell",
                command,
                ClaudeBashToolShell::ClaudeCodeDefault
            ),
            "{command}"
        );
    }
    // A bounded literal loop keeps its caller's mode in every expanded body.
    let quoted_in_loop = "for f in a.rs b.rs; do grep -n 'x|y' $f; done";
    assert!(claude_allows(quoted_in_loop));
    assert!(!strict_allows(quoted_in_loop));
    assert!(!kimi_allows(quoted_in_loop));
    // A writer in the loop body is refused on every path.
    assert_refused_everywhere("for f in a.rs b.rs; do grep -n 'x|y' $f | tee out; done");
}

#[test]
fn a_claude_runtime_whose_bash_shell_is_unproven_stays_strict() {
    for command in [
        r#"git grep "a\|b""#,
        "grep 'a|b' f | head -5",
        r#"rg "x|y" src"#,
    ] {
        assert!(claude_allows(command), "{command}");
        assert!(!unproven_claude_allows(command), "{command}");
    }
    // Master-compatible commands are allowed either way.
    for command in ["git status", "grep -n x f | head -5"] {
        assert!(claude_allows(command), "{command}");
        assert!(unproven_claude_allows(command), "{command}");
    }
}

/// The launch decision for the given explicit settings (`None` removes) and
/// inherited names, all injected: no process environment is read.
fn launch_shell(
    explicit: &[(&str, Option<std::ffi::OsString>)],
    inherited: &[&str],
) -> ClaudeBashToolShell {
    claude_bash_tool_shell_from_env(
        explicit
            .iter()
            .map(|(key, value)| (std::ffi::OsStr::new(*key), value.as_deref())),
        inherited.iter().map(|key| std::ffi::OsString::from(*key)),
    )
}

#[test]
fn a_shell_changing_setting_in_the_spawned_environment_leaves_the_shell_unproven() {
    const KEY: &str = "CLAUDE_CODE_GIT_BASH_PATH";
    let value = |text: &str| Some(std::ffi::OsString::from(text));
    // Neither set nor inherited: Claude Code chooses its own bash.
    assert_eq!(
        launch_shell(&[], &[]),
        ClaudeBashToolShell::ClaudeCodeDefault
    );
    assert_eq!(
        launch_shell(&[("PATH", value("/usr/bin"))], &["HOME"]),
        ClaudeBashToolShell::ClaudeCodeDefault
    );
    // Set by the launch, in any letter case, with any value: unproven. A
    // file name that looks like bash proves nothing.
    for (key, set) in [
        (KEY, value(r"C:\Program Files\Git\bin\bash.exe")),
        (KEY, value("/usr/local/bin/pwsh")),
        (KEY, value("")),
        ("claude_code_git_bash_path", value("/bin/bash")),
    ] {
        assert_eq!(
            launch_shell(&[(key, set)], &[]),
            ClaudeBashToolShell::Unproven,
            "{key}"
        );
    }
    // A value that is not Unicode is still a value.
    #[cfg(windows)]
    let unreadable = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xD800])
    };
    #[cfg(not(windows))]
    let unreadable = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xFF])
    };
    assert!(unreadable.to_str().is_none());
    assert_eq!(
        launch_shell(&[(KEY, Some(unreadable))], &[]),
        ClaudeBashToolShell::Unproven
    );
    // Inherited and left alone: unproven. Removed by the launch under that
    // exact name: Claude Code's own bash. Removed under another spelling:
    // still unproven.
    assert_eq!(launch_shell(&[], &[KEY]), ClaudeBashToolShell::Unproven);
    assert_eq!(
        launch_shell(&[(KEY, None)], &[KEY]),
        ClaudeBashToolShell::ClaudeCodeDefault
    );
    assert_eq!(
        launch_shell(&[("claude_code_git_bash_path", None)], &[KEY]),
        ClaudeBashToolShell::Unproven
    );
    // An explicit setting on the real launch command counts too.
    let mut command = Command::new("claude");
    command.env(KEY, "bash.exe");
    assert_eq!(
        claude_bash_tool_shell(&command),
        ClaudeBashToolShell::Unproven
    );
}

/// The answer a read-only Claude runtime installed on `session_id`, with its
/// reader context marked by `shell` when given, gives one Bash `command`,
/// through the production frame application: whether it is allowed.
fn runtime_allows(
    state: &AppState,
    session_id: &str,
    shell: Option<ClaudeBashToolShell>,
    command: &str,
) -> bool {
    let (runtime, commands) =
        test_claude_runtime_handle(&format!("quoted-separators-{session_id}"));
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let input_tx = runtime.input_tx.clone();
    let cwd = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .expect("the session exists");
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.claude_approval_mode = Some(ClaudeApprovalMode::ReadOnlyAutoApprove);
        record.session.workdir.clone()
    };
    let mut context = ClaudeRuntimeContext::new(
        state.clone(),
        session_id.to_owned(),
        token,
        new_claude_turn_ownership(),
        Arc::new(Mutex::new(None)),
        cwd,
    );
    if let Some(shell) = shell {
        context = context.with_bash_tool_shell(shell);
    }
    let mut frames = ClaudeReaderFrames::new(&context, input_tx, None);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.to_owned());
    let frame = json!({"type": "control_request", "request_id": "quoted-runtime",
        "request": {"subtype": "can_use_tool", "tool_name": "Bash",
            "input": {"command": command}}});
    let outcome = apply_claude_frame(&context, &mut frames, &mut recorder, &frame);
    assert_eq!(outcome.next, ClaudeFrameApplied::Continue, "{command}");
    let answers = commands
        .try_iter()
        .filter_map(|command| match command {
            ClaudeRuntimeCommand::PermissionResponse(decision) => Some(decision),
            _ => None,
        })
        .collect::<Vec<_>>();
    match answers.as_slice() {
        [ClaudePermissionDecision::Allow { .. }] => true,
        [ClaudePermissionDecision::Deny { .. }] => false,
        other => panic!("one answer, not {}", other.len()),
    }
}

#[test]
fn each_runtime_carries_its_own_shell_decision() {
    let state = test_app_state();
    let reported = r#"git grep "a\|b""#;
    let first = test_session_id(&state, Agent::Claude);
    let second = test_session_id(&state, Agent::Claude);
    // A runtime spawned with Claude Code's own bash keeps the quoted pipe.
    assert!(runtime_allows(
        &state,
        &first,
        Some(ClaudeBashToolShell::ClaudeCodeDefault),
        reported
    ));
    // Another runtime, at the same time, does not share that decision.
    assert!(!runtime_allows(&state, &second, None, reported));
    // A replacement or restored runtime on the first session, whose reader
    // was not given the spawn's decision, inherits no stale allowance.
    assert!(!runtime_allows(&state, &first, None, reported));
    // A runtime spawned with the setting stays strict.
    assert!(!runtime_allows(
        &state,
        &first,
        Some(ClaudeBashToolShell::Unproven),
        reported
    ));
    // Master-compatible commands are answered alike by every runtime.
    assert!(runtime_allows(&state, &second, None, "git status"));
}

#[test]
fn mixed_quoted_and_real_separators_into_a_writer_stay_refused() {
    for command in [
        "grep 'a|b' f | rm x",
        r#"grep "a&&b" f || tee out"#,
        "grep 'a|b' f && sh",
        r#"rg "x|y" src | head -5 | xargs rm"#,
        r#"grep "a||b" f | cat | tee out"#,
    ] {
        assert_refused_everywhere(command);
    }
}

#[test]
fn the_strict_check_accepts_nothing_master_refused() {
    // Master splits at every `|`, `||` and `&&` as text: a quoted one leaves
    // an unterminated quote and is refused; a lone quoted '&' is no separator.
    // Commands master accepted stay accepted on the strict path, except the
    // shapes the defensive refusals here newly reject.
    for command in [
        "grep -n 'a & b' docs/bugs.md",
        "grep -n x f | head -5",
        "git status && git log -1",
        "git status 2>/dev/null",
    ] {
        assert!(strict_allows(command), "{command}");
        assert!(kimi_allows(command), "{command}");
    }
    for command in [
        "grep 'a|b' f",
        r#"grep "a&&b" f"#,
        r#"grep "a||b" f"#,
        r#"git grep -n "fn a\|fn b" HEAD -- src"#,
        "cat f | sh",
    ] {
        assert!(!strict_allows(command), "{command}");
        assert!(!kimi_allows(command), "{command}");
    }
}

#[test]
fn a_real_separator_still_splits_and_every_segment_is_checked() {
    for command in [
        // An unquoted pipe into a writer or an unknown program.
        r#"grep "a|b" f | tee out"#,
        "cat f | sh",
        // The quote closes before the pipe.
        "grep 'a'|rm x",
        // An or-list or an and-list into a writer.
        r#"grep "a|b" f || rm x"#,
        r#"grep "a|b" f && rm x"#,
    ] {
        assert_refused_everywhere(command);
    }
}

#[test]
fn an_escaped_separator_is_refused_on_the_bash_path_and_keeps_master_verdict_elsewhere() {
    // A separator escaped outside quotes: a literal to bash, an operator to
    // PowerShell. The bash-path splitter refuses it outright. The strict path
    // keeps master's text split unchanged, so its verdict is master's: the
    // split at `|` leaves `grep a\` with nothing escaped and is refused, while
    // `\&\&` holds no adjacent `&&`, is no background `&` to master's gate, and
    // is accepted as the grep pattern `a&&b`. Whether the strict path should
    // refuse it too is a question about master's check, not about this mode.
    for command in [r"grep a\|b f", r"grep a\&\&b f"] {
        assert!(!claude_allows(command), "claude: {command}");
        assert_eq!(
            kimi_allows(command),
            strict_allows(command),
            "kimi: {command}"
        );
    }
    assert!(!strict_allows(r"grep a\|b f"));
    assert!(strict_allows(r"grep a\&\&b f"));
}

#[test]
fn ambiguous_or_malformed_separators_and_quotes_stay_refused() {
    for command in [
        // `\"` inside double quotes: bash stays in the string, PowerShell
        // ends it, so the quoted '|' may be a real pipe.
        r#"grep "x\"|rm y""#,
        r#"grep "a|b\"" f"#,
        // `\'` outside quotes: a literal quote to bash, a quote opener to
        // PowerShell, which then sees the quoted '|' as a real pipe.
        r"echo \'a' |rm y '\'",
        r"grep \' '|sc x' \'",
        // A typographic quote ends a PowerShell string but not a bash one.
        "grep 'a‘|rm x ‘'",
        r#"grep "a”|sc x“""#,
        // PowerShell's stop-parsing token: after `--%` it ignores quotes
        // until the next '|', so the quoted pipe is a real one.
        "rg --% -e x README.md '| % { Remove-Item -Recurse src } | echo #'",
        // An unterminated quote.
        r#"grep "a|b f"#,
        "grep 'a|b f",
        // An empty segment: leading, trailing or doubled separators.
        r#"grep "a|b" f |"#,
        r#"| grep "a|b" f"#,
        r#"grep "a|b" f | | head"#,
        // A lone or background '&'.
        r#"grep "a|b" f & head"#,
        // The raw-text rejections still apply with a quoted separator.
        r#"grep "a|b" f; rm x"#,
        r#"grep "a|b" f > out"#,
        r#"grep "a|b" $(cat f)"#,
    ] {
        assert_refused_everywhere(command);
    }
}

#[test]
fn a_stderr_discard_is_stripped_only_as_a_word_of_its_own() {
    // Removing the discard as text would turn `\2>/dev/null'` into `\'`: an
    // escaped quote to the splitter, a real one to bash, so the real pipe
    // into a writer would look quoted.
    for command in [
        r"grep \2>/dev/null'a' | rm -rf src '\2>/dev/null'",
        r"echo \2>/dev/null\'|touch marker # '",
        r"echo first \2>/dev/null& touch marker",
        "grep a2>/dev/null f",
        "grep '2>/dev/null' f",
        // Next to a `\` inside double quotes.
        r#"grep "a\2>/dev/null|rm b" f"#,
        r#"grep "\2>/dev/null" f | rm x"#,
        // A line break right after the discard.
        "git status 2>/dev/null\nrm x",
        "git status 2>/dev/null\r\nrm x",
    ] {
        assert_refused_everywhere(command);
    }
    for command in [
        "git status 2>/dev/null",
        "git status 2> /dev/null",
        r#"find .claude/reviewers -name "*.md" 2>/dev/null"#,
        "git status 2>/dev/null|head",
        // A tab is whitespace on either side.
        "git status\t2>/dev/null",
        "git status 2>/dev/null\t| head",
    ] {
        assert!(strict_allows(command), "{command}");
        assert!(claude_allows(command), "{command}");
    }
    // With a quoted separator, only the bash path keeps it.
    let quoted = "grep 'a|b' f 2>/dev/null | head -5";
    assert!(claude_allows(quoted));
    assert!(!strict_allows(quoted));
}
