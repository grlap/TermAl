// Owns the tests of carried background and detached launcher gates
// (src/engram_carried_checks.rs): which command results mark only a gate's
// launch; which of the holder's commands only read; how a run's directory is
// found and its terminal record read; a background run and a detached run
// credited at a checkpoint; each refusal (a write the host observed, a
// terminal record changed after the first terminal read, a source basis
// changed since the launch, an input fingerprint that changed during the run,
// a claim or root generation changed, two terminal reads that disagree)
// told to the holder; the fence ending at the run's end but for the watcher;
// a launcher checked while its run ends; the fence's turn starts; the merge
// of the lines about checks' credit; and the marker a host restart leaves.
// Does not own the in-turn check tests (src/tests/engram_turn_checks.rs,
// whose `CheckedTurn` fixture this child module uses). New module.
use super::*;

const GATE: &str = "node scripts/test-launcher.mjs full";
const DETACHED_GATE: &str = "node scripts/test-launcher.mjs full --detach";

#[test]
fn a_full_gate_launch_is_carried_dropped_or_ordinary_by_its_line_and_result() {
    use EngramLaunchDisposition::{Carry, Drop, Ordinary};
    let check = |line: &str| engram_check_command(line).expect("a recognised test");
    let disposition = |line: &str, exit| engram_launch_disposition(&check(line), exit);
    assert_eq!(disposition(GATE, EngramCommandExit::NotFinished), Carry);
    assert_eq!(disposition(GATE, EngramCommandExit::Code(0)), Ordinary);
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::Code(0)),
        Carry
    );
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::ReportedSuccess),
        Carry
    );
    assert_eq!(
        disposition(DETACHED_GATE, EngramCommandExit::NotFinished),
        Carry
    );
    // A detached launch that failed started no run: dropped, never recorded.
    for exit in [EngramCommandExit::Code(1), EngramCommandExit::Unknown] {
        assert!(
            matches!(disposition(DETACHED_GATE, exit), Drop(why) if why.contains("launch failed")),
            "{exit:?}"
        );
    }
    // A launch on a line that runs more than the gate is dropped: what the
    // rest wrote in that call is outside the fence.
    // A foreground one that finished is an ordinary check.
    let compound = "node scripts/test-launcher.mjs full; git checkout -- README.md";
    for (line, exit) in [
        (compound, EngramCommandExit::NotFinished),
        (
            "node scripts/test-launcher.mjs full --detach --notify session-1 && echo x > README.md",
            EngramCommandExit::NotFinished,
        ),
        (
            "node scripts/test-launcher.mjs full --detach --notify session-1 && echo x > README.md",
            EngramCommandExit::Code(0),
        ),
        (
            "node scripts/test-launcher.mjs full --detach&& git checkout -- README.md",
            EngramCommandExit::Code(0),
        ),
    ] {
        assert!(
            matches!(disposition(line, exit), Drop(why) if why.contains("runs more than the gate")),
            "{line} {exit:?}"
        );
    }
    assert_eq!(disposition(compound, EngramCommandExit::Code(0)), Ordinary);
    assert_eq!(
        disposition(
            "node scripts/test-launcher.mjs live",
            EngramCommandExit::NotFinished
        ),
        Ordinary
    );
    assert_eq!(
        disposition("cargo test", EngramCommandExit::NotFinished),
        Ordinary
    );
    // Only a full gate may be detached and still be recognised.
    assert!(engram_check_command("node scripts/test-launcher.mjs live --detach").is_none());
    assert!(
        engram_check_command("node scripts/test-launcher.mjs focused --detach -- cargo test")
            .is_none()
    );
}

#[test]
fn only_a_command_that_reads_leaves_a_carried_check_unfenced() {
    let workdir = if cfg!(windows) { r"C:\repo" } else { "/repo" };
    for reads in [
        "git status",
        "git diff --stat",
        "node scripts/test-launcher.mjs summary C:/repo/.git/review-runs/test-1",
        "cat README.md",
    ] {
        assert!(engram_command_reads_only(reads, workdir), "{reads}");
    }
    for writes in [
        "cargo build",
        "echo changed > README.md",
        "node scripts/test-launcher.mjs full",
        "node scripts/test-launcher.mjs summary a b",
        "node scripts/test-launcher.mjs summary run; rm README.md",
        "git checkout -- README.md",
        // Only the repository's own launcher, run from its root, counts.
        "node .tmp/my-test-launcher.mjs summary run",
        "node tools/scripts/test-launcher.mjs summary run",
        "node C:/elsewhere/scripts/test-launcher.mjs summary run",
        "node /elsewhere/scripts/test-launcher.mjs summary run",
    ] {
        assert!(!engram_command_reads_only(writes, workdir), "{writes}");
    }
    // The same launcher written absolute, or from the one-call form.
    let absolute = format!(
        "node {}/scripts/test-launcher.mjs summary run",
        workdir.replace('\\', "/")
    );
    assert!(engram_command_reads_only(&absolute, workdir), "{absolute}");
    let one_call = format!("pushd \"{workdir}\" && node ./scripts/test-launcher.mjs summary run");
    assert!(
        engram_command_reads_only(&one_call, "/somewhere/else"),
        "{one_call}"
    );
}

#[test]
fn a_fence_names_the_holders_command_without_its_line() {
    let cause = engram_own_command_cause(
        "curl -H \"Authorization: Bearer sk-secret-123\" https://example.com -o out.txt",
    );
    assert!(cause.contains("`curl` command"), "{cause}");
    assert!(!cause.contains("sk-secret-123"), "{cause}");
    assert!(!cause.contains("Authorization"), "{cause}");
    let one_call = engram_own_command_cause("pushd \"C:/repo\" && cargo build --token=abc");
    assert!(one_call.contains("`cargo` command"), "{one_call}");
    assert!(!one_call.contains("abc"), "{one_call}");
    // A leading assignment is skipped, whatever its value.
    for (line, program, secret) in [
        (
            "GITHUB_TOKEN=ghp_secret123 gh api repos",
            "`gh` command",
            "ghp_secret123",
        ),
        (
            "env API_KEY=abc123 npm run build",
            "`npm` command",
            "abc123",
        ),
    ] {
        let cause = engram_own_command_cause(line);
        assert!(cause.contains(program), "{cause}");
        assert!(!cause.contains(secret), "{cause}");
    }
}

/// A process that has exited, with its pid, which fits every platform's pid
/// type. The caller keeps the child: on Windows, holding its handle keeps the
/// pid from being reused while the test runs.
fn exited_process() -> (std::process::Child, u32) {
    #[cfg(windows)]
    let mut child = {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        // No console window: a test must not flash one on the desktop.
        Command::new("cmd")
            .args(["/C", "exit 0"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("a short process should start")
    };
    #[cfg(not(windows))]
    let mut child = Command::new("true")
        .spawn()
        .expect("a short process should start");
    let pid = child.id();
    assert!(child.wait().expect("the process should exit").success());
    (child, pid)
}

/// A run directory named `run_id` under `runs`, with a request for `root`
/// started at `started` and a terminal record: `state`, its stages each
/// `stage_state`, and fingerprints expected `fingerprint`, after `after`.
fn write_run(
    runs: &FsPath,
    run_id: &str,
    root: &FsPath,
    started: &str,
    state: &str,
    after: &str,
) -> PathBuf {
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).expect("the run directory should be created");
    let stages = [
        "cargo-check",
        "typescript",
        "fingerprint-tests",
        "rust-tests",
        "vitest",
    ];
    let fingerprint = "f".repeat(64);
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": run_id,
            "root": root.to_string_lossy(),
            "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
            "full": true,
            "detached": false,
            "started": started,
            "expectedFingerprint": fingerprint,
        }))
        .expect("the request serializes"),
    )
    .expect("the request should write");
    let passed = state == "passed";
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run_id,
            "state": state,
            "started": started,
            "stages": stages
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let stage_passed = passed || index < 3;
                    json!({
                        "name": name,
                        "state": if stage_passed { "passed" } else { "failed" },
                        "code": if stage_passed { 0 } else { 101 },
                    })
                })
                .collect::<Vec<_>>(),
            "expectedFingerprint": fingerprint,
            "before": fingerprint,
            "after": after,
            "exitCode": if passed { 0 } else { 101 },
            "ended": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }))
        .expect("the results serialize"),
    )
    .expect("the results should write");
    directory
}

