// Literal regex alternation versus shell composition in credit recognition.
// Uses the production recognizer/outcome and recorder with matched synthetic
// launcher artifacts; it executes no quoted command or live host operation.
use super::*;

#[path = "engram_quoted_pipe_context.rs"]
mod context;

#[path = "engram_quoted_pipe_windows_shims.rs"]
mod windows_shims;

const PREFIX: &str = "node scripts/test-launcher.mjs focused -- node --test";
const ARGS: [&str; 3] = ["--test", "--test-name-pattern=a|b", "fixture.test.mjs"];

fn assert_alternation_outcome(label: &str, command: &str, one_call: bool) {
    let plain = engram_plain_check_command_in(command, EngramShellDialect::Bash)
        .expect("the known Bash focused Node test is recognized");
    assert_eq!(plain.kind, EngramVerificationKind::Test);
    assert!(plain.argv
        .contains(&"--test-name-pattern=a|b".to_owned()));
    for native in [0, 1] {
        let result_line = if native == 0 { "focused: passed" } else { "focused: failed" };
        assert_eq!(
            engram_check_command_outcome(&plain, EngramCommandExit::Code(native),
                &[result_line.to_owned()]),
            Some(if native == 0 { EngramExecutionOutcome::Succeeded }
                else { EngramExecutionOutcome::Failed }),
            "a literal alternation cannot hide the test's native exit: {command}",
        );
        assert!(plain.simple, "{command}");
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("quoted-pipe-{label}-{native}-{one_call}"), native, one_call,
            command, &ARGS,
        );
        fixture.assert_started();
        let checkpoint = fixture.finish(EngramCommandExit::Code(native));
        one_node_test(&checkpoint, if native == 0 { "succeeded" } else { "failed" });
        assert!(checkpoint["verification_evidence"][0]["refs"].as_array().unwrap()
            .contains(&json!(format!("exit:{native}"))), "{checkpoint:#}");
        eprintln!("literal alternation passed: label={label}, native={native}, one_call={one_call}");
    }
}

#[test]
fn single_quoted_node_alternation_keeps_native_outcome() {
    let command = format!("{PREFIX} --test-name-pattern='a|b' fixture.test.mjs");
    assert_alternation_outcome("single-bash", &command, false);
}

#[test]
fn double_quoted_node_alternation_keeps_native_outcome() {
    let command = format!("{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs");
    for one_call in [false, true] {
        assert_alternation_outcome("double", &command, one_call);
    }
}

#[test]
fn mixed_quoted_node_alternation_is_one_argument() {
    let command = format!("{PREFIX} --test-name-pattern=a'|'\"b\" fixture.test.mjs");
    assert_alternation_outcome("mixed-bash", &command, false);
}

#[test]
fn wrapped_literal_alternation_keeps_the_dialect_but_stays_ineligible() {
    for (label, command, dialect) in [
        ("bash", format!("bash -lc '{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs'"),
            EngramShellDialect::Bash),
        ("powershell", format!("pwsh -Command {PREFIX} --test-name-pattern='a|b' fixture.test.mjs"),
            EngramShellDialect::PowerShell),
        ("cmd", format!("cmd /c {PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs"),
            EngramShellDialect::Cmd),
    ] {
        assert_eq!(engram_plain_check_command(&command).unwrap().dialect, dialect);
        assert_not_eligible(&command, true);
        eprintln!("wrapper observation refused: {label}");
    }
}

fn assert_not_eligible(command: &str, expected_recognized: bool) {
    for runtime in [EngramShellDialect::Unknown, EngramShellDialect::Bash] {
        for recognized in [engram_plain_check_command_in(command, runtime),
            engram_check_command_in(command, runtime)] {
            assert_eq!(recognized.is_some(), expected_recognized,
                "the refusal must be observed at the intended boundary ({runtime:?}): {command}");
            if let Some(check) = recognized {
                assert!(!check.simple, "{runtime:?}: {command}");
                for native in [0, 1] {
                    assert_eq!(engram_check_command_outcome(&check, EngramCommandExit::Code(native),
                        &["focused: passed".to_owned(), "focused: failed".to_owned()]),
                        Some(EngramExecutionOutcome::Unknown), "{runtime:?}: {command}");
                }
            }
        }
    }
}

