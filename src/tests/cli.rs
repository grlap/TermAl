use super::*;

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