#[test]
fn a_run_record_is_credited_only_as_it_was_first_read_and_consistent() {
    let temp = TestTempRoot::create("termal-engram-carried-run");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let started = "2026-09-29T00:00:00.000Z";
    let passed = write_run(
        temp.path(),
        "test-passed",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    let digest = engram_terminal_record_digest(&passed).expect("a terminal record");
    let verdict = engram_read_carried_run(&passed, &digest).expect("a consistent passed run");
    assert!(verdict.passed);
    assert_eq!(verdict.exit_code, 0);
    assert_eq!(verdict.stages.len(), 5);
    assert_eq!(verdict.fingerprint, fingerprint);
    assert_eq!(
        engram_read_carried_run(&passed, &"0".repeat(64)),
        Err("its terminal record changed after the host first read it as terminal")
    );

    let failed = write_run(
        temp.path(),
        "test-failed",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let digest = engram_terminal_record_digest(&failed).expect("a terminal record");
    let verdict = engram_read_carried_run(&failed, &digest).expect("a consistent failed run");
    assert!(!verdict.passed);
    assert_eq!(verdict.exit_code, 101);

    let drifted = write_run(
        temp.path(),
        "test-drifted",
        &root,
        started,
        "passed",
        &"e".repeat(64),
    );
    let digest = engram_terminal_record_digest(&drifted).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&drifted, &digest),
        Err("its input fingerprint after the run differs from the one before")
    );

    // A record with no fingerprint from before or after its stages (a run
    // that ended in its preflight) cannot say what input it tested.
    let unmeasured = write_run(
        temp.path(),
        "test-unmeasured",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(unmeasured.join("results.json")).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("before");
    record.as_object_mut().unwrap().remove("after");
    fs::write(
        unmeasured.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&unmeasured).expect("a terminal record");
    assert!(
        engram_read_carried_run(&unmeasured, &digest)
            .is_err_and(|why| why.contains("no input fingerprint from before or after")),
    );

    // A record that says passed while a stage it requested did not.
    let lying = write_run(
        temp.path(),
        "test-lying",
        &root,
        started,
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(lying.join("results.json")).expect("the results read"))
            .expect("the results parse");
    record["state"] = json!("passed");
    record["exitCode"] = json!(0);
    fs::write(
        lying.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&lying).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&lying, &digest),
        Err("its terminal record says passed, but not every stage it requested passed with code 0")
    );

    // A run whose stages all passed but which requested no test stage the
    // host recognises: a foreground gate would not be credited on it either.
    let compile_only = write_run(
        temp.path(),
        "test-compile-only",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    for name in ["request.json", "results.json"] {
        let mut record: Value =
            serde_json::from_slice(&fs::read(compile_only.join(name)).unwrap()).unwrap();
        record["stages"] = json!([{ "name": "cargo-check", "state": "passed", "code": 0 }]);
        fs::write(
            compile_only.join(name),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let digest = engram_terminal_record_digest(&compile_only).expect("a terminal record");
    assert_eq!(
        engram_read_carried_run(&compile_only, &digest),
        Err("its run requested no test stage the host recognises")
    );

    // A run still going has no terminal record.
    let running = write_run(
        temp.path(),
        "test-running",
        &root,
        started,
        "passed",
        &fingerprint,
    );
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-running", "state": "running" })).unwrap(),
    )
    .unwrap();
    assert_eq!(engram_terminal_record_digest(&running), None);
    // No recorded pid: its launcher is never judged gone.
    assert_eq!(engram_carried_run_progress(&running), (None, false));
    // A recorded pid whose process has exited: gone, so the run will never
    // end.
    let (_exited, dead_pid) = exited_process();
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(
            &json!({ "runId": "test-running", "state": "running", "pid": dead_pid }),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(engram_carried_run_progress(&running), (None, true));

    // Engram's launcher settles a dead run as stopped, with no `ended` and no
    // exit code: terminal all the same, and neither a pass nor a failure.
    fs::write(
        running.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": "test-running",
            "state": "stopped",
            "stopped": { "stage": "rust-tests" },
        }))
        .unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&running).expect("a stopped record is terminal");
    assert_eq!(
        engram_read_carried_run(&running, &digest),
        Err(ENGRAM_CARRIED_RUN_NEITHER)
    );

    // A run that ended stopped, or failed but interrupted, is terminal and
    // is neither a pass nor a failure.
    for (name, state, interrupted) in [
        ("test-stopped", "stopped", false),
        ("test-interrupted", "failed", true),
    ] {
        let directory = write_run(temp.path(), name, &root, started, "failed", &fingerprint);
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
        record["state"] = json!(state);
        if interrupted {
            record["interrupted"] = json!(true);
        }
        fs::write(
            directory.join("results.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let digest =
            engram_terminal_record_digest(&directory).expect("an ended record is terminal");
        assert_eq!(
            engram_read_carried_run(&directory, &digest),
            Err(ENGRAM_CARRIED_RUN_NEITHER),
            "{name}"
        );
    }
}

#[test]
fn a_launch_takes_the_earliest_full_run_of_its_root_that_started_after_it() {
    let temp = TestTempRoot::create("termal-engram-carried-run");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let fingerprint = "f".repeat(64);
    // Minutes apart, well beyond the slack a launch stamp is given.
    let at = |minute: u32| format!("2026-09-29T00:{minute:02}:00.000Z");
    write_run(&runs, "test-before", &root, &at(1), "passed", &fingerprint);
    write_run(
        &runs,
        "test-elsewhere",
        &temp.path().join("other"),
        &at(3),
        "passed",
        &fingerprint,
    );
    let first = write_run(&runs, "test-first", &root, &at(4), "passed", &fingerprint);
    let second = write_run(&runs, "test-second", &root, &at(5), "passed", &fingerprint);

    assert_eq!(engram_git_run_directory(&root), Some(runs.clone()));
    let none = std::collections::BTreeSet::new();
    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(2), &[], &none);
    assert_eq!(found, Some(first.clone()));
    // Runs that can never be this launch's are ruled out for good.
    let mut ruled_out = ruled_out;
    ruled_out.sort();
    assert_eq!(
        ruled_out,
        [runs.join("test-before"), runs.join("test-elsewhere")]
    );
    let (found, _, _) =
        engram_find_carried_run("session-1", &root, &at(2), &[first.clone()], &none);
    assert_eq!(found, Some(second.clone()));
    assert_eq!(
        engram_find_carried_run("session-1", &root, &at(6), &[], &none).0,
        None
    );
    // A ruled-out run is not read again, so a search sees only the others.
    let skip: std::collections::BTreeSet<PathBuf> = [first.clone(), runs.join("test-second")]
        .into_iter()
        .collect();
    assert_eq!(
        engram_find_carried_run("session-1", &root, &at(2), &[], &skip).0,
        None
    );
    // A run the launcher records as another session's is never this one's;
    // a request with no owner may be anyone's.
    let mut request: Value =
        serde_json::from_slice(&fs::read(first.join("request.json")).unwrap()).unwrap();
    request["owner"] = json!("session-2");
    fs::write(
        first.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(2), &[], &none);
    assert_eq!(found, Some(second));
    assert!(ruled_out.contains(&first), "{ruled_out:?}");
    assert_eq!(
        engram_find_carried_run("session-2", &root, &at(2), &[], &none).0,
        Some(first)
    );
}

