// Owns the tests that a test-launcher check which never ran a test stage is
// recorded as unknown, never as a failed test: a launcher that refuses
// before its run starts (a registration abort prints only `FAIL launcher: …`
// and exits 1), in the one-call form and the plain, cwd-reporting form; a
// launcher run whose own results record every stage unrun, even when its
// summary prints a `FAIL <run> exit=1` verdict; and, as the counterpart, a run
// that did execute, which still ends failed or passed as before. Does not own
// the recognition of launcher commands or their result lines in general
// (src/tests/engram_one_call_checks.rs, src/tests/engram_turn_checks.rs) or a
// launcher full gate's carried credit (src/tests/engram_carried_checks.rs).
// New module, a child of the turn-check tests, whose `CheckedTurn` fixture it
// uses.
use super::*;

/// The launch line of the aborted live run the item records, as its runtime
/// ran it.
const LIVE_RUN: &str = "node scripts/test-launcher.mjs live --engram-binary C:/engram.exe \
     --engram-sha256 c548d5bceba9a9a96a716d813b4ecfd3b60da13ec84a13d7b6cc6316113705f4 -- \
     tests::live";

/// The output of that run: the shell's echo of its directory, the launcher's
/// run line, and its refusal at registration, before any stage started.
const REGISTRATION_ABORT: &str = "/c/github/Personal/TermAl/.tmp/wt /c/github/Personal/TermAl/.tmp/wt\n\
RUN C:\\github\\Personal\\TermAl\\.git\\worktrees\\wt\\review-runs\\test-7f571048-f068-4b41-9dd7-fdd6c6d24ae9\n\
FAIL launcher: registration error: POST send aborted; no tests started; phase=post_headers; \
category=aborted; runId=test-7f571048-f068-4b41-9dd7-fdd6c6d24ae9; attempt=1; httpStatus=unavailable; \
operation=registration; elapsedMs=8006.3; phaseElapsedMs=8006.2; budgetMs=8000; remainingMs=0\n";

/// A native absolute directory for recognition alone.
fn launcher_dir() -> &'static str {
    if cfg!(windows) {
        r"C:\launcher\root"
    } else {
        "/launcher/root"
    }
}

/// The outcome `exit` gives the launcher `command` with `output`, as the
/// check's end reads it.
fn launcher_outcome(
    command: &str,
    output: &str,
    exit: EngramCommandExit,
) -> Option<EngramExecutionOutcome> {
    let check = engram_check_command(command)
        .unwrap_or_else(|| panic!("`{command}` is a recognised check"));
    let lines = engram_check_result_lines(&check.program, output);
    engram_check_command_outcome(&check, exit, &lines)
}

#[test]
fn a_launcher_that_refused_before_its_run_started_ends_unknown_never_failed() {
    let one_call = format!("pushd \"{}\" && {LIVE_RUN}", launcher_dir());
    for command in [one_call.as_str(), LIVE_RUN] {
        assert_eq!(
            launcher_outcome(command, REGISTRATION_ABORT, EngramCommandExit::Code(1)),
            Some(EngramExecutionOutcome::Unknown),
            "`{command}`: a refusal before any stage is no test failure"
        );
    }
    // Every other refusal the launcher prints before its run starts.
    for refusal in [
        "FAIL launcher: failed before admission: registration error: missing or malformed \
         TERMAL_SESSION_ID",
        "FAIL launcher: no terminal result was saved: spawn ENOENT",
        "FAIL launcher: unknown mode",
    ] {
        for command in [one_call.as_str(), LIVE_RUN] {
            assert_eq!(
                launcher_outcome(command, refusal, EngramCommandExit::Code(1)),
                Some(EngramExecutionOutcome::Unknown),
                "`{command}`: {refusal}"
            );
        }
    }
}

