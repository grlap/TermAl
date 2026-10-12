// Owns the tests of what a focused launcher run's holder is told when the
// run's own records back a pass but its summary has no `focused:` stage line
// (src/engram_launcher_stages.rs, engram_focused_stage_line_missing): such a
// run, as another project's launcher prints it and as a Codex session wraps
// it in `pwsh -Command`, is withheld with a notice naming the missing line
// rather than "no passing test"; a TermAl launcher run is still credited in
// that wrapped form and in Claude's one-call form; and a run that passed
// nothing still reads as no passing test. Does not own crediting a focused
// run from its records (src/tests/engram_focused_launcher_credit.rs) or the
// withheld-check report itself (src/tests/engram_turn_checks.rs). New module,
// a sibling of the focused-credit tests.
use super::*;

const RUN: &str = "test-23fa8c24-5817-4998-a255-1dfcd2727bf7";
/// The command as a Codex session's runtime reports it.
const WRAPPED: &str = "\"C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe\" -Command 'node scripts/test-launcher.mjs focused -- cargo test --test source_file_size'";
const MISSING_LINE: &str = "its summary has no `focused: passed` stage line";

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
fn runs_dir(turn: &CheckedTurn, label: &str) -> PathBuf {
    turn.state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(format!("launcher-runs-{label}"))
}

fn run_and_finish(turn: &CheckedTurn, command: &str, output: &str) -> Value {
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
    turn.finish()
}

#[test]
fn a_wrapped_focused_run_whose_records_back_a_pass_without_a_stage_line_names_that_line() {
    let turn = CheckedTurn::start("focused-stage-line-missing", true);
    let directory = records(&runs_dir(&turn, "other"), 36);
    let output = other_launcher_summary(&directory);
    assert_eq!(
        engram_launcher_output_test_stages(&output),
        ["focused"],
        "the run's own records back its pass"
    );

    let checkpoint = run_and_finish(&turn, WRAPPED, &output);

    assert_withheld_and_told(&turn, &checkpoint, MISSING_LINE);
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(!line.contains("its output shows no passing test"), "{line}");
    assert!(line.contains("the launcher must print that line"), "{line}");
}

#[test]
fn a_termal_launcher_focused_run_is_still_credited_wrapped_and_in_the_one_call_form() {
    let wrapped = CheckedTurn::start("focused-stage-line-wrapped", true);
    let directory = records(&runs_dir(&wrapped, "wrapped"), 36);
    let checkpoint = run_and_finish(&wrapped, WRAPPED, &termal_launcher_summary(&directory));
    assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded", "{checkpoint:#}");
    assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");

    let one_call = CheckedTurn::start("focused-stage-line-one-call", true);
    let directory = records(&runs_dir(&one_call, "one-call"), 36);
    let command = format!(
        "pushd \"{}\" && node scripts/test-launcher.mjs focused -- cargo test --test source_file_size",
        one_call.root.display()
    );
    let checkpoint = run_and_finish(&one_call, &command, &termal_launcher_summary(&directory));
    assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded", "{checkpoint:#}");
    assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
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
    assert!(!engram_focused_stage_line_missing(&full, &[], &["focused".to_owned()]));
}