#[test]
fn a_run_that_ends_while_its_launcher_is_checked_is_read_as_ended() {
    let temp = TestTempRoot::create("termal-engram-carried-liveness");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let directory = write_run(
        temp.path(),
        "test-racing",
        &root,
        "2026-09-29T00:00:00.000Z",
        "passed",
        &fingerprint,
    );
    let terminal = fs::read(directory.join("results.json")).unwrap();
    let running = |pid: u64| {
        serde_json::to_vec(&json!({ "runId": "test-racing", "state": "running", "pid": pid }))
            .unwrap()
    };

    // The launcher writes its terminal record and exits between the read
    // that found none and the check that finds it gone: read again, ended.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    let (digest, gone) = engram_carried_run_progress_with(&directory, |pid, _| {
        assert_eq!(pid, 41);
        fs::write(directory.join("results.json"), &terminal).unwrap();
        false
    });
    assert_eq!(digest, Some(sha256_hex(&terminal)));
    assert!(!gone);

    // A detached creator hands over to its worker between the read and the
    // check: the worker is checked too, and it is alive.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    let checked = std::cell::RefCell::new(Vec::new());
    let (digest, gone) = engram_carried_run_progress_with(&directory, |pid, _| {
        checked.borrow_mut().push(pid);
        if pid == 41 {
            fs::write(directory.join("results.json"), running(42)).unwrap();
            false
        } else {
            true
        }
    });
    assert_eq!((digest, gone), (None, false));
    assert_eq!(*checked.borrow(), [41, 42]);

    // Still not terminal after the check, under the same process: gone.
    fs::write(directory.join("results.json"), running(41)).unwrap();
    assert_eq!(
        engram_carried_run_progress_with(&directory, |_, _| false),
        (None, true)
    );
}

/// The claimed turn's named source root registered as the host records a
/// name, under a store the turn's project uses, so a carried check's
/// settlement can find its generation.
fn register_named_root(turn: &CheckedTurn, worktree: &FsPath, generation: u64) {
    let (root, common_dir_key) = turn.record(|record| {
        let root = record
            .engram
            .active_turn_source_root
            .as_ref()
            .expect("the root is named");
        (root.root.clone(), root.common_dir_key.clone())
    });
    let binding = turn.record(|record| {
        record
            .engram
            .work_binding
            .clone()
            .expect("the turn is bound")
    });
    let store = EngramAuthorityStoreKey {
        database_path: turn.root.join("engram.db"),
        project_id: "github.com/example/carried".to_owned(),
    };
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let project_id = {
        let index = inner
            .find_session_index(&turn.session_id)
            .expect("root should exist");
        inner.sessions[index]
            .session
            .project_id
            .clone()
            .expect("the root belongs to a project")
    };
    let project = inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("the project exists");
    project
        .engram
        .as_mut()
        .expect("the project uses Engram")
        .authority_store_key = Some(store.clone());
    inner.engram_work_source_roots = vec![EngramWorkSourceRoot {
        store,
        work_id: binding.work_id,
        short_ref: "w-carried".to_owned(),
        claim_id: binding.claim_id,
        claim_fence: binding.claim_fence,
        root,
        common_dir_key,
        named_by_session: turn.session_id.clone(),
        named_at: "2026-09-29T00:00:00.000Z".to_owned(),
        generation,
    }];
    let _ = worktree;
}

/// A claimed turn measured in a named linked worktree, registered as named.
fn named_turn(label: &str) -> (CheckedTurn, PathBuf) {
    let turn = CheckedTurn::start(label, true);
    let worktree = name_turn_source_root(&turn);
    register_named_root(&turn, &worktree, 0);
    (turn, worktree)
}

/// Launches the full gate in `worktree`, as a runtime that reports the
/// command's directory does: in the background (its result marks only the
/// launch) or detached (`--detach`, whose launch returns at once).
fn launch_gate(turn: &CheckedTurn, worktree: &FsPath, detached: bool) {
    let command = if detached { DETACHED_GATE } else { GATE };
    let cwd = fs::canonicalize(worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", command, Some(command), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            command,
            "STARTED",
            CommandStatus::Success,
            if detached {
                EngramCommandExit::Code(0)
            } else {
                EngramCommandExit::NotFinished
            },
        )
        .expect("the launch result should record");
    let carried = turn.record(|record| record.engram.carried_checks.len());
    assert_eq!(carried, 1, "the gate is carried past its launch");
    // The launch's source snapshot is taken on its own thread.
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
}

/// The launcher's run directory for `worktree`, from Git itself.
fn review_runs(worktree: &FsPath) -> PathBuf {
    PathBuf::from(
        run_git_test_command_output(
            worktree,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "review-runs",
            ],
        )
        .trim(),
    )
}

/// Writes the gate's run for `worktree`, started after its launch, ending
/// `state` with its input fingerprint after the run `after`.
fn finish_run(worktree: &FsPath, state: &str, after: &str) -> PathBuf {
    let runs = review_runs(worktree);
    let canonical = fs::canonicalize(worktree).expect("the worktree canonicalizes");
    write_run(
        &runs,
        "test-carried",
        &canonical,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        state,
        after,
    )
}

/// Asserts the checkpoint carries a test verification of the gate, whose
/// producer ended `outcome`, and that its summary names the run directory and
/// the input fingerprint.
fn assert_credited(turn: &CheckedTurn, checkpoint: &Value, run: &FsPath, outcome: &str) {
    let producer = observations(checkpoint)
        .into_iter()
        .find(|observation| observation["effect"] == "observe" && observation["outcome"] == outcome)
        .unwrap_or_else(|| panic!("the gate's producer ends {outcome}: {checkpoint:#}"));
    assert_eq!(producer["source_changed"], false);
    let verification = &checkpoint["verification_evidence"][0];
    assert_eq!(verification["check_kind"], "test", "{checkpoint:#}");
    let summary = verification["summary"].as_str().unwrap_or_default();
    assert!(summary.contains("test-carried"), "{summary}");
    assert!(summary.contains(&"f".repeat(64)), "{summary}");
    assert!(
        summary.contains(&run.to_string_lossy().to_string()) || summary.contains("run directory"),
        "{summary}"
    );
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "a settled gate is no longer carried"
    );
}

#[test]
fn a_background_gate_whose_run_passed_is_credited_at_the_checkpoint() {
    let (turn, worktree) = named_turn("carried-background");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn a_detached_gate_whose_run_passed_is_credited_at_the_checkpoint() {
    let (turn, worktree) = named_turn("carried-detached");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn a_background_gate_whose_run_failed_is_recorded_as_failed() {
    let (turn, worktree) = named_turn("carried-failed");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "failed");
}

#[test]
fn a_gate_whose_run_has_not_ended_stays_carried() {
    let (turn, worktree) = named_turn("carried-running");
    launch_gate(&turn, &worktree, false);
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
}

/// Runs `command` to its end in `worktree` as the holder's own command.
fn run_own_command(turn: &CheckedTurn, worktree: &FsPath, key: &str, command: &str) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(key, command, Some(command), Some(&cwd))
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            key,
            command,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
}

#[test]
fn a_read_only_line_of_another_runtime_still_refuses_the_gate() {
    // Only a Claude session's lines are read as Bash; another runtime's
    // line may run under PowerShell, where the same text can mean a write.
    let (turn, worktree) = named_turn("carried-codex-read-only");
    launch_gate(&turn, &worktree, false);
    run_own_command(&turn, &worktree, "look", "git status");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "this session ran a `git` command there");
}

#[test]
fn a_read_only_command_in_the_worktree_leaves_the_gate_credited() {
    let (turn, worktree) = named_turn("carried-read-only");
    // A Claude session's lines are its Bash tool's.
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    launch_gate(&turn, &worktree, false);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("look", "git status", Some("git status"), Some(&cwd))
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            "look",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

/// Asserts the checkpoint credits no gate, the gate is no longer carried,
/// and its holder was told it earned no credit, and `why`.
fn assert_refused(turn: &CheckedTurn, checkpoint: &Value, why: &str) {
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("background full gate")
            && line.contains("earned no credit")
            && line.contains(why),
        "{line}"
    );
}

#[test]
fn a_command_of_the_holder_that_writes_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-own-write");
    launch_gate(&turn, &worktree, false);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    turn.recorder()
        .command_started_in("build", "cargo build", Some("cargo build"), Some(&cwd))
        .expect("the start should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "this session ran a `cargo` command there",
    );
}

#[test]
fn an_edit_the_holder_reports_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-own-edit");
    launch_gate(&turn, &worktree, false);
    turn.state.note_engram_workspace_edit(&turn.session_id);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "this session reported a file edit");
}

