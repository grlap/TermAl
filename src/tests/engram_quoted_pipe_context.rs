// Runtime-qualified literal pipes and one argv across focused verification.
// Synthetic reader provenance and artifacts drive the production recorder;
// these witnesses execute no shell command and claim no live runtime proof.
use super::*;

fn wrapper_must_stay_unknown(label: &str, command: &str) {
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("wrapper-{label}-{native}"), native, false, command, &ARGS,
        );
        fixture.assert_started();
        let checkpoint = fixture.finish(EngramCommandExit::Code(native));
        assert_truthful_refusal(&checkpoint, native, "pipe containment");
        let refs = checkpoint["verification_evidence"][0]["refs"].as_array().unwrap();
        assert!(!refs.contains(&json!(format!("exit:{native}"))), "{checkpoint:#}");
        eprintln!("wrapper containment refused: {label}, native={native}");
    }
}

#[test]
fn bash_wrapper_has_no_literal_pipe_exemption() {
    wrapper_must_stay_unknown("bash", &format!(
        "bash -lc '{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs'"));
}

#[test]
fn powershell_wrapper_has_no_literal_pipe_exemption() {
    wrapper_must_stay_unknown("powershell", &format!(
        "pwsh -Command {PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs"));
}

#[test]
fn cmd_wrapper_has_no_literal_pipe_exemption() {
    wrapper_must_stay_unknown("cmd", &format!(
        "cmd /c {PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs"));
}

#[test]
fn cmd_leading_quote_cannot_supply_cargo_test_credit() {
    let command = r#"cmd /c "cargo" test -- "x|echo test result: ok. 1 passed""#;
    let checkpoint = review_record_native_exit("cmd-leading-quote", command,
        "test result: ok. 1 passed; 0 failed");
    assert_truthful_refusal(&checkpoint, 0, "pipe containment");
}

#[test]
fn powershell_interposition_cannot_supply_cargo_test_credit() {
    let command = r#"pwsh -Command cargo test -- "x|echo test result: ok. 1 passed""#;
    let checkpoint = review_record_native_exit("powershell-interposition", command,
        "test result: ok. 1 passed; 0 failed");
    assert_truthful_refusal(&checkpoint, 0, "pipe containment");
}

#[test]
fn wrappers_cannot_supply_cargo_build_exit_credit() {
    for wrapper in ["bash -lc 'cargo build -- \"x|y\"'",
        "pwsh -Command cargo build -- \"x|y\"", "cmd /c cargo build -- \"x|y\""] {
        let checkpoint = review_record_native_exit("wrapper-cargo-build", wrapper, "");
        assert!(checkpoint.get("verification_evidence").is_none(),
            "a wrapped Build has no eligible native completion: {checkpoint:#}");
    }
}

#[test]
fn unknown_runtime_double_quotes_are_not_an_exemption() {
    let command = format!("{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs");
    let check = engram_check_command(&command).expect("the Test remains observable");
    assert!(!check.simple, "an unwrapped line alone does not identify its shell");
    let fixture = FocusedNodeFixture::with_test("unknown-double-quote", 0, false, &command, &ARGS);
    one_node_test(&fixture.finish(EngramCommandExit::Code(0)), "unknown");
}

#[test]
fn known_bash_unwrapped_quotes_keep_native_results() {
    for (label, pattern) in [("single", "'a|b'"), ("double", "\"a|b\""),
        ("mixed", "a'|'\"b\"")] {
        let command = format!("{PREFIX} --test-name-pattern={pattern} fixture.test.mjs");
        for native in [0, 1] {
            let fixture = FocusedNodeFixture::with_bash_test(
                &format!("direct-bash-{label}-{native}"), native, false, &command, &ARGS);
            fixture.assert_started();
            one_node_test(&fixture.finish(EngramCommandExit::Code(native)),
                if native == 0 { "succeeded" } else { "failed" });
            eprintln!("known Bash literal pipe validated: {label}, native={native}");
        }
    }
}

const ESCAPED_TEST: &str =
    r#"node scripts/test-launcher.mjs focused -- node --te\st --test-name-pattern="a|b" fixture.test.mjs"#;

#[test]
fn known_bash_escaped_token_uses_focused_validation() {
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("direct-bash-token-{native}"), native, false, ESCAPED_TEST, &ARGS);
        // This is the liveness witness: a known Bash token must be decoded
        // before recognition, rather than rejected by the unknown-shell view.
        fixture.assert_started();
        assert!(fixture.turn.record(|record|
            engram_is_focused_node_check(&record.engram.active_turn_checks[0].command)),
            "classification must use the same argv that recognized the Test");
        one_node_test(&fixture.finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" });
        eprintln!("known Bash escaped token validated: native={native}");
    }
}

