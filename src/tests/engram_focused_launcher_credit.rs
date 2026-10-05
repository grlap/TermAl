// Owns the tests of crediting a foreground focused launcher run
// (src/engram_launcher_stages.rs, engram_launcher_focused_record_passed): a
// captured passing focused run is credited from its own results.json, and the
// controls that are not: no counts, zero passed, any failed, a missing,
// unreadable, network or other run's record, counts only in the summary text,
// a failed run, a wrapped non-test or no-run command, and a full gate's
// stages under a focused command. Does not own the full and live rules
// (src/tests/engram_launcher_stages.rs, from which this module is declared).
use super::*;

/// A real passing focused run's summary, request and results records,
/// captured from `node scripts/test-launcher.mjs focused -- cargo test --bin
/// termal -q -- tests::delegation_result_parser`.
const CAPTURED_SUMMARY: &str = include_str!("fixtures/focused-launcher-pass/summary.txt");
const CAPTURED_REQUEST: &str = include_str!("fixtures/focused-launcher-pass/request.json");
const CAPTURED_RESULTS: &str = include_str!("fixtures/focused-launcher-pass/results.json");
const CAPTURED_RUN: &str = "test-f3f7e586-ec86-45f1-af3e-78afa04108cb";
const CAPTURED_DIRECTORY: &str = "C:\\github\\Personal\\TermAl\\.git\\worktrees\\wt-focused-credit-9474\\review-runs\\test-f3f7e586-ec86-45f1-af3e-78afa04108cb";
const FOCUSED_COMMAND: &str =
    "node scripts/test-launcher.mjs focused -- cargo test --bin termal -q -- tests::delegation_result_parser";

/// The captured run placed under `runs`: its records written there, with
/// `results` edited by `edit`, and its summary naming that directory.
fn captured_run(runs: &FsPath, edit: impl FnOnce(&mut Value)) -> (PathBuf, String) {
    let directory = runs.join(CAPTURED_RUN);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    fs::write(directory.join("request.json"), CAPTURED_REQUEST).expect("the request should write");
    let mut results: Value = serde_json::from_str(CAPTURED_RESULTS).expect("captured results");
    edit(&mut results);
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&results).expect("the results serialize"),
    )
    .expect("the results should write");
    let summary = CAPTURED_SUMMARY.replace(CAPTURED_DIRECTORY, &directory.display().to_string());
    (directory, summary)
}

fn focused_showed(command: &str, output: &str) -> bool {
    let check = engram_check_command(command).expect("a focused test command");
    engram_check_showed_passing_tests(
        &check,
        &engram_check_result_lines(&check.program, output),
        &engram_launcher_output_test_stages(output),
    )
}

#[test]
fn a_captured_passing_focused_run_is_credited_from_its_own_results_record() {
    let temp = TestTempRoot::create("termal-engram-focused-credit");
    let (_, summary) = captured_run(temp.path(), |_| {});
    assert!(summary.contains("tests: passed=39 failed=0 ignored=0"));
    assert_eq!(engram_launcher_output_test_stages(&summary), ["focused"]);
    assert!(focused_showed(FOCUSED_COMMAND, &summary));
}