#[test]
fn a_write_after_the_run_ended_leaves_the_gate_to_the_basis_check() {
    // Once the host has read the run as terminal, a write can no longer reach
    // what it tested: the holder's own command or edit does not refuse it,
    // and a change still there at settlement is caught by the basis.
    let (turn, worktree) = named_turn("carried-after-end");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    run_own_command(&turn, &worktree, "commit", "git commit -m done");
    turn.state.note_engram_workspace_edit(&turn.session_id);
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");

    let (turn, worktree) = named_turn("carried-after-end-changed");
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    run_own_command(&turn, &worktree, "edit", "sed -i s/a/b/ README.md");
    fs::write(worktree.join("README.md"), "changed after the run\n").unwrap();
    let checkpoint = turn.finish();
    assert_refused(
        &turn,
        &checkpoint,
        "the host's source basis at settlement differs from its basis at the launch",
    );
}

#[test]
fn a_write_through_termal_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-host-write");
    launch_gate(&turn, &worktree, false);
    let written = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_host_write(&written);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "TermAl wrote");
}

#[test]
fn another_session_running_a_command_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-other-session");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    launch_gate(&turn, &worktree, false);
    SessionRecorder::new(turn.state.clone(), other)
        .command_started("other-edit", "git checkout -- README.md")
        .expect("the other session's command should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "was in a turn or ran a command there");
}

#[test]
fn a_terminal_record_changed_after_the_first_terminal_read_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-rewritten");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["note"] = json!("rewritten");
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "its terminal record changed after the host first read it as terminal",
    );
}

#[test]
fn a_source_change_the_host_did_not_see_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-moved");
    launch_gate(&turn, &worktree, false);
    fs::write(worktree.join("README.md"), "changed behind the host\n").unwrap();
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "the host's source basis at settlement differs from its basis at the launch",
    );
}

#[test]
fn an_input_fingerprint_that_changed_during_the_run_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-drifted");
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"e".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "its input fingerprint after the run differs from the one before",
    );
}

#[test]
fn a_run_that_ended_stopped_is_dropped_and_never_recorded() {
    let (turn, worktree) = named_turn("carried-stopped");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["state"] = json!("stopped");
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "ended neither passed nor failed");
}

#[test]
fn a_run_whose_launcher_is_gone_is_dropped_and_never_recorded() {
    let (turn, worktree) = named_turn("carried-launcher-gone");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let (_exited, dead_pid) = exited_process();
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": "test-carried",
            "state": "running",
            "pid": dead_pid,
        }))
        .unwrap(),
    )
    .unwrap();
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "its launcher ended without recording a terminal result",
    );
}

#[test]
fn a_root_named_again_before_settlement_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-renamed");
    launch_gate(&turn, &worktree, false);
    register_named_root(&turn, &worktree, 7);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "its claim was released, or its source root renamed or cleared",
    );
}

#[test]
fn a_host_restart_tells_the_holder_its_carried_gate_lost_its_credit() {
    let (turn, worktree) = named_turn("carried-restart");
    launch_gate(&turn, &worktree, false);
    let persisted = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record))
            .expect("the record serializes")
    });
    assert_eq!(
        persisted["engramCarriedLaunches"].as_array().map(Vec::len),
        Some(1),
        "{persisted:#}"
    );
    let loaded: PersistedSessionRecord =
        serde_json::from_value(persisted).expect("the record deserializes");
    let record = loaded.into_record().expect("the record loads");

    assert!(record.engram.carried_checks.is_empty());
    let line = record
        .engram
        .pending_source_root_line
        .expect("the holder should be told");
    assert!(
        line.contains("the host restarted while the background full gate")
            && line.contains("earned no credit"),
        "{line}"
    );
}

/// Starts `command` in `worktree` and ends it `exit`, as the holder's
/// launch, without asserting what became of it.
fn launch_command(turn: &CheckedTurn, worktree: &FsPath, command: &str, exit: EngramCommandExit) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", command, Some(command), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit("gate", command, "", CommandStatus::Success, exit)
        .expect("the launch result should record");
}

/// Asserts no check of the launch is kept or carried, and the holder was
/// told `why` it earned no credit.
fn assert_dropped_at_launch(turn: &CheckedTurn, why: &str) {
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains(ENGRAM_CHECK_CREDIT_LINE_PREFIX) && line.contains(why),
        "{line}"
    );
}

#[test]
fn a_detached_launch_that_failed_is_dropped_and_never_recorded() {
    let (turn, worktree) = named_turn("carried-failed-launch");
    launch_command(&turn, &worktree, DETACHED_GATE, EngramCommandExit::Code(1));
    assert_dropped_at_launch(&turn, "its detached launch failed, so no run started");
    let checkpoint = turn.finish();
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
}

#[test]
fn a_launch_on_a_line_that_runs_more_than_the_gate_is_dropped() {
    let (turn, worktree) = named_turn("carried-compound");
    launch_command(
        &turn,
        &worktree,
        "node scripts/test-launcher.mjs full --detach --notify session-9 && echo x",
        EngramCommandExit::Code(0),
    );
    assert_dropped_at_launch(&turn, "its line runs more than the gate");
    let checkpoint = turn.finish();
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
}

#[test]
fn a_launch_outside_a_named_root_or_beside_another_command_is_not_carried() {
    // No named root for the turn.
    let turn = CheckedTurn::start("carried-unnamed", true);
    launch_command(
        &turn,
        &turn.root.clone(),
        GATE,
        EngramCommandExit::NotFinished,
    );
    assert_dropped_at_launch(&turn, "not measured in a named source root");

    // A named root, but the gate ran in the repository's main worktree.
    let (turn, _worktree) = named_turn("carried-elsewhere");
    launch_command(
        &turn,
        &turn.root.clone(),
        GATE,
        EngramCommandExit::NotFinished,
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));

    // Another command running as it launched.
    let (turn, worktree) = named_turn("carried-beside");
    turn.recorder()
        .command_started("other", "git status")
        .expect("the other command should record");
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
}

#[test]
fn a_command_later_described_as_running_in_the_worktree_refuses_the_gate() {
    // An ACP runtime may give a command's directory only in a later update.
    let (turn, worktree) = named_turn("carried-described");
    launch_gate(&turn, &worktree, false);
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "late",
            "write-things",
            Some("write-things"),
            Some(&turn.root.to_string_lossy()),
        )
        .expect("the start should record");
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    recorder
        .command_described("late", None, Some(&cwd))
        .expect("the description should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "whose line TermAl was not told");
}

#[test]
fn a_file_change_the_watcher_saw_in_the_worktree_refuses_the_gate() {
    let (turn, worktree) = named_turn("carried-watched");
    launch_gate(&turn, &worktree, false);
    let change = |path: PathBuf| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    // A change elsewhere leaves it alone.
    turn.state
        .note_engram_workspace_file_changes(&[change(turn.root.join("README.md"))]);
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state
        .note_engram_workspace_file_changes(&[change(changed.clone())]);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "a file in its worktree changed");

    // The watcher reports late, so a change it reports after the run was
    // read as ended still refuses it: it may have landed while it ran.
    let (turn, worktree) = named_turn("carried-watched-late");
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state
        .note_engram_workspace_file_changes(&[change(changed)]);
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}

