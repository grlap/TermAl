// A standalone runner flag must retain the same admission and result fences.
// Recorder cases use matched synthetic launcher records, not live-store proof.
use super::*;

const FORCE_EXIT_TEST: &str =
    "node scripts/test-launcher.mjs focused -- node --test --test-force-exit fixture.test.mjs";
const FORCE_EXIT_ARGS: &[&str] = &["--test", "--test-force-exit", "fixture.test.mjs"];

#[test]
fn engram_node_force_exit_recognizes_the_maintained_helper_arguments() {
    // Read the actual exported stage argv, so a later gate flag cannot silently
    // diverge from a duplicated list in this test. Importing starts no tests.
    let launcher = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/test-launcher.mjs");
    let output = crate::host_command::Command::new("node")
        .args(["--input-type=module", "-e", "import { pathToFileURL } from 'node:url'; const { helperTestArgs } = await import(pathToFileURL(process.argv[2]).href); console.log(JSON.stringify(helperTestArgs));"])
        .arg("termal-metadata-probe")
        .arg(launcher)
        .env_remove("NODE_TEST_CONTEXT")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("maintained launcher metadata is readable");
    assert!(output.status.success(), "{:?}", output.stderr);
    let args: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(args.iter().any(|arg| arg == "--test-force-exit"));
    let command = format!(
        "node scripts/test-launcher.mjs focused -- node {}",
        args.join(" ")
    );
    let check =
        engram_check_command(&command).expect("the exact maintained helper argv is recognized");
    assert_eq!(check.kind, EngramVerificationKind::Test);
    let mut expected = vec!["node".to_owned()];
    expected.extend(args);
    let words = engram_shell_words(&check.normalized).unwrap();
    assert_eq!(
        engram_focused_node_words(&check.program, &words[1..]),
        Some(expected)
    );
}

#[test]
fn engram_node_force_exit_preserves_supported_options_and_literal_operands() {
    for arguments in [
        "--test --test-force-exit fixture.test.mjs",
        "--test --test-force-exit --test-timeout=600000 fixture.test.mjs",
        "--test --test-reporter tap --test-force-exit --test-name-pattern \"selected case\" fixture.test.mjs",
        "--test --test-force-exit --test-reporter=spec --test-skip-pattern=skip --test-concurrency=2 fixture.test.mjs",
        "--test --test-force-exit -- --test-force-exit",
    ] {
        let command = format!("node scripts/test-launcher.mjs focused -- node {arguments}");
        let check = engram_check_command(&command).unwrap_or_else(|| panic!("{command}"));
        assert_eq!(check.kind, EngramVerificationKind::Test);
        assert_eq!(check.normalized, command);
        let words = engram_shell_words(&check.normalized).unwrap();
        let expected = engram_shell_words(&format!("node {arguments}")).unwrap();
        assert_eq!(engram_focused_node_words(&check.program, &words[1..]), Some(expected));
    }
}

#[test]
fn engram_node_force_exit_rejects_values_late_flags_and_unreadable_forms() {
    for (arguments, flag) in [
        (
            "--test --test-force-exit=true fixture.test.mjs",
            "--test-force-exit=true",
        ),
        (
            "--test --test-force-exit=false fixture.test.mjs",
            "--test-force-exit=false",
        ),
        (
            "--test fixture.test.mjs --test-force-exit",
            "--test-force-exit",
        ),
        (
            "--test --test-force-exit --watch fixture.test.mjs",
            "--watch",
        ),
        (
            "--test --test-force-exit --test-reporter=json fixture.test.mjs",
            "--test-reporter=json",
        ),
        (
            "--test --test-force-exit --test-reporter=custom.mjs fixture.test.mjs",
            "--test-reporter=custom.mjs",
        ),
    ] {
        let command = format!("node scripts/test-launcher.mjs focused -- node {arguments}");
        assert!(engram_check_command(&command).is_none(), "{command}");
        let notice =
            engram_unsupported_focused_line(&command).expect("unsupported test form is explained");
        assert!(notice.contains("no Test record will follow"), "{notice}");
        assert!(notice.contains(flag), "{notice}");
    }
}

#[test]
fn engram_node_force_exit_records_matched_pass_and_failure_with_native_counts() {
    for one_call in [false, true] {
        for native in [0, 1] {
            let fixture = FocusedNodeFixture::with_test(
                &format!("force-exit-{one_call}-{native}"),
                native,
                one_call,
                FORCE_EXIT_TEST,
                FORCE_EXIT_ARGS,
            );
            one_node_test(
                &fixture.finish(EngramCommandExit::Code(native)),
                if native == 0 { "succeeded" } else { "failed" },
            );
        }
    }
}

#[test]
fn engram_node_force_exit_cannot_rescue_invalid_artifacts_or_counts() {
    for invalid in ["owner", "argv", "zero", "runner"] {
        let mut fixture = FocusedNodeFixture::with_test(
            &format!("force-exit-invalid-{invalid}"),
            0,
            false,
            FORCE_EXIT_TEST,
            FORCE_EXIT_ARGS,
        );
        match invalid {
            "owner" => fixture.request["owner"] = json!("foreign"),
            "argv" => fixture.results["stages"][0]["command"][2] = json!("--watch"),
            "zero" => fixture.results["stages"][0]["tests"]["passed"] = json!(0),
            "runner" => fixture.results["stages"][0]["tests"]["runner"] = json!("cargo-libtest"),
            _ => unreachable!(),
        }
        fixture
            .output
            .push_str("tests: passed=20 failed=0 ignored=0\n");
        one_node_test(&fixture.finish(EngramCommandExit::Code(0)), "unknown");
    }
}

#[test]
fn engram_node_force_exit_keeps_source_and_grant_fences() {
    for changed in ["source", "grant"] {
        let fixture = FocusedNodeFixture::with_test(
            &format!("force-exit-{changed}-changed"),
            0,
            false,
            FORCE_EXIT_TEST,
            FORCE_EXIT_ARGS,
        );
        if changed == "source" {
            fixture.turn.change_readme("changed while Node tests ran\n");
        } else {
            fixture.turn.record_mut(|record| {
                record.engram.active_grant_id = Some("replacement-grant".to_owned())
            });
        }
        assert!(fixture
            .finish(EngramCommandExit::Code(0))
            .get("verification_evidence")
            .is_none());
    }
}

#[test]
fn engram_node_force_exit_native_tap_and_spec_keep_their_counts() {
    for reporter in ["tap", "spec"] {
        for failed in [false, true] {
            let source = if failed {
                "import test from 'node:test'; test('case', () => { throw new Error('fixture failure'); });\n"
            } else {
                "import test from 'node:test'; test('case', () => {});\n"
            };
            let (counts, text) =
                native_node_counts_with_force_exit(reporter, source, i32::from(failed), true);
            assert_eq!(
                counts,
                json!({ "runner": "node-test", "passed": u32::from(!failed),
                "failed": u32::from(failed), "ignored": 0 }),
                "{text}"
            );
        }
    }
}