#[test]
fn a_focused_run_is_not_credited_without_counts_its_record_backs() {
    let temp = TestTempRoot::create("termal-engram-focused-controls");
    let cases: Vec<(&str, Box<dyn FnOnce(&mut Value)>)> = vec![
        ("no counts", Box::new(|results| {
            results["stages"][0]
                .as_object_mut()
                .expect("a stage object")
                .remove("tests");
        })),
        ("zero passed", Box::new(|results| {
            results["stages"][0]["tests"]["passed"] = json!(0);
        })),
        ("a failed test", Box::new(|results| {
            results["stages"][0]["tests"]["failed"] = json!(1);
        })),
        ("another run's record", Box::new(|results| {
            results["runId"] = json!("test-other");
        })),
        ("a failed stage", Box::new(|results| {
            results["stages"][0]["state"] = json!("failed");
        })),
    ];
    for (case, edit) in cases {
        let runs = temp.path().join(case.replace(' ', "-"));
        let (_, summary) = captured_run(&runs, edit);
        // The summary still prints its counts line: text alone earns nothing.
        assert!(summary.contains("tests: passed=39"), "{case}");
        assert!(engram_launcher_output_test_stages(&summary).is_empty(), "{case}");
        assert!(!focused_showed(FOCUSED_COMMAND, &summary), "{case}");
    }

    // A missing or unreadable results record.
    let (directory, summary) = captured_run(&temp.path().join("missing"), |_| {});
    fs::remove_file(directory.join("results.json")).unwrap();
    assert!(!focused_showed(FOCUSED_COMMAND, &summary), "missing");
    let (directory, summary) = captured_run(&temp.path().join("unreadable"), |_| {});
    fs::write(directory.join("results.json"), "not json").unwrap();
    assert!(!focused_showed(FOCUSED_COMMAND, &summary), "unreadable");

    // A results record on a network path is never resolved.
    let network = CAPTURED_SUMMARY.replace(CAPTURED_DIRECTORY, r"\\server\share\review-runs\run");
    assert!(engram_launcher_output_test_stages(&network).is_empty());
    assert!(!focused_showed(FOCUSED_COMMAND, &network));
}

#[test]
fn a_failing_focused_run_stays_failed_and_wrapped_non_tests_are_no_checks() {
    let temp = TestTempRoot::create("termal-engram-focused-failed");
    let (directory, _) = captured_run(temp.path(), |results| {
        results["state"] = json!("failed");
        results["exitCode"] = json!(101);
        results["stages"][0]["state"] = json!("failed");
        results["stages"][0]["code"] = json!(101);
        results["stages"][0]["tests"]["failed"] = json!(1);
    });
    let failed = format!(
        "FAIL {CAPTURED_RUN} exit=101\nresults: {}\nfocused: failed exit=101 log=x\ntests: passed=38 failed=1 ignored=0",
        directory.join("results.json").display()
    );
    let check = engram_check_command(FOCUSED_COMMAND).expect("a focused test command");
    let lines = engram_check_result_lines(&check.program, &failed);
    assert!(lines.contains(&"focused: failed".to_owned()), "{lines:?}");
    assert!(!focused_showed(FOCUSED_COMMAND, &failed));

    // A focused run of a command that is no test, or only builds tests, is no
    // test check at all, as before.
    assert!(engram_check_command("node scripts/test-launcher.mjs focused -- node -e 1").is_none());
    assert!(
        engram_check_command("node scripts/test-launcher.mjs focused -- cargo test --no-run")
            .is_none()
    );
}

#[test]
fn a_focused_command_never_takes_a_full_gates_stages() {
    let temp = TestTempRoot::create("termal-engram-focused-full");
    let full = temp.path().join("test-full");
    fs::create_dir_all(&full).unwrap();
    fs::write(
        full.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": "test-full",
            "full": true,
            "stages": [{ "name": "rust-tests" }, { "name": "vitest" }],
        }))
        .unwrap(),
    )
    .unwrap();
    let output = format!(
        "PASS test-full exit=0\nresults: {}\nrust-tests: passed exit=0\nvitest: passed exit=0",
        full.join("results.json").display()
    );
    assert_eq!(engram_launcher_output_test_stages(&output), ["rust-tests", "vitest"]);
    assert!(!focused_showed(FOCUSED_COMMAND, &output));
}

#[test]
fn a_credited_focused_run_is_recorded_as_its_test_command_not_a_full_gate() {
    let temp = TestTempRoot::create("termal-engram-focused-kind");
    let (_, summary) = captured_run(temp.path(), |_| {});
    let check = engram_check_command(FOCUSED_COMMAND).expect("a focused test command");
    let lines = engram_check_result_lines(&check.program, &summary);
    let recorded = engram_check_summary(&check, EngramCommandExit::Code(0), &lines);
    // Recorded as its own command and that command's result lines, the way
    // a plain test command is, not as a gate's stage report.
    assert!(
        recorded.starts_with(&format!("`{}` exited 0", check.normalized)),
        "{recorded}"
    );
    assert!(check.normalized.contains("focused -- cargo test --bin termal"));
    assert!(recorded.contains(&format!("\nPASS {CAPTURED_RUN} exit=0")), "{recorded}");
    assert!(recorded.contains("\nfocused: passed"), "{recorded}");
    assert!(!recorded.contains("full gate"), "{recorded}");
}