#[test]
fn a_gate_launched_in_the_one_call_form_is_carried_and_credited() {
    // The form the instructions give a Claude session, whose runtime reports
    // no directory: the line itself names where the gate runs.
    let (turn, worktree) = named_turn("carried-one-call");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    assert!(
        engram_one_call_reads(&root),
        "a plain test directory: {root}"
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let mut recorder = turn.recorder();
    recorder
        .command_started("gate", &line)
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
    // Reading its summary from the same form leaves it unfenced.
    let summary =
        format!("pushd \"{root}\" && node scripts/test-launcher.mjs summary test-carried");
    let mut recorder = turn.recorder();
    recorder.command_started("look", &summary).unwrap();
    recorder
        .command_completed_with_exit(
            "look",
            &summary,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn another_session_starting_a_turn_fences_a_carried_gate_of_its_own_worktree_only() {
    // It has reported nothing since TermAl started, so its worktree is
    // resolved as its turn starts rather than taken to be every one.
    for same_worktree in [true, false] {
        let (turn, worktree) = named_turn("carried-turn-start");
        launch_gate(&turn, &worktree, false);
        let other = test_session_id(&turn.state, Agent::Codex);
        let other_workdir = if same_worktree {
            fs::canonicalize(&worktree).unwrap()
        } else {
            sibling_worktree(&turn, "carried-turn-start-elsewhere")
        };
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            inner.sessions[index].session.workdir = other_workdir.to_string_lossy().into_owned();
        }
        let dispatched = turn
            .state
            .dispatch_turn(
                &other,
                SendMessageRequest {
                    text: "Edit the README.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("the other session should start a turn");
        assert!(matches!(dispatched, DispatchTurnResult::Dispatched(_)));

        assert_eq!(
            turn.record(|record| record.engram.carried_checks[0].fence.is_some()),
            same_worktree,
            "same_worktree={same_worktree}"
        );
    }
}

#[test]
fn a_launch_whose_run_never_appeared_is_refused_as_not_found() {
    let (turn, worktree) = named_turn("carried-missing");
    launch_gate(&turn, &worktree, false);
    let long_ago = (chrono::Utc::now() - chrono::Duration::minutes(11))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.record_mut(|record| record.engram.carried_checks[0].check.started_at = long_ago);
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "no run of it appeared within ten minutes",
    );
}

#[test]
fn a_run_that_never_ends_is_dropped_after_six_hours() {
    let (turn, worktree) = named_turn("carried-expired");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-carried", "state": "running" })).unwrap(),
    )
    .unwrap();
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| record.engram.carried_checks[0].check.started_at = long_ago);
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "did not end within six hours");
}

#[test]
fn a_fifth_launch_drops_the_oldest_carried_gate() {
    let (turn, worktree) = named_turn("carried-limit");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for index in 0..=ENGRAM_CARRIED_CHECK_LIMIT {
        let key = format!("gate-{index}");
        let mut recorder = turn.recorder();
        // Detached launches, whose results end their commands: a background
        // launch's command stays running and would overlap the next one.
        recorder
            .command_started_in(&key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
            .expect("the launch should record");
        turn.wait_for_snapshots();
        recorder
            .command_completed_with_exit(
                &key,
                DETACHED_GATE,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the launch result should record");
    }
    let keys = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .map(|carried| carried.check.key.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(keys, ["gate-1", "gate-2", "gate-3", "gate-4"]);
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder of the dropped gate is told");
    assert!(
        line.contains("a newer background gate took its place"),
        "{line}"
    );
}

#[test]
fn a_carried_gate_whose_claim_changed_is_refused() {
    let (turn, worktree) = named_turn("carried-claim");
    launch_gate(&turn, &worktree, false);
    turn.record_mut(|record| record.engram.carried_checks[0].claim_id = "claim-other".to_owned());
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "its claim was released");
}

#[test]
fn a_gate_launched_in_one_turn_is_credited_at_the_next_turns_checkpoint() {
    const NEXT_GRANT: &str = "turn-check-grant-next";
    let turn = CheckedTurn::start_with_turns(
        "carried-next-turn",
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            grant_reply(NEXT_GRANT),
            begin_reply(NEXT_GRANT),
            checkpoint_reply(NEXT_GRANT),
        ],
        2,
    );
    let worktree = name_turn_source_root(&turn);
    register_named_root(&turn, &worktree, 0);
    launch_gate(&turn, &worktree, false);
    let first = turn.finish();
    assert!(first.get("verification_evidence").is_none(), "{first:#}");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);

    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let dispatch = match turn
        .state
        .dispatch_turn(
            &turn.session_id,
            SendMessageRequest {
                text: "Read the gate's summary.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the next turn should reach admission")
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("an idle root should dispatch"),
    };
    deliver_turn_dispatch(&turn.state, dispatch).expect("the next turn should be delivered");
    let runtime_token = turn.record(|record| {
        record
            .runtime
            .runtime_token()
            .expect("the begun turn should own the runtime")
    });
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    let settling = turn
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .last()
        .expect("the next turn is checkpointed")
        .request;

    assert_eq!(settling["grant_id"], NEXT_GRANT, "{settling:#}");
    assert_credited(&turn, &settling, &run, "succeeded");
    let refs = settling["verification_evidence"][0]["refs"].clone();
    assert!(
        refs.as_array().is_some_and(|refs| refs
            .iter()
            .any(|reference| *reference == format!("launched-under-grant:{CHECK_GRANT}"))),
        "{refs:#}"
    );
    // The producer's id is built from the grant the gate launched under.
    let carried_key = format!("{}:{CHECK_GRANT}:0:gate", turn.session_id);
    let producer_id = sha256_hex(format!("termal-turn-check:{carried_key}").as_bytes());
    assert!(
        observations(&settling)
            .iter()
            .any(|observation| observation["observation_id"] == producer_id),
        "{settling:#}"
    );
}

#[test]
fn a_run_recovered_as_failed_and_interrupted_is_neither_credited_nor_recorded() {
    // The exact shape both launchers' `recover` writes.
    let temp = TestTempRoot::create("termal-engram-carried-recovered");
    let root = temp.path().join("root");
    let fingerprint = "f".repeat(64);
    let directory = write_run(
        temp.path(),
        "test-recovered",
        &root,
        "2026-09-29T00:00:00.000Z",
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(directory.join("results.json")).unwrap()).unwrap();
    record["exitCode"] = json!(1);
    record["interrupted"] = json!(true);
    record["error"] = json!("interrupted: the launcher died before the run ended");
    record["stages"][3] = json!({
        "name": "rust-tests",
        "state": "failed",
        "error": "interrupted: the launcher died before the run ended",
        "outcome": "unknown",
    });
    record["recovered"] = json!({ "by": "recover" });
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let digest = engram_terminal_record_digest(&directory).expect("a recovered record is terminal");

    assert_eq!(
        engram_read_carried_run(&directory, &digest),
        Err(ENGRAM_CARRIED_RUN_NEITHER)
    );
}

#[test]
fn check_credit_lines_read_cleanly_and_merge_without_evicting_bind_lines() {
    let check = engram_check_command(GATE).expect("a recognised test");
    let lines = [
        engram_dropped_launch_line(&check, "its detached launch failed, so no run started"),
        engram_carried_lost_to_restart_line(&EngramCarriedLaunchMarker {
            root: "C:/repo".to_owned(),
            launched_at: "2026-09-29T00:00:00.000Z".to_owned(),
            fingerprint: "a".repeat(64),
        }),
        ENGRAM_CARRIED_RUN_NEITHER.to_owned(),
    ];
    for line in &lines {
        assert!(!line.contains("  "), "no run of spaces: {line:?}");
    }
    let mut engram = EngramSessionState::default();
    engram.set_pending_source_root_line("[TermAl] bind line".to_owned());
    for index in 0..6 {
        engram.set_pending_source_root_line(format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate {index} earned no credit."
        ));
    }
    let pending = engram
        .pending_source_root_line
        .clone()
        .expect("lines are pending");
    let pending = pending.lines().collect::<Vec<_>>();
    assert_eq!(pending.len(), 2, "{pending:?}");
    assert_eq!(pending[0], "[TermAl] bind line");
    assert!(
        pending[1].contains("gate 0") && pending[1].contains("gate 5"),
        "{}",
        pending[1]
    );

    // A line a prompt in flight carries is not merged into: its delivery
    // takes it out when the prompt is accepted, and the new line waits.
    let carried_line = pending[1].to_owned();
    engram.source_root_line_delivery = Some((carried_line.clone(), 1));
    engram.set_pending_source_root_line(format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate 6 earned no credit."
    ));
    let pending = engram.pending_source_root_line.clone().unwrap();
    let pending = pending.lines().collect::<Vec<_>>();
    assert!(pending.contains(&carried_line.as_str()), "{pending:?}");
    assert!(
        pending
            .iter()
            .any(|line| line.contains("gate 6") && !line.contains("gate 5")),
        "{pending:?}"
    );

    // Past the bound, the newest is kept and the front is cut.
    let mut merged = format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate first earned no credit.");
    for index in 0..60 {
        merged = engram_merge_credit_lines(
            &merged,
            &format!(
                "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} gate {index} earned no credit: why {index}."
            ),
        );
        assert!(
            merged.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES,
            "{}",
            merged.len()
        );
        assert!(
            merged.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX),
            "{merged}"
        );
    }
    assert!(
        merged.ends_with("gate 59 earned no credit: why 59."),
        "{merged}"
    );
    assert!(!merged.contains("gate first"), "{merged}");
    assert!(merged.contains("earlier lines cut"), "{merged}");
}