#[test]
fn known_bash_escaped_quote_keeps_the_complete_argument() {
    let command = format!(r#"{PREFIX} --test-name-pattern="a\"|b" fixture.test.mjs"#);
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("direct-bash-quote-{native}"), native, false, &command,
            &["--test", "--test-name-pattern=a\"|b", "fixture.test.mjs"]);
        fixture.assert_started();
        one_node_test(&fixture.finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" });
        eprintln!("known Bash escaped quote validated: native={native}");
    }
}

#[test]
fn escaped_token_never_falls_back_to_generic_artifact_credit() {
    for defect in ["owner", "interval", "argv", "input"] {
        // A wrapper with an escaped grammar token must not select a weaker
        // completion path that borrows generic stage counts.
        let command = format!("bash -lc '{ESCAPED_TEST}'");
        let mut fixture = FocusedNodeFixture::with_test(
            &format!("wrapped-token-{defect}"), 0, false, &command, &ARGS);
        corrupt_artifact(&mut fixture, defect);
        let checkpoint = fixture.finish(EngramCommandExit::Code(0));
        one_node_test(&checkpoint, "unknown");
        eprintln!("wrapped escaped token artifact refused: {defect}");
    }
}

fn corrupt_artifact(fixture: &mut FocusedNodeFixture, defect: &str) {
    match defect {
        "owner" => fixture.request["owner"] = json!("foreign-owner"),
        "interval" => {
            fixture.request["started"] = json!("2000-01-01T00:00:00.000Z");
            fixture.results["started"] = json!("2000-01-01T00:00:00.000Z");
            fixture.results["ended"] = json!("2000-01-01T00:00:01.000Z");
            fixture.results["stages"][0]["started"] = json!("2000-01-01T00:00:00.000Z");
            fixture.results["stages"][0]["ended"] = json!("2000-01-01T00:00:01.000Z");
        }
        "argv" => fixture.request["stages"][0]["args"][2] = json!("other.test.mjs"),
        "input" => fixture.input["fingerprint"] = json!("e".repeat(64)),
        _ => panic!("unknown fixture corruption"),
    }
}

#[test]
fn known_bash_literal_pipe_refuses_each_invalid_artifact() {
    // An ordinary grammar token reaches each artifact guard independently
    // of the escaped-token classification and wrapper-containment controls.
    let command = format!("{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs");
    for defect in ["owner", "interval", "argv", "input"] {
        let mut fixture = FocusedNodeFixture::with_bash_test(
            &format!("direct-literal-{defect}"), 0, false, &command, &ARGS);
        fixture.assert_started();
        corrupt_artifact(&mut fixture, defect);
        let checkpoint = fixture.finish(EngramCommandExit::Code(0));
        one_node_test(&checkpoint, "unknown");
        assert!(!checkpoint["verification_evidence"][0]["refs"].as_array().unwrap()
            .contains(&json!("exit:0")), "{checkpoint:#}");
        eprintln!("known Bash literal pipe artifact refused: {defect}");
    }
}

#[test]
fn known_bash_one_call_has_a_positive_twin_in_the_same_directory() {
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("direct-bash-one-call-{native}"), native, true,
            &format!("{PREFIX} --test-name-pattern='a|b' fixture.test.mjs"), &ARGS);
        // Prove this exact fixture directory is readable by the one-call parser.
        let positive = format!("pushd \"{}\" && {NODE_TEST}", fixture.turn.root.display());
        assert!(engram_check_command(&positive).is_some(), "plain one-call control must resolve");
        fixture.assert_started();
        one_node_test(&fixture.finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" });
        eprintln!("known Bash one-call validated: native={native}");
    }
}