#[test]
fn unquoted_and_mixed_pipeline_operators_stay_ineligible() {
    for arguments in [
        "--test-name-pattern=a|b fixture.test.mjs",
        "--test-name-pattern='a|b' fixture.test.mjs | cat",
        "--test-name-pattern=\"a|b\" fixture.test.mjs || true",
        "--test-name-pattern=a'|'\"b\" fixture.test.mjs | cat",
        "--test-name-pattern='a|'|b fixture.test.mjs",
        "--test-name-pattern=\"a|b\" fixture.test.mjs && echo done",
        "--test-name-pattern='a|b' fixture.test.mjs > output.txt",
        "--test-name-pattern=a\\|b fixture.test.mjs",
        "--test-name-pattern='a|b' fixture.test.mjs; echo done",
        "--test-name-pattern='a|b' fixture.test.mjs \\",
        "--test-name-pattern='a$|b$' fixture.test.mjs",
        "--test-name-pattern='a|b' 'semi;colon' fixture.test.mjs",
        "--test-name-pattern='a|b' 'a&b' fixture.test.mjs",
        "--test-name-pattern='a|b' 'a<b' fixture.test.mjs",
        "--test-name-pattern='a|b' 'a>b' fixture.test.mjs",
    ] {
        assert_not_eligible(&format!("{PREFIX} {arguments}"), true);
    }
    // cmd does not use single quotes to protect its pipeline operator.
    assert_not_eligible(&format!("cmd /c {PREFIX} --test-name-pattern='a|b' fixture.test.mjs"), true);
    assert_not_eligible(&format!("bash -lc '{PREFIX} --test-name-pattern=a\\|b fixture.test.mjs'"), true);
}