#[test]
fn two_terminal_reads_that_disagree_refuse_the_gate() {
    // Polls read off the lock: a later terminal read of another record,
    // stored after the first, marks the conflict rather than being ignored.
    let (turn, worktree) = named_turn("carried-conflict");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let digest = engram_terminal_record_digest(&run).expect("a terminal record");
    turn.record_mut(|record| {
        let carried = &mut record.engram.carried_checks[0];
        engram_note_carried_run_read(
            carried,
            Some(run.clone()),
            Some("0".repeat(64)),
            false,
            false,
            Vec::new(),
        );
        engram_note_carried_run_read(
            carried,
            Some(run.clone()),
            Some(digest.clone()),
            false,
            false,
            Vec::new(),
        );
        assert!(carried.terminal_conflict);
    });
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "two reads that found its run terminal saw different records",
    );
}

#[test]
fn a_carried_gate_is_kept_until_it_can_be_settled_and_told_when_it_never_was() {
    let (turn, worktree) = named_turn("carried-kept");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let generation = Some(0);
    turn.record_mut(|record| {
        let (grant, sequence) = {
            let carried = &record.engram.carried_checks[0];
            (carried.check.grant_id.clone(), carried.check.sequence)
        };
        // A snapshot not ready in time: kept, still pinned, for later.
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            vec![(grant, sequence, Ok(None))],
            &[generation],
            chrono::Utc::now(),
        );
        assert!(credited.is_empty() && lines.is_empty(), "{lines:?}");
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert!(record.engram.carried_checks[0].terminal_digest.is_some());
        // Not settled at all (the holder's checkpoints were on another claim
        // or root): kept while young.
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            Vec::new(),
            &[generation],
            chrono::Utc::now(),
        );
        assert!(credited.is_empty() && lines.is_empty(), "{lines:?}");
        assert_eq!(record.engram.carried_checks.len(), 1);
        // Six hours on, its run ended but it was never settled: told so.
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            Vec::new(),
            &[generation],
            chrono::Utc::now() + chrono::Duration::hours(7),
        );
        assert!(credited.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("was not settled within six hours"),
            "{}",
            lines[0]
        );
    });
    let _ = run;
}

#[test]
fn a_turn_a_stop_drains_from_the_queue_fences_a_carried_gate_of_its_own_worktree_only() {
    // The stop starts the queued turn itself, for a session whose worktree
    // TermAl has not resolved (it has reported nothing since TermAl
    // started): it is resolved before that turn starts, rather than taken
    // to be every worktree.
    let (turn, worktree) = named_turn("carried-stop-drain");
    launch_gate(&turn, &worktree, false);
    let other = test_session_id(&turn.state, Agent::Cursor);
    let runtime = |name: &str| {
        let owner = phase_sync::ParkedProcess::spawn();
        let (input_tx, input_rx) = mpsc::channel();
        let handle = AcpRuntimeHandle {
            agent: AcpAgent::Cursor,
            runtime_id: format!("cursor-carried-{name}"),
            input_tx,
            process: owner.process.clone(),
            turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
        };
        (owner, input_rx, handle)
    };
    let (_original_owner, _original_rx, original) = runtime("original");
    let (_successor_owner, _successor_rx, successor) = runtime("successor");
    turn.state
        .install_test_acp_runtime_override(AcpAgent::Cursor, successor);
    let elsewhere = sibling_worktree(&turn, "carried-stop-drain-elsewhere");
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        let record = &mut inner.sessions[index];
        record.session.workdir = elsewhere.to_string_lossy().into_owned();
        record.engram.workdir_worktree = None;
        record.runtime = SessionRuntime::Acp(original);
        record.session.status = SessionStatus::Active;
        record.queued_prompts.push_back(QueuedPromptRecord {
            engram_waiting: false,
            promoted_message_index: None,
            promotion_disposition_known: true,
            engram_bind: None,
            engram_evaluate: None,
            engram_interrupted: false,
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: "queued-carried-stop-drain".to_owned(),
                timestamp: stamp_now(),
                text: "Edit the README.".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
        sync_pending_prompts(record);
    }
    turn.state
        .stop_session_with_options(
            &other,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: true,
                pause_automatic_resumes_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .expect("the stop should start the queued turn");
    let (drained, resolved) = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        (
            inner.sessions[index].queued_prompts.is_empty(),
            engram_session_worktree(&inner.sessions[index]),
        )
    };
    assert!(drained, "the stop started the queued turn");
    assert_eq!(resolved, Some(engram_worktree_root(&elsewhere)));

    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None
    );
}

#[test]
fn a_conflict_found_after_the_settlement_copy_still_refuses_the_gate() {
    // The checkpoint settles a copy off the lock; a poll that read off the
    // lock too may store a disagreeing terminal read meanwhile.
    let (turn, worktree) = named_turn("carried-late-conflict");
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let (copy, workers) = turn.record(|record| {
        (
            record.engram.carried_checks[0].clone(),
            record.engram.capture_workers.clone(),
        )
    });
    let settled =
        engram_settle_carried_check(&copy, &workers, std::time::Instant::now() + DEADLOCK_GUARD);
    assert!(
        matches!(settled, Ok(Some(_))),
        "{:?}",
        settled.as_ref().map(Option::is_some)
    );
    turn.record_mut(|record| {
        record.engram.carried_checks[0].terminal_conflict = true;
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            vec![(copy.check.grant_id.clone(), copy.check.sequence, settled)],
            &[Some(0)],
            chrono::Utc::now(),
        );
        assert!(credited.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("two reads that found its run terminal saw different records"),
            "{}",
            lines[0]
        );
    });
}

#[test]
fn a_fenced_gate_claims_no_run_and_is_no_longer_polled() {
    // A fenced gate can never be credited, so it is not read again and finds
    // no run.
    let (turn, worktree) = named_turn("carried-fenced-claim");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for key in ["first", "second"] {
        let mut recorder = turn.recorder();
        recorder
            .command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
            .expect("the launch should record");
        turn.wait_for_snapshots();
        recorder
            .command_completed_with_exit(
                key,
                DETACHED_GATE,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .expect("the launch result should record");
    }
    turn.record_mut(|record| {
        assert_eq!(record.engram.carried_checks.len(), 2);
        record.engram.carried_checks[0].fence = Some("a test fence".to_owned());
    });
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();

    let directories = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .map(|carried| (carried.check.key.clone(), carried.run_directory.clone()))
            .collect::<Vec<_>>()
    });
    // The fenced first finds no run; the second, launched while the first
    // had none matched, is ambiguous and takes none either.
    let _ = run;
    assert_eq!(
        directories,
        [("first".to_owned(), None), ("second".to_owned(), None)]
    );
    let now = chrono::Utc::now();
    turn.record(|record| {
        let carried = &record.engram.carried_checks;
        assert!(!engram_carried_check_polls(&carried[0], now), "fenced");
        assert!(carried[1].ambiguous);
        assert!(!engram_carried_check_polls(&carried[1], now), "ambiguous");
    });
}

#[test]
fn a_failed_gate_launched_in_the_one_call_form_keeps_its_exit() {
    // Its stages' own lines say the exit is the gate's, not its `pushd`'s.
    let (turn, worktree) = named_turn("carried-one-call-failed");
    turn.record_mut(|record| record.session.agent = Agent::Claude);
    let root = engram_source_root_display(
        &fs::canonicalize(&worktree)
            .expect("the worktree canonicalizes")
            .to_string_lossy(),
    );
    assert!(
        engram_one_call_reads(&root),
        "a plain test directory: {root}"
    );
    let line = format!("pushd \"{root}\" && node scripts/test-launcher.mjs full");
    let mut recorder = turn.recorder();
    recorder
        .command_started("gate", &line)
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "gate",
            &line,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    let start_basis =
        turn.record(|record| record.engram.carried_checks[0].check.start_basis.clone());
    start_basis.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
    let run = finish_run(&worktree, "failed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "failed");
    let verification = &checkpoint["verification_evidence"][0];
    let summary = verification["summary"].as_str().unwrap_or_default();
    assert!(!summary.contains("does not say whether"), "{summary}");
    assert!(summary.contains("rust-tests: failed exit=101"), "{summary}");
    assert!(
        verification["refs"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|reference| reference == "exit:101")),
        "{verification:#}"
    );
}