#[test]
fn a_launcher_run_that_executed_still_ends_failed_or_passed() {
    let full = "node scripts/test-launcher.mjs full";
    let one_call = format!("pushd \"{}\" && {full}", launcher_dir());
    for command in [one_call.as_str(), full] {
        // The run's verdict and a failed test stage: the run executed.
        assert_eq!(
            launcher_outcome(
                command,
                "RUN C:/r/test-1\nFAIL test-1 exit=1\nrust-tests: failed exit=101\n",
                EngramCommandExit::Code(1),
            ),
            Some(EngramExecutionOutcome::Failed),
            "`{command}`"
        );
        // A failed stage line alone is the run's own output too.
        assert_eq!(
            launcher_outcome(command, "vitest: failed exit=1\n", EngramCommandExit::Code(1)),
            Some(EngramExecutionOutcome::Failed),
            "`{command}`"
        );
        assert_eq!(
            launcher_outcome(
                command,
                "PASS test-1 exit=0\nrust-tests: passed exit=0\n",
                EngramCommandExit::Code(0),
            ),
            Some(EngramExecutionOutcome::Succeeded),
            "`{command}`"
        );
    }
}

/// The five stages of TermAl's own full gate.
const FULL_STAGES: [&str; 5] = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];

/// Writes in `parent` the records of the full-gate run `run`, ended failed
/// with exit 1: its stages `states`, in the order of `FULL_STAGES`, a stage
/// that ran with its code. Returns its run directory.
fn write_failed_run(parent: &FsPath, run: &str, states: [&str; 5]) -> PathBuf {
    let directory = parent.join(run);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    let fingerprint = "f".repeat(64);
    let started = "2026-10-20T00:00:00.000Z";
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": run,
            "root": parent,
            "full": true,
            "detached": false,
            "started": started,
            "expectedFingerprint": fingerprint,
            "stages": FULL_STAGES.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
        }))
        .unwrap(),
    )
    .expect("the request should write");
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run,
            "state": "failed",
            "started": started,
            "ended": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "exitCode": 1,
            "expectedFingerprint": fingerprint,
            "before": fingerprint,
            "after": fingerprint,
            "stages": FULL_STAGES
                .iter()
                .zip(states)
                .map(|(name, state)| match state {
                    "unrun" => json!({ "name": name, "state": "unrun" }),
                    _ => json!({
                        "name": name,
                        "state": state,
                        "code": if state == "passed" { 0 } else { 1 },
                    }),
                })
                .collect::<Vec<_>>(),
        }))
        .unwrap(),
    )
    .expect("the results should write");
    directory
}

/// A foreground summary of the run at `directory` whose verdict names
/// `verdict_run`.
fn failed_summary(verdict_run: &str, directory: &FsPath) -> String {
    format!(
        "RUN {}\nFAIL {verdict_run} exit=1\nresults: {}\n",
        directory.display(),
        directory.join("results.json").display()
    )
}

#[test]
fn only_the_runs_own_record_of_no_stage_run_makes_its_failure_unknown() {
    let temp = TestTempRoot::create("termal-engram-launcher-not-run");
    let unrun = ["unrun"; 5];
    let none_ran = write_failed_run(temp.path(), "test-none-ran", unrun);
    assert!(engram_launcher_output_ran_no_stage(&failed_summary(
        "test-none-ran",
        &none_ran
    )));
    // A run that failed at a stage ran: its later stages unrun do not make it
    // a run that tested nothing.
    let ran = write_failed_run(
        temp.path(),
        "test-ran",
        ["passed", "passed", "passed", "failed", "unrun"],
    );
    assert!(!engram_launcher_output_ran_no_stage(&failed_summary("test-ran", &ran)));
    // A `results:` line of another run, printed after this run's verdict
    // (inside a stage's diagnostics), is not this run's record.
    assert!(!engram_launcher_output_ran_no_stage(&failed_summary(
        "test-ran", &none_ran
    )));
    // A summary with no verdict, or a record that names another run, says
    // nothing either.
    assert!(!engram_launcher_output_ran_no_stage(&format!(
        "results: {}\n",
        none_ran.join("results.json").display()
    )));
    let renamed = temp.path().join("test-renamed");
    fs::create_dir_all(&renamed).unwrap();
    for name in ["request.json", "results.json"] {
        fs::copy(none_ran.join(name), renamed.join(name)).unwrap();
    }
    assert!(!engram_launcher_output_ran_no_stage(&failed_summary(
        "test-renamed",
        &renamed
    )));
    // A record of a passed run, or one with no stages, ran no stage only
    // when it failed with stages all unrun.
    assert!(!engram_launcher_record_ran_no_stage(
        &json!({ "state": "passed", "stages": [{ "name": "vitest", "state": "unrun" }] })
    ));
    assert!(!engram_launcher_record_ran_no_stage(
        &json!({ "state": "failed", "stages": [] })
    ));
}

