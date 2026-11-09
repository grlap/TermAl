// Owns focused-launcher stage-line diagnostics from actual result artifacts.
// Keeps record-backed pass/zero-test and wrapped/one-call discrimination while
// ordinary EO remains uncredited. Does not register native verification.
use super::*;

const RUN: &str = "test-23fa8c24-5817-4998-a255-1dfcd2727bf7";
/// The command as a Codex session's runtime reports it.
const WRAPPED: &str = "\"C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe\" -Command 'node scripts/test-launcher.mjs focused -- cargo test --test source_file_size'";

/// A focused cargo run's records under `runs`, as the launcher of the other
/// project wrote them for a real run: TermAl's focused request and results
/// shape, with extra fields this host ignores.
fn records(runs: &FsPath, passed: u64) -> PathBuf {
    let directory = runs.join(RUN);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": RUN,
            "stages": [{"name": "focused", "kind": "test", "command": "cargo",
                        "args": ["test", "--test", "source_file_size"]}],
            "full": false,
            "detached": false,
        }))
        .unwrap(),
    )
    .expect("the request should write");
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": RUN,
            "state": "passed",
            "exitCode": 0,
            "stages": [{
                "name": "focused", "state": "passed", "code": 0,
                "tests": {"executed": passed, "passed": passed, "failed": 0, "ignored": 0,
                          "runners": 1, "filteredOut": 0, "failures": 0},
            }],
        }))
        .unwrap(),
    )
    .expect("the results should write");
    directory
}

/// The other launcher's summary: a verdict, its results record, totals, and
/// no per-stage line.
fn other_launcher_summary(directory: &FsPath) -> String {
    format!(
        "STARTED {dir}\npid=108284 completion=host-process-wait\nPASS {RUN} exit=0\n\
         results: {results}\nreason: completed\n\
         stages: total=1 passed=1 failed=0 interrupted=0 running=0 unrun=0 unknown=0\n\
         input: matched; boundary comparison only, not host check credit\n",
        dir = directory.display(),
        results = directory.join("results.json").display(),
    )
}

/// TermAl's launcher summary for the same run, with its stage line.
fn termal_launcher_summary(directory: &FsPath) -> String {
    format!(
        "PASS {RUN} exit=0\nresults: {results}\nfocused: passed exit=0 log={log}\n\
         tests: passed=36 failed=0 ignored=0\n",
        results = directory.join("results.json").display(),
        log = directory.join("focused.log").display(),
    )
}

/// The test runs' directory, outside the session's repository so writing
/// records there touches no checked worktree.
fn runs_dir(turn: &EOCheckedTurn, label: &str) -> PathBuf {
    turn.state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("launcher-runs-{label}"))
}

fn run_and_finish(turn: &EOCheckedTurn, command: &str, output: &str) -> (Value, EngramTurnCheck) {
    let mut recorder = turn.recorder();
    recorder
        .command_started("check-1", command)
        .expect("the start should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "check-1",
            command,
            output,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
    let check = turn.record(|record| record.engram.active_turn_checks[0].clone());
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(turn, &request);
    (request, check)
}

#[test]
fn a_wrapped_focused_run_without_its_stage_line_keeps_the_missing_line_diagnostic() {
    let turn = EOCheckedTurn::start("focused-stage-line-missing", true);
    let directory = records(&runs_dir(&turn, "other"), 36);
    let output = other_launcher_summary(&directory);
    assert_eq!(
        engram_launcher_output_test_stages(&output),
        ["focused"],
        "the run's actual artifact backs its pass"
    );
    let (request, check) = run_and_finish(&turn, WRAPPED, &output);
    let end = check.end.as_ref().unwrap();
    assert!(
        end.focused_stage_line_missing,
        "missing stage line is distinct from zero tests"
    );
    assert!(!end.showed_passing_tests);
    assert_eq!(
        request["occurrence"]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_termal_launcher_focused_run_keeps_its_diagnostics_wrapped_and_in_the_one_call_form() {
    let wrapped = EOCheckedTurn::start("focused-stage-line-wrapped", true);
    let directory = records(&runs_dir(&wrapped, "wrapped"), 36);
    let (_, check) = run_and_finish(&wrapped, WRAPPED, &termal_launcher_summary(&directory));
    let end = check.end.as_ref().unwrap();
    assert!(end.showed_passing_tests);
    assert!(!end.focused_stage_line_missing);
    let one_call = EOCheckedTurn::start("focused-stage-line-one-call", true);
    let directory = records(&runs_dir(&one_call, "one-call"), 36);
    let command = format!(
        "pushd \"{}\" && node scripts/test-launcher.mjs focused -- cargo test --test source_file_size",
        one_call.root.display()
    );
    let (_, check) = run_and_finish(&one_call, &command, &termal_launcher_summary(&directory));
    let end = check.end.as_ref().unwrap();
    assert!(end.showed_passing_tests);
    assert!(!end.focused_stage_line_missing);
}
#[test]
fn only_a_record_backed_pass_without_its_stage_line_gets_the_new_reason() {
    let check = engram_check_command(WRAPPED).expect("a focused test command");
    let temp = TestTempRoot::create("termal-engram-focused-stage-line");
    let directory = records(temp.path(), 36);
    let missing = |output: &str| {
        engram_focused_stage_line_missing(
            &check,
            &engram_check_result_lines(&check.program, output),
            &engram_launcher_output_test_stages(output),
        )
    };
    assert!(missing(&other_launcher_summary(&directory)));
    // TermAl's own summary carries the line.
    assert!(!missing(&termal_launcher_summary(&directory)));

    // A run that passed nothing has no record-backed stage: still no passing test.
    let zero = records(&temp.path().join("zero"), 0);
    assert!(engram_launcher_output_test_stages(&other_launcher_summary(&zero)).is_empty());
    assert!(!missing(&other_launcher_summary(&zero)));

    // A full gate's command never reads as a focused run's.
    let full = engram_check_command("node scripts/test-launcher.mjs full").expect("a full gate");
    assert!(!engram_focused_stage_line_missing(
        &full,
        &[],
        &["focused".to_owned()]
    ));
}