#[test]
fn a_run_is_found_through_a_link_to_its_root_and_a_launch_stamped_late() {
    let temp = TestTempRoot::create("termal-engram-carried-alias");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let canonical = fs::canonicalize(&root).expect("the root canonicalizes");
    let alias = temp.path().join("alias");
    link_directory(&alias, &canonical);
    let fingerprint = "f".repeat(64);
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();

    // The launcher writes the directory as the line gave it.
    write_run(
        &runs,
        "test-aliased",
        &alias,
        &at(30),
        "passed",
        &fingerprint,
    );
    let aliased = canonical
        .join(".git")
        .join("review-runs")
        .join("test-aliased");
    let (found, _, _) = engram_find_carried_run("session-1", &canonical, &at(20), &[], &none);
    assert_eq!(found, Some(aliased.clone()));

    // A run that started a little before the host stamped the launch is
    // still the launch's; one well before it is not.
    let (found, _, _) = engram_find_carried_run("session-1", &canonical, &at(32), &[], &none);
    assert_eq!(found, Some(aliased.clone()));
    let (found, ruled_out, _) =
        engram_find_carried_run("session-1", &canonical, &at(59), &[], &none);
    assert_eq!(found, None);
    assert_eq!(ruled_out, [aliased]);
}

#[test]
fn a_terminal_read_in_flight_is_stored_before_a_checkpoint_can_settle() {
    // One poll reads the run as terminal and pauses before storing that
    // read; the record changes; a checkpoint then polls and settles. The
    // checkpoint's poll waits for the first, so the first read is the one
    // kept, and the changed record is refused.
    let (turn, worktree) = named_turn("carried-in-flight");
    launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let runtime_token = turn.record(|record| {
        record
            .runtime
            .runtime_token()
            .expect("the begun turn should own the runtime")
    });
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let first = {
        let state = turn.state.clone();
        std::thread::spawn(move || state.poll_engram_carried_runs())
    };
    paused
        .recv_timeout(DEADLOCK_GUARD)
        .expect("the first poll read the run");
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["note"] = json!("rewritten after the first terminal read");
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let waiting = install_test_engram_carried_poll_waiting(&turn.state);
    let (events_tx, events) = mpsc::channel();
    let forwarder = {
        let events_tx = events_tx.clone();
        std::thread::spawn(move || {
            if waiting.recv_timeout(DEADLOCK_GUARD).is_ok() {
                let _ = events_tx.send("the poll of the checkpoint waits");
            }
        })
    };
    let checkpoint = {
        let state = turn.state.clone();
        let session_id = turn.session_id.clone();
        std::thread::spawn(move || {
            state
                .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
                .expect("the turn should complete");
            let _ = events_tx.send("the checkpoint settled");
        })
    };
    let event = events
        .recv_timeout(DEADLOCK_GUARD)
        .expect("the checkpoint waits or settles");
    release.send(()).expect("the first poll is released");
    first.join().expect("the first poll ends");
    checkpoint.join().expect("the checkpoint ends");
    forwarder.join().expect("the forwarder ends");
    assert_eq!(event, "the poll of the checkpoint waits");

    let settled = turn
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .expect("the turn is checkpointed")
        .request;
    assert_refused(
        &turn,
        &settled,
        "its terminal record changed after the host first read it as terminal",
    );
}

#[test]
fn a_run_that_ended_before_the_launch_is_not_taken() {
    let temp = TestTempRoot::create("termal-engram-carried-earlier");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let fingerprint = "f".repeat(64);
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();
    // An earlier run that started within the slack before the stamp, but had
    // ended by it.
    let earlier = write_run(
        &runs,
        "test-earlier",
        &root,
        &at(25),
        "failed",
        &fingerprint,
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(earlier.join("results.json")).unwrap()).unwrap();
    record["ended"] = json!(at(27));
    fs::write(
        earlier.join("results.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let own = write_run(&runs, "test-own", &root, &at(31), "passed", &fingerprint);

    let (found, ruled_out, _) = engram_find_carried_run("session-1", &root, &at(30), &[], &none);
    assert_eq!(found, Some(own));
    assert!(ruled_out.contains(&earlier), "{ruled_out:?}");
}

#[test]
fn a_run_a_dropped_gate_used_is_never_taken_by_a_later_launch() {
    let (turn, worktree) = named_turn("carried-consumed");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    // Still going, so the slack alone would let a launch just after take it.
    fs::write(
        run.join("results.json"),
        serde_json::to_vec(&json!({ "runId": "test-carried", "state": "running" })).unwrap(),
    )
    .unwrap();
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| {
        assert_eq!(
            record.engram.carried_checks[0].run_directory.as_ref(),
            Some(&run)
        );
        let (grant, sequence) = {
            let carried = &record.engram.carried_checks[0];
            (carried.check.grant_id.clone(), carried.check.sequence)
        };
        let (_, lines) = engram_take_settled_carried_checks(
            record,
            vec![(grant, sequence, Err("dropped for the test".to_owned()))],
            &[Some(0)],
            chrono::Utc::now(),
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(record.engram.carried_consumed_runs, [run.clone()]);
    });
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("again", DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "again",
            DETACHED_GATE,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the launch result should record");
    turn.state.poll_engram_carried_runs();

    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].run_directory.clone()),
        None
    );
}

#[test]
fn a_restart_notice_is_told_once_through_the_store() {
    let (turn, worktree) = named_turn("carried-restart-store");
    launch_gate(&turn, &worktree, false);
    // A gate outlives its turn: the session is idle when the host goes
    // down, so nothing else marks it to be written again.
    turn.finish();
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let temp = TestTempRoot::create("termal-engram-carried-restart-store");
    let path = temp.path().join("termal.sqlite");
    {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        persist_state(&path, &inner).expect("the state persists");
    }
    let loaded = load_state(&path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = loaded
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    let record = &loaded.sessions[index];
    assert!(record.engram.carried_checks.is_empty());
    assert!(
        record
            .engram
            .pending_source_root_line
            .as_deref()
            .is_some_and(|line| line.contains("the host restarted while the background full gate")),
        "{:?}",
        record.engram.pending_source_root_line
    );
    // Marked to be written again (boot recovery marks every local
    // session), now without the marker, so a second restart does not repeat
    // the notice.
    assert_ne!(record.mutation_stamp, 0);
    persist_state(&path, &loaded).expect("the loaded state persists");
    let again = load_state(&path)
        .expect("the state loads")
        .expect("a state was stored");
    let index = again
        .find_session_index(&turn.session_id)
        .expect("the session loads");
    assert_eq!(again.sessions[index].engram.pending_source_root_line, None);
}

#[test]
fn a_project_settings_reset_tells_the_holder_of_a_carried_gate() {
    let (turn, worktree) = named_turn("carried-reset");
    launch_gate(&turn, &worktree, false);
    turn.finish();
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let project_id = turn.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the session has a project")
    });
    turn.state
        .update_project_engram_settings(
            &project_id,
            EngramProjectSettings {
                acceptance_evaluation: None,
                enabled: false,
                turn_gated_control: false,
                binary_path: None,
                home: None,
                work_authority_grant: None,
                authority_store_key: None,
                deadline_ms: None,
            },
        )
        .expect("disabling Engram should succeed");

    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("earned no credit")
            && line.contains("its project's Engram settings changed before it settled"),
        "{line}"
    );
}

/// Writes a run `run_id` of the gate for `worktree`, started now, that
/// ended `state`.
fn write_named_run(worktree: &FsPath, run_id: &str, state: &str) -> PathBuf {
    let runs = review_runs(worktree);
    let canonical = fs::canonicalize(worktree).expect("the worktree canonicalizes");
    write_run(
        &runs,
        run_id,
        &canonical,
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        state,
        &"f".repeat(64),
    )
}

