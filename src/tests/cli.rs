use super::*;

#[test]
fn cli_help_dispatches_without_a_host_or_provider() {
    for spelling in ["--help", "-h", "help"] {
        for prefix in [
            vec![],
            vec!["server"],
            vec!["repl"],
            vec!["cli"],
            vec!["repl", "codex"],
            vec!["repl", "--agent", "codex"],
            vec!["cli", "--agent", "codex"],
            vec!["--agent", "codex"],
            vec!["--agent", "claude"],
            vec!["codex"],
            vec!["claude"],
            vec!["cursor"],
            vec!["cursor-agent"],
            vec!["gemini"],
            vec!["gemini-cli"],
            vec!["opencode"],
            vec!["open-code"],
            vec!["kimi"],
            vec!["kimi-code"],
            vec!["telegram"],
            vec!["telegram-bot"],
            vec!["delegation-mcp"],
            vec!["delegation-mcp", "--parent-session-id", "session-x"],
            vec!["delegation-mcp", "--parent-session-id", "help"],
            vec!["review-freeze-check"],
            vec!["review-identity"],
            vec!["sessions"],
            vec!["sessions", "list"],
            vec!["mailbox"],
            vec!["mailbox", "send"],
        ] {
            let mut args: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
            args.push(spelling.to_owned());
            let Mode::CoordinationCli(invocation) = Mode::parse(args).unwrap() else {
                panic!("{prefix:?} {spelling} must select the local help path");
            };
            assert_eq!(invocation.command, CoordinationCliCommand::Help);
            // An unusable URL must not matter: help never constructs a bridge.
            let output = execute_coordination_cli(&invocation.command, "not a URL").unwrap();
            validate_coordination_cli_output(&invocation.command, &output).unwrap();
            let mut bytes = Vec::new();
            render_coordination_cli_output(&invocation.command, &output, &mut bytes).unwrap();
            let usage = String::from_utf8(bytes).unwrap();
            for expected in [
                "termal [server]",
                "termal repl",
                "termal delegation-mcp",
                "termal review-freeze-check",
                "termal review-identity",
                "termal sessions list",
                "termal mailbox send",
                "--receipt <receipt>",
                "sessions list lists root sessions",
                "POST /api/sessions",
            ] {
                assert!(usage.contains(expected), "missing {expected}");
            }
        }
    }
}

#[test]
fn cli_help_preserves_complete_review_command_positionals() {
    for value in ["help", "-h", "--help"] {
        let freeze: Vec<String> = ["review-freeze-check", value, "manifest.json", "expected"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let Mode::ReviewFreeze(actual) = Mode::parse(freeze.clone()).unwrap() else {
            panic!("a full verification command must not become help");
        };
        assert_eq!(actual, freeze);
        let identity: Vec<String> = ["review-identity", value, "{\"operation\":\"check\"}"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let Mode::ReviewIdentity(actual) = Mode::parse(identity.clone()).unwrap() else {
            panic!("a full identity command must not become help");
        };
        assert_eq!(actual, identity);
    }
    // Non-root positionals must stay data too.
    let Mode::ReviewFreeze(actual) = Mode::parse(
        ["review-freeze-check", "root", "help", "expected"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
    .unwrap() else {
        panic!("manifest named help must not become help dispatch");
    };
    assert_eq!(actual[2], "help");
}

#[test]
fn cli_help_preserves_consumed_bridge_values() {
    let Mode::DelegationMcp {
        parent_session_id,
        base_url,
    } = Mode::parse(
        [
            "delegation-mcp",
            "--parent-session-id",
            "help",
            "--base-url",
            "help",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    )
    .unwrap()
    else {
        panic!("bare help consumed as a flag value must remain data");
    };
    assert_eq!(parent_session_id, "help");
    assert_eq!(base_url.as_deref(), Some("help"));
    for spelling in ["-h", "--help"] {
        for prefix in [
            vec!["repl", "--agent"],
            vec!["delegation-mcp", "--parent-session-id"],
        ] {
            let args = prefix
                .into_iter()
                .chain([spelling])
                .map(str::to_owned)
                .collect();
            assert!(matches!(
                Mode::parse(args).unwrap(),
                Mode::CoordinationCli(CoordinationCliInvocation {
                    command: CoordinationCliCommand::Help,
                    ..
                })
            ));
        }
    }
}

#[test]
fn cli_rejects_claude_repl_before_prompt_loop() {
    for args in [
        vec!["claude"],
        vec!["repl", "claude"],
        vec!["cli", "claude"],
        vec!["repl", "--agent", "claude"],
    ] {
        match Mode::parse(args.into_iter().map(str::to_owned).collect()) {
            Ok(_) => panic!("Claude REPL should be rejected before prompt loop"),
            Err(err) => assert!(
                err.to_string()
                    .contains("Claude REPL mode is not supported"),
                "unexpected error: {err:#}"
            ),
        }
    }
}

#[test]
fn cli_still_accepts_codex_repl_shortcuts() {
    for args in [vec!["codex"], vec!["repl", "codex"], vec!["cli", "codex"]] {
        let mode = Mode::parse(args.into_iter().map(str::to_owned).collect())
            .expect("Codex REPL should remain supported");
        assert!(matches!(
            mode,
            Mode::Repl {
                agent: Agent::Codex
            }
        ));
    }
}

#[test]
fn cli_rejects_telegram_relay_aliases_with_settings_guidance() {
    for args in [vec!["telegram"], vec!["telegram-bot"]] {
        match Mode::parse(args.into_iter().map(str::to_owned).collect()) {
            Ok(_) => panic!("Telegram relay aliases should not select a process mode"),
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("Telegram relay runs inside server mode"),
                    "unexpected error: {err:#}"
                );
                assert!(
                    message.contains("Settings -> Telegram"),
                    "unexpected error: {err:#}"
                );
            }
        }
    }
}