#[test]
fn a_focused_record_needs_an_integer_count_and_exit_zero_throughout() {
    let temp = TestTempRoot::create("termal-engram-focused-exits");
    let cases: Vec<(&str, Box<dyn FnOnce(&mut Value)>)> = vec![
        ("run exit 1 while passed", Box::new(|results| {
            results["exitCode"] = json!(1);
        })),
        ("stage code 1 while passed", Box::new(|results| {
            results["stages"][0]["code"] = json!(1);
        })),
        ("passed count as text", Box::new(|results| {
            results["stages"][0]["tests"]["passed"] = json!("39");
        })),
    ];
    for (case, edit) in cases {
        let (_, summary) = captured_run(&temp.path().join(case.replace(' ', "-")), edit);
        assert!(engram_launcher_output_test_stages(&summary).is_empty(), "{case}");
        assert!(!focused_showed(FOCUSED_COMMAND, &summary), "{case}");
    }
}

#[test]
fn a_focused_run_takes_counts_only_from_the_run_its_own_verdict_names() {
    let temp = TestTempRoot::create("termal-engram-focused-verdict");
    // Another run, fully backed by its record.
    let (other, _) = captured_run(&temp.path().join("other"), |_| {});
    // This run's summary names its own record first, but a stage's
    // diagnostics, printed after it, carry a `results:` line naming the
    // other run's record: the host reads the last one.
    let (_, own) = captured_run(&temp.path().join("own"), |results| {
        results["stages"][0]
            .as_object_mut()
            .expect("a stage object")
            .remove("tests");
    });
    let own_run = "test-0000own0-0000-0000-0000-000000000000";
    let injected = format!(
        "{}\nwarning: something\nresults: {}",
        own.replace(CAPTURED_RUN, own_run),
        other.join("results.json").display()
    );
    assert!(engram_launcher_output_test_stages(&injected).is_empty());
    assert!(!focused_showed(FOCUSED_COMMAND, &injected));
    // A summary whose verdict names a different run than its record.
    let (_, summary) = captured_run(&temp.path().join("mismatch"), |_| {});
    let mismatched = summary.replacen(
        &format!("PASS {CAPTURED_RUN} exit=0"),
        "PASS test-someone-else exit=0",
        1,
    );
    assert!(engram_launcher_output_test_stages(&mismatched).is_empty());
    assert!(!focused_showed(FOCUSED_COMMAND, &mismatched));
}

/// A request record that is not a focused run's, under `runs`, with `request`
/// as its record and no results record, and the focused-looking summary that
/// names it.
fn non_focused_record(runs: &FsPath, run_id: &str, request: Value) -> String {
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("request.json"), serde_json::to_vec(&request).unwrap()).unwrap();
    format!(
        "PASS {run_id} exit=0\nresults: {}\nfocused: passed exit=0 log=x",
        directory.join("results.json").display()
    )
}

#[test]
fn a_full_record_with_a_test_stage_named_focused_never_credits_a_focused_command() {
    let temp = TestTempRoot::create("termal-engram-focused-reserved");
    // A kinded full gate whose only test stage is named `focused`: no results
    // record, so no counts, so no credit.
    let kinded = non_focused_record(
        temp.path(),
        "test-kinded",
        json!({
            "runId": "test-kinded",
            "full": true,
            "stages": [{ "name": "focused", "kind": "test" }],
        }),
    );
    assert!(engram_launcher_output_test_stages(&kinded).is_empty(), "kinded");
    assert!(!focused_showed(FOCUSED_COMMAND, &kinded), "kinded");
    // A one-stage `focused` record that does not say it is not a full gate.
    for (run_id, full) in [("test-missing-full", None), ("test-full-no", Some(json!("no")))] {
        let mut request = json!({ "runId": run_id, "stages": [{ "name": "focused", "kind": "test" }] });
        if let Some(full) = full {
            request["full"] = full;
        }
        let summary = non_focused_record(temp.path(), run_id, request);
        assert!(engram_launcher_output_test_stages(&summary).is_empty(), "{run_id}");
        assert!(!focused_showed(FOCUSED_COMMAND, &summary), "{run_id}");
    }
}