/// Launches the detached full gate as `key` in `worktree`, without asserting
/// what became of it.
fn launch_detached(turn: &CheckedTurn, worktree: &FsPath, key: &str) {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            key,
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the launch result should record");
}

#[test]
fn a_launch_after_one_with_no_run_matched_yet_is_refused_as_ambiguous() {
    // Two launches before a poll matched the first to its run: the run found
    // could be either's, so the second is refused, even though a passed run
    // is there, and the first is fenced by the second's launch.
    let (turn, worktree) = named_turn("carried-ambiguous");
    launch_detached(&turn, &worktree, "first");
    launch_detached(&turn, &worktree, "second");
    assert!(turn.record(|record| record.engram.carried_checks[1].ambiguous));
    write_named_run(&worktree, "test-first", "passed");
    turn.state.poll_engram_carried_runs();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[1].run_directory.clone()),
        None,
        "an ambiguous launch takes no run"
    );
    let checkpoint = turn.finish();
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(line.contains("cannot tell their runs apart"), "{line}");
}

#[test]
fn a_launch_after_one_matched_to_its_run_takes_its_own() {
    let (turn, worktree) = named_turn("carried-matched-first");
    launch_detached(&turn, &worktree, "first");
    let first = write_named_run(&worktree, "test-first", "passed");
    turn.state.poll_engram_carried_runs();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].run_directory.clone()),
        Some(first)
    );
    launch_detached(&turn, &worktree, "second");
    assert!(!turn.record(|record| record.engram.carried_checks[1].ambiguous));
    let second = write_named_run(&worktree, "test-second", "failed");
    turn.state.poll_engram_carried_runs();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[1].run_directory.clone()),
        Some(second)
    );
}

#[test]
fn a_launch_after_a_dropped_compound_launch_is_refused_as_ambiguous() {
    let (turn, worktree) = named_turn("carried-after-compound");
    launch_command(
        &turn,
        &worktree,
        "node scripts/test-launcher.mjs full --detach --notify session-9 && echo x",
        EngramCommandExit::Code(0),
    );
    launch_detached(&turn, &worktree, "again");
    assert!(turn.record(|record| record.engram.carried_checks[0].ambiguous));
}

#[test]
fn a_file_change_while_the_gate_is_being_launched_fences_it() {
    let (turn, worktree) = named_turn("carried-watched-launch");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", GATE, Some(GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    turn.state
        .note_engram_workspace_file_changes(&[WorkspaceFileChangeEvent {
            path: fs::canonicalize(&worktree)
                .unwrap()
                .join("README.md")
                .to_string_lossy()
                .into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        }]);
    recorder
        .command_completed_with_exit(
            "gate",
            GATE,
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "(while it was being launched)");
}

#[test]
fn a_launch_snapshot_that_failed_refuses_the_gate_at_once() {
    let (turn, worktree) = named_turn("carried-failed-snapshot");
    launch_gate(&turn, &worktree, false);
    let failed = Arc::new(EngramBasisCapture::default());
    failed.finish(None);
    turn.record_mut(|record| record.engram.carried_checks[0].check.start_basis = failed);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(
        &turn,
        &checkpoint,
        "the host could not take its source snapshot at the launch",
    );
}

#[test]
fn a_launch_after_one_refused_for_an_overlap_is_refused_as_ambiguous() {
    // A launch refused before it was carried may still have started a run,
    // which the next launch in the worktree must not take.
    let (turn, worktree) = named_turn("carried-after-refused");
    let mut recorder = turn.recorder();
    recorder
        .command_started("other", "git status")
        .expect("the other command should record");
    launch_detached(&turn, &worktree, "first");
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    recorder
        .command_completed_with_exit(
            "other",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other command should end");
    launch_detached(&turn, &worktree, "second");

    assert!(turn.record(|record| record.engram.carried_checks[0].ambiguous));
}

#[test]
fn a_gate_settled_after_six_hours_is_refused_whatever_its_run_says() {
    let (turn, worktree) = named_turn("carried-settled-late");
    launch_gate(&turn, &worktree, false);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let (copy, workers) = turn.record(|record| {
        (
            record.engram.carried_checks[0].clone(),
            record.engram.capture_workers.clone(),
        )
    });
    let settled =
        engram_settle_carried_check(&copy, &workers, std::time::Instant::now() + DEADLOCK_GUARD);
    assert!(
        matches!(settled, Ok(Some(_))),
        "{:?}",
        settled.as_ref().map(Option::is_some)
    );
    turn.record_mut(|record| {
        let (credited, lines) = engram_take_settled_carried_checks(
            record,
            vec![(copy.check.grant_id.clone(), copy.check.sequence, settled)],
            &[Some(0)],
            chrono::Utc::now() + chrono::Duration::hours(7),
        );
        assert!(credited.is_empty(), "a late settlement is not credited");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("not settled within six hours"),
            "{}",
            lines[0]
        );
    });
}

#[test]
fn a_run_found_already_ended_in_the_slack_is_pinned_by_that_first_read() {
    // A run that started in the slack before the stamp is read at matching;
    // when that read finds it terminal, it is the first terminal read, and
    // its digest is returned to be pinned rather than read again.
    let temp = TestTempRoot::create("termal-engram-carried-pinned");
    let root = temp.path().join("root");
    let runs = root.join(".git").join("review-runs");
    fs::create_dir_all(&runs).expect("the runs directory should be created");
    let at = |second: u32| format!("2026-09-29T00:00:{second:02}.000Z");
    let none = std::collections::BTreeSet::new();
    let run = write_run(
        &runs,
        "test-slack",
        &root,
        &at(25),
        "passed",
        &"f".repeat(64),
    );
    let mut record: Value =
        serde_json::from_slice(&fs::read(run.join("results.json")).unwrap()).unwrap();
    record["ended"] = json!(at(40));
    let bytes = serde_json::to_vec(&record).unwrap();
    fs::write(run.join("results.json"), &bytes).unwrap();

    let (found, _, first_terminal) =
        engram_find_carried_run("session-1", &root, &at(30), &[], &none);
    assert_eq!(found, Some(run.clone()));
    assert_eq!(first_terminal, Some(sha256_hex(&bytes)));

    // A run that started after the stamp is not read at matching.
    let later = write_run(
        &runs,
        "test-later",
        &root,
        &at(50),
        "passed",
        &"f".repeat(64),
    );
    let skip: std::collections::BTreeSet<PathBuf> = [run].into_iter().collect();
    let (found, _, first_terminal) =
        engram_find_carried_run("session-1", &root, &at(45), &[], &skip);
    assert_eq!(found, Some(later));
    assert_eq!(first_terminal, None);
}

#[test]
fn a_relaunch_after_the_stated_wait_is_carried_unambiguously() {
    // A refused launch arms the ambiguity window; the relaunch its line asks
    // for, two minutes on, is carried as the holder's own.
    let (turn, worktree) = named_turn("carried-remedy");
    let mut recorder = turn.recorder();
    recorder
        .command_started("other", "git status")
        .expect("the other command should record");
    launch_detached(&turn, &worktree, "first");
    recorder
        .command_completed_with_exit(
            "other",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other command should end");
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("two minutes or more after this launch"),
        "{line}"
    );
    // The stated wait has passed.
    let earlier = (chrono::Utc::now()
        - chrono::Duration::seconds(ENGRAM_CARRIED_AMBIGUITY_SECONDS + 1))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.record_mut(|record| {
        assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
        record.engram.carried_unmatched_launches[0].1 = earlier;
    });
    launch_detached(&turn, &worktree, "again");

    assert!(!turn.record(|record| record.engram.carried_checks[0].ambiguous));
}

#[test]
fn a_delegated_session_is_told_its_background_gate_is_not_carried() {
    let (turn, worktree) = named_turn("carried-delegated");
    turn.record_mut(|record| {
        record.session.parent_delegation_id = Some("delegation-parent".to_owned())
    });
    launch_detached(&turn, &worktree, "gate");

    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains("a delegated session's turns are measured in its workdir"),
        "{line}"
    );
    assert!(!line.contains("termal_name_source_root"), "{line}");
}