#[test]
fn malformed_quotes_and_expansions_remain_fail_closed() {
    for arguments in [
        "--test-name-pattern='a|b fixture.test.mjs",
        "--test-name-pattern=\"a|b fixture.test.mjs",
        "--test-name-pattern='a|b' \"fixture.test.mjs",
        "--test-name-pattern='a|b' fixture.test.mjs\nnode other.mjs",
    ] {
        assert_not_eligible(&format!("{PREFIX} {arguments}"), false);
    }
    for arguments in [
        "--test-name-pattern=\"$(echo a)|b\" fixture.test.mjs",
        "--test-name-pattern=\"`echo a`|b\" fixture.test.mjs",
        "--test-name-pattern='a|b' fixture.test.mjs; echo done",
    ] {
        assert_not_eligible(&format!("{PREFIX} {arguments}"), true);
    }
    // Backslash-quote protection differs across unwrapped/Windows shells.
    // These spellings cannot establish that their pipe is literal.
    assert_not_eligible(&format!(r#"{PREFIX} --test-name-pattern=\'a|b' fixture.test.mjs"#), true);
    for wrapper in ["pwsh -Command", "cmd /c"] {
        assert_not_eligible(&format!(r#"{wrapper} {PREFIX} --test-name-pattern="a\"|b" fixture.test.mjs"#), false);
    }
}

// The supplied native exit and output model a pipeline's final command.
// No shell, compiler, or test runner is executed by these recorder witnesses.
fn review_record_native_exit(label: &str, command: &str, output: &str) -> Value {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start(label, true);
    let mut recorder = turn.recorder();
    recorder.command_started_in("quoted-pipe", command, Some(command),
        Some(&turn.root.to_string_lossy())).unwrap();
    turn.wait_for_snapshots();
    recorder.command_completed_with_exit("quoted-pipe", command, output,
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    turn.finish()
}

#[test]
fn review_cmd_mixed_quotes_cannot_forge_cargo_test_credit() {
    let command = r#"cmd /c cargo test -- '"'"|echo test result: ok. 1 passed""#;
    let checkpoint = review_record_native_exit("cmd-mixed-test", command,
        "test result: ok. 1 passed; 0 failed");
    one_node_test(&checkpoint, "unknown");
    let check = engram_check_command(command).expect("a compound Cargo Test is still observed");
    assert!(!check.simple, "cmd apostrophes cannot hide double-quote transitions");
    eprintln!("cmd mixed quote recorder refused: test");
}

#[test]
fn review_cmd_mixed_quotes_cannot_forge_cargo_build_credit() {
    let command = r#"cmd /c cargo build '"x' "| echo ok " 'x"'"#;
    let checkpoint = review_record_native_exit("cmd-mixed-build", command, "");
    assert!(checkpoint.get("verification_evidence").is_none(),
        "a compound Build has no eligible completion: {checkpoint:#}");
    assert!(engram_check_command(command).is_none(), "a Build requires its own exit");
    eprintln!("cmd mixed quote recorder refused: build");
}

#[test]
fn review_non_bash_smart_quotes_cannot_prove_literal_pipe() {
    for wrapper in ["pwsh -Command ", ""] {
        let command = format!("{wrapper}cargo test -- \"x\u{201d}|echo \"test result: ok. 1 passed\" \"y\u{201d}");
        let check = engram_check_command(&command).expect("the Cargo Test is observed");
        assert!(!check.simple, "unmodelled shell quotes cannot prove containment: {command}");
        assert_eq!(engram_check_command_outcome(&check, EngramCommandExit::Code(0),
            &["test result: ok. 1 passed; 0 failed".to_owned()]),
            Some(EngramExecutionOutcome::Unknown));
        eprintln!("smart quote containment refused: wrapper={wrapper:?}");
    }
}

#[test]
fn review_bash_escaped_quote_keeps_focused_recorder_native_outcome() {
    let command = format!(r#"{PREFIX} --test-name-pattern="a\"|b" fixture.test.mjs"#);
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_bash_test(&format!("bash-escaped-quote-{native}"),
            native, false, &command, &["--test", "--test-name-pattern=a\"|b", "fixture.test.mjs"]);
        fixture.assert_started();
        let checkpoint = fixture.finish(EngramCommandExit::Code(native));
        one_node_test(&checkpoint, if native == 0 { "succeeded" } else { "failed" });
        assert!(checkpoint["verification_evidence"][0]["refs"].as_array().unwrap()
            .contains(&json!(format!("exit:{native}"))), "{checkpoint:#}");
        eprintln!("escaped Bash quote recorder passed: native={native}");
    }
}

#[test]
fn review_bash_quoted_pipe_cannot_hide_no_run() {
    let command = r#"cargo test \''|' --no-run"#;
    assert!(engram_check_command_in(command, EngramShellDialect::Bash).is_none(),
        "recognition must inspect the --no-run after the literal pipe");
}

#[test]
fn review_unknown_single_quotes_do_not_establish_pipe_containment() {
    for argument in ["'a|b'", "a'|'\"b\""] {
        let command = format!("{PREFIX} --test-name-pattern={argument} fixture.test.mjs");
        let check = engram_plain_check_command(&command).expect("the test is observed");
        assert_eq!(check.dialect, EngramShellDialect::Unknown);
        assert!(!check.simple, "single quotes do not protect a pipe under every candidate shell");
        assert_eq!(engram_check_command_outcome(&check, EngramCommandExit::Code(0),
            &["focused: passed".to_owned()]), Some(EngramExecutionOutcome::Unknown));
        // This parser is lexical. A platform-native plain directory makes
        // the positive twin meaningful even for a non-ASCII checkout path.
        let directory = if cfg!(windows) { "C:/termal-tests" } else { "/termal-tests" };
        let positive = format!("pushd \"{directory}\" && {PREFIX} fixture.test.mjs");
        assert!(engram_check_command(&positive).is_some(), "plain one-call positive twin");
        let one_call = format!("pushd \"{directory}\" && {command}");
        assert!(engram_check_command(&one_call).is_none(),
            "one-call placement does not establish the shell dialect");
        eprintln!("unknown quote containment refused: argument={argument}");
    }
}