#[test]
fn a_carried_gate_that_ran_no_stage_is_refused_never_recorded_failed() {
    let temp = TestTempRoot::create("termal-engram-launcher-not-run-carried");
    let read = |directory: &FsPath| {
        let digest = engram_terminal_record_digest(directory).expect("a terminal record");
        engram_read_carried_run(directory, &digest)
    };
    let none_ran = write_failed_run(temp.path(), "test-none-ran", ["unrun"; 5]);
    assert_eq!(read(&none_ran), Err(ENGRAM_CARRIED_RUN_NO_STAGE_RAN));
    // A gate that failed at a stage is recorded failed, as before.
    let ran = write_failed_run(
        temp.path(),
        "test-ran",
        ["passed", "passed", "passed", "failed", "unrun"],
    );
    let verdict = read(&ran).expect("a gate that failed at a stage is recorded");
    assert!(!verdict.passed);
    // Failed at its first stage, it still ran one.
    let first = write_failed_run(
        temp.path(),
        "test-first",
        ["failed", "unrun", "unrun", "unrun", "unrun"],
    );
    assert!(!read(&first).expect("a gate that failed at its first stage is recorded").passed);
}

#[test]
fn a_launcher_run_whose_results_record_every_stage_unrun_ends_unknown() {
    const FULL: &str = "node scripts/test-launcher.mjs full";
    let turn = CheckedTurn::start("launcher-not-run", true);
    turn.recorder()
        .command_started_in("gate", FULL, Some(FULL), Some(&turn.root.to_string_lossy()))
        .expect("the start should record");
    turn.wait_for_snapshots();

    // The run's own records: a full gate refused before its first stage (as
    // the disk-capacity check refuses), every stage unrun.
    let run = format!("test-{}", uuid::Uuid::new_v4());
    let directory = engram_git_run_directory(&turn.root)
        .expect("the fixture is a Git repository")
        .join(&run);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    let stages = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": run,
            "root": turn.root,
            "full": true,
            "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
        }))
        .unwrap(),
    )
    .expect("the request should write");
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run,
            "state": "failed",
            "exitCode": 1,
            "stages": stages
                .iter()
                .map(|name| json!({ "name": name, "state": "unrun" }))
                .collect::<Vec<_>>(),
            "error": "capacity: the full-gate floor is not met",
        }))
        .unwrap(),
    )
    .expect("the results should write");
    let output = format!(
        "RUN {}\nFAIL {run} exit=1\nresults: {}\n",
        directory.display(),
        directory.join("results.json").display()
    );
    turn.recorder()
        .command_completed_with_exit(
            "gate",
            FULL,
            &output,
            CommandStatus::Error,
            EngramCommandExit::Code(1),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();

    let outcomes = checkpoint["observations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|observation| observation["outcome"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !outcomes.iter().any(|outcome| outcome == "failed"),
        "a run that executed no stage records no failed test: {checkpoint:#}"
    );
    assert!(
        outcomes.iter().any(|outcome| outcome == "unknown"),
        "its end is recorded as unknown: {checkpoint:#}"
    );
}