#[test]
fn ordinary_pipe_free_focused_artifacts_still_validate() {
    for native in [0, 1] {
        one_node_test(&FocusedNodeFixture::new(&format!("pipe-free-context-{native}"), native)
            .finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" });
    }
}

#[test]
fn known_bash_pipeline_cannot_borrow_a_focused_native_exit() {
    let command = format!("{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs | cat");
    let fixture = FocusedNodeFixture::with_bash_test(
        "known-bash-pipeline", 0, false, &command, &ARGS);
    fixture.assert_started();
    assert_truthful_refusal(&fixture.finish(EngramCommandExit::Code(0)),
        0, "compound command");
}

#[test]
fn qualified_escaped_token_keeps_every_artifact_guard() {
    for defect in ["owner", "interval", "argv", "input"] {
        let mut fixture = FocusedNodeFixture::with_bash_test(
            &format!("qualified-token-{defect}"), 0, false, ESCAPED_TEST, &ARGS);
        fixture.assert_started();
        corrupt_artifact(&mut fixture, defect);
        assert_truthful_refusal(&fixture.finish(EngramCommandExit::Code(0)), 0, defect);
        eprintln!("qualified escaped token artifact guard reached: {defect}");
    }
}

#[test]
fn qualified_launcher_cannot_borrow_generic_stage_counts() {
    let command = "node scripts/test-launcher.mjs focused -- cargo test --filter=\"a|b\"";
    let fixture = FocusedNodeFixture::with_bash_test(
        "qualified-generic-counts", 0, false, command, &ARGS);
    fixture.assert_started();
    let checkpoint = fixture.finish(EngramCommandExit::Code(0));
    assert_truthful_refusal(&checkpoint, 0, "argv");
}

#[test]
fn qualified_description_preserves_argv_and_refuses_substitution() {
    for changed in [false, true] {
        let fixture = FocusedNodeFixture::with_bash_test(
            &format!("qualified-description-{changed}"), 0, false, ESCAPED_TEST, &ARGS);
        fixture.assert_started();
        let described = if changed {
            ESCAPED_TEST.replace("fixture.test.mjs", "other.test.mjs")
        } else { ESCAPED_TEST.to_owned() };
        let mut recorder = fixture.turn.recorder();
        recorder.set_observation_provenance(fixture.provenance.clone());
        recorder.command_described("node", Some(&described),
            Some(&fixture.turn.root.to_string_lossy())).unwrap();
        if changed {
            assert!(fixture.turn.record(|record| record.engram.active_turn_checks.is_empty()),
                "a described argv substitution must drop its original check");
            assert!(fixture.finish(EngramCommandExit::Code(0))
                .get("verification_evidence").is_none());
        } else {
            fixture.assert_started();
            one_node_test(&fixture.finish(EngramCommandExit::Code(0)), "succeeded");
        }
        eprintln!("qualified command description checked: changed={changed}");
    }
}

#[test]
fn qualified_pipe_does_not_prove_expanded_or_unsupported_argv() {
    for suffix in ["{a,b}", "*.mjs", "$'a\\\'|b'", "$\"a|b\"", "x^\"|y\"", "a$"] {
        let command = format!(r#"{PREFIX} --test-name-pattern="a\"|b" {suffix}"#);
        assert!(engram_check_command_in(&command, EngramShellDialect::Bash)
            .is_none_or(|check| !check.simple), "unsupported argv must not qualify: {command}");
    }
    let plain = format!("{PREFIX} --test-name-pattern=\"a|b\" fixture.test.mjs");
    assert!(engram_check_command_in(&plain, EngramShellDialect::Bash).unwrap().simple);
}

#[test]
fn qualified_plain_runner_end_must_match_its_started_argv() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for changed in [false, true] {
        let turn = CheckedTurn::start(&format!("qualified-cargo-end-{changed}"), true);
        let provenance = turn.record(|record|
            EngramObservationProvenance::Claude(ClaudeObservationProvenance {
                token: record.runtime.runtime_token().unwrap(),
                origin: ClaudeWorkOrigin::Attempt { turn_generation: record.active_turn_generation },
            }));
        let mut recorder = turn.recorder();
        recorder.set_observation_provenance(provenance);
        let started = r#"cargo test --filter="a|b""#;
        recorder.command_started_in("cargo", started, Some(started),
            Some(&turn.root.to_string_lossy())).unwrap();
        turn.wait_for_snapshots();
        assert_eq!(turn.record(|record| record.engram.active_turn_checks.len()), 1);
        recorder.command_completed_with_exit("cargo",
            if changed { r#"cargo test --filter="c|d""# } else { started },
            "test result: ok. 1 passed; 0 failed", CommandStatus::Success,
            EngramCommandExit::Code(0)).unwrap();
        turn.wait_for_snapshots();
        let checkpoint = turn.finish();
        if changed {
            assert_truthful_refusal(&checkpoint, 0, "association");
        } else {
            one_node_test(&checkpoint, "succeeded");
        }
        eprintln!("qualified plain runner end checked: changed={changed}");
    }
}
