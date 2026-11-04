//! Check recognition, measured execution facts and environment identity for
//! ordinary turns in a real Git repository. Automatic observations carry
//! uncredited facts; explicit native verification has a separate registration
//! and binding path.
//!
//! Owns the check and evidence tests. Does not own the turn's own
//! observation (`engram_turn_observations.rs`) or native verification delivery.

use super::super::delegation_support::install_required_review_delegation;
use super::super::run_git_test_command;
use super::*;

#[path = "engram_ordinary_opening.rs"]
mod ordinary_opening;

#[test]
fn a_test_command_is_recognised_through_one_shell_wrapper() {
    let recognised = |command: &str| {
        engram_check_command(command).unwrap_or_else(|| panic!("`{command}` is a test"))
    };
    for (command, normalized) in [
        (
            "cargo test --test source_file_size -- --nocapture",
            "cargo test --test source_file_size -- --nocapture",
        ),
        ("bash -lc 'cargo test -p termal'", "cargo test -p termal"),
        (
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command 'cargo  test'",
            "cargo test",
        ),
        ("cmd /c npm test", "npm test"),
        // The rest of a PowerShell or cmd line is taken as written, so each
        // argument stays the one the runner saw; whitespace inside quotes is
        // part of it.
        ("pwsh -Command cargo test   \"a  b\"", "cargo test \"a  b\""),
        (
            "cmd /c pytest  \"tests/a  b.py\"",
            "pytest \"tests/a  b.py\"",
        ),
        ("cargo +nightly nextest run", "cargo +nightly nextest run"),
        ("npm run test", "npm run test"),
        ("pnpm test", "pnpm test"),
        (
            "npx vitest run src/app.test.ts",
            "npx vitest run src/app.test.ts",
        ),
        ("python -m pytest -q", "python -m pytest -q"),
        ("go test ./...", "go test ./..."),
        (
            "node scripts/test-launcher.mjs focused -- cargo test",
            "node scripts/test-launcher.mjs focused -- cargo test",
        ),
        (
            "node scripts/test-launcher.mjs full",
            "node scripts/test-launcher.mjs full",
        ),
    ] {
        let check = recognised(command);
        assert_eq!(check.kind, EngramVerificationKind::Test, "{command}");
        assert_eq!(check.normalized, normalized, "{command}");
        assert!(check.simple, "{command}");
    }
    assert_eq!(recognised("cargo test").program, "cargo");
    assert_eq!(
        recognised("node scripts/test-launcher.mjs full --detach --notify session-1").program,
        "node",
        "a detached full gate is a launch the host carries"
    );
    for (command, dialect) in [
        ("cargo test", EngramShellDialect::Unknown),
        ("bash -lc 'cargo test'", EngramShellDialect::Bash),
        ("pwsh -Command cargo test", EngramShellDialect::PowerShell),
        ("cmd /c cargo test", EngramShellDialect::Cmd),
    ] {
        assert_eq!(recognised(command).dialect, dialect, "{command}");
    }
    assert_eq!(
        engram_shell_words(&recognised("pwsh -Command pytest \"a  b\" -k 'x y'").normalized),
        Some(vec![
            "pytest".to_owned(),
            "a  b".to_owned(),
            "-k".to_owned(),
            "x y".to_owned()
        ]),
        "the words the runner saw"
    );
    assert_eq!(
        recognised("\"C:\\tools\\npm.cmd\" test").program,
        "npm",
        "a Windows extension is not part of the program's name"
    );

    let build = engram_check_command("cargo build").expect("a genuine Build check");
    assert_eq!(build.kind, EngramVerificationKind::Build);
    assert_eq!(build.normalized, "cargo build");
    assert_eq!(build.directory, None);
    assert!(build.simple);

    for command in [
        "cargo check --tests",
        "git status",
        "cd crate && cargo test",
        "npm install",
        "echo cargo test",
        // Only a full gate may be detached: it is carried as a launch
        // (`engram_carried_checks.rs`).
        "node scripts/test-launcher.mjs live --detach --notify session-1",
        // A focused launcher run executes whatever follows `--`.
        "node scripts/test-launcher.mjs focused -- true",
        "node scripts/test-launcher.mjs focused",
        // These exit 0 having run no tests.
        "cargo test --no-run",
        "cargo test -- --list",
        "pytest --collect-only",
        "python -m pytest --co -q",
        // Several lines are several commands, not one test's arguments.
        "cargo test\nrm -rf target",
        "go test -list .",
        "go test -c ./pkg",
        "",
        // A PowerShell wrapper told to start elsewhere runs its test there,
        // not where the runtime says.
        "pwsh -NoProfile -WorkingDirectory C:/other-repo -Command pytest -q",
        "pwsh -wd ../other -Command cargo test",
        "pwsh -wd:../other -Command cargo test",
        "powershell /Wo ../other -Command cargo test",
        "pwsh --workingdir ../other -c cargo test",
    ] {
        assert_eq!(
            engram_check_command(command),
            None,
            "`{command}` is not a test"
        );
    }
    // Other PowerShell parameters leave the script where the runtime says.
    assert_eq!(
        recognised("pwsh -NoProfile -WindowStyle Hidden -Command pytest -q").normalized,
        "pytest -q"
    );
    assert!(engram_wrapper_sets_directory(
        "pwsh -wd ../other -Command git checkout ."
    ));
    assert!(!engram_wrapper_sets_directory(
        "pwsh -NoProfile -Command Set-Location -wd"
    ));
    assert!(!engram_wrapper_sets_directory("cargo test -wd"));

    // A pipe, a list or a redirection can hide the test's own exit status,
    // so such a run is recognised but cannot claim an outcome.
    for command in [
        "cargo test 2>&1 | tail -5",
        "cargo test || true",
        "cargo test; echo done",
        "bash -lc 'cargo test > out.txt'",
        "cargo test $(cat args)",
    ] {
        assert!(!recognised(command).simple, "{command}");
    }
}

#[test]
fn the_check_fingerprint_is_the_sha256_of_the_normalised_line() {
    let check =
        engram_check_command("bash -lc 'cargo test  --test source_file_size -- --nocapture'")
            .expect("a test");
    // Pinned: `printf '%s' 'cargo test --test source_file_size -- --nocapture' | sha256sum`.
    assert_eq!(
        engram_check_fingerprint(&check),
        "2d7cc0ce52623c3bde2cd355ea5a9f35001e657b72bf75d3fbca2a15d321b48c"
    );
}

#[test]
fn each_runtime_end_maps_to_an_honest_outcome() {
    use EngramCommandExit::{Code, NotFinished, ReportedSuccess, Unknown};

    for (item, exit) in [
        (json!({"status": "completed", "exitCode": 0}), Code(0)),
        (json!({"status": "completed", "exitCode": 101}), Code(101)),
        (json!({"status": "failed", "exitCode": 1}), Code(1)),
        (json!({"status": "completed"}), Unknown),
        (json!({"status": "declined"}), Unknown),
        (json!({"status": "inProgress"}), NotFinished),
    ] {
        assert_eq!(engram_codex_command_exit(&item), exit, "{item}");
    }
    assert_eq!(
        engram_acp_command_exit(AcpAgent::OpenCode, &json!({"rawOutput": {"exitCode": 2}})),
        Code(2)
    );
    assert_eq!(
        engram_acp_command_exit(AcpAgent::OpenCode, &json!({"rawOutput": {"stdout": "ok"}})),
        Unknown
    );

    assert_eq!(
        engram_claude_command_exit(false, false, false, ""),
        ReportedSuccess
    );
    assert_eq!(
        engram_claude_command_exit(true, false, false, "Exit code 101\nerror: test failed"),
        Code(101)
    );
    assert_eq!(
        engram_claude_command_exit(true, false, false, "Command timed out"),
        Unknown,
        "an error that shows no exit status may not have been the test failing"
    );
    assert_eq!(engram_claude_command_exit(false, true, false, ""), Unknown);
    assert_eq!(
        engram_claude_command_exit(false, false, true, ""),
        NotFinished
    );

    for (exit, simple, outcome) in [
        (Code(0), true, Some(EngramExecutionOutcome::Succeeded)),
        (
            ReportedSuccess,
            true,
            Some(EngramExecutionOutcome::Succeeded),
        ),
        (Code(1), true, Some(EngramExecutionOutcome::Failed)),
        (Unknown, true, Some(EngramExecutionOutcome::Unknown)),
        (Code(0), false, Some(EngramExecutionOutcome::Unknown)),
        (Code(1), false, Some(EngramExecutionOutcome::Unknown)),
        (NotFinished, true, None),
    ] {
        assert_eq!(
            engram_check_outcome(exit, simple),
            outcome,
            "{exit:?} simple={simple}"
        );
    }
}

#[test]
fn the_summary_carries_only_result_lines_within_engram_bounds() {
    // Raw test output can carry secrets and paths, and a summary Engram's
    // redactor refuses drops the whole report: only the runner's own result
    // lines are kept, without terminal codes.
    let check = engram_check_command("cargo test").expect("a test");
    let output = "\u{1b}[32m   Compiling termal v0.1.0 (C:\\secret\\path)\u{1b}[0m\n\
        running 3 tests\r\n\
        token=hunter2\n\
        \u{1b}[1mtest result:\u{1b}[0m ok. 3 passed; 0 failed; 0 ignored\n\
        test result: ok. 0 passed; 0 failed; 0 ignored\n";
    let lines = engram_check_result_lines("cargo", output);
    assert_eq!(
        lines,
        [
            "test result: ok. 3 passed; 0 failed; 0 ignored",
            "test result: ok. 0 passed; 0 failed; 0 ignored"
        ]
    );
    assert_eq!(
        engram_check_summary(&check, EngramCommandExit::Code(0), &lines),
        "`cargo test` exited 0\n\
         test result: ok. 3 passed; 0 failed; 0 ignored\n\
         test result: ok. 0 passed; 0 failed; 0 ignored"
    );
    assert_eq!(
        engram_check_summary(&check, EngramCommandExit::ReportedSuccess, &[]),
        "`cargo test` succeeded (the runtime reports no exit status)"
    );
    let many = vec!["test result: ok. 1 passed".repeat(10); 100];
    let summary = engram_check_summary(&check, EngramCommandExit::Code(0), &many);
    assert!(
        summary.len() <= ENGRAM_CHECK_SUMMARY_MAX_BYTES,
        "{}",
        summary.len()
    );

    assert_eq!(
        engram_check_refs(&check, EngramCommandExit::Code(0), &[]),
        ["command:cargo test", "exit:0"]
    );
    assert_eq!(
        engram_check_refs(&check, EngramCommandExit::ReportedSuccess, &[]),
        ["command:cargo test"]
    );
    let long = engram_check_command(&format!("cargo test {}", "x".repeat(1100))).expect("a test");
    assert_eq!(
        engram_check_refs(&long, EngramCommandExit::Code(0), &[]),
        ["exit:0"],
        "an oversized command reference is dropped, not truncated"
    );
}

#[test]
fn a_size_inventory_is_reported_whole_or_not_at_all() {
    // Engram's size test prints one line per guarded file so the evidence can
    // list what it covered. A partial list would misstate that, so the lines
    // go in all together, or none of them with a note saying how many.
    let check = engram_check_command(SIZE_TEST).expect("a test");
    let inventory = |count: usize| {
        (0..count)
            .map(|index| {
                format!("src/storage/family_{index:03}/tests.rs: 2400 physical lines (limit 2499)")
            })
            .collect::<Vec<_>>()
    };
    let output = |lines: &[String]| {
        format!(
            "running 1 test\n{}\n  indented.rs: 3 physical lines (limit 2499)\n\
             test result: ok. 1 passed; 0 failed\n",
            lines.join("\n")
        )
    };

    let small = inventory(3);
    let lines = engram_check_result_lines("cargo", &output(&small));
    let mut expected = small.clone();
    expected.push("test result: ok. 1 passed; 0 failed".to_owned());
    assert_eq!(
        lines, expected,
        "in output order; an indented line is not inventory"
    );
    let summary = engram_check_summary(&check, EngramCommandExit::Code(0), &lines);
    assert!(
        small.iter().all(|line| summary.contains(line.as_str())),
        "{summary}"
    );

    let large = inventory(120);
    let lines = engram_check_result_lines("cargo", &output(&large));
    assert_eq!(
        lines.len(),
        121,
        "the whole inventory is kept for the summary to judge"
    );
    let summary = engram_check_summary(&check, EngramCommandExit::Code(0), &lines);
    assert!(summary.len() <= ENGRAM_CHECK_SUMMARY_MAX_BYTES);
    assert_eq!(
        summary,
        format!(
            "`{SIZE_TEST}` exited 0\n\
             test result: ok. 1 passed; 0 failed\n\
             inventory omitted: 120 lines over the summary budget"
        )
    );
}

#[test]
fn a_test_run_passes_only_on_evidence_that_tests_passed() {
    // A run that tested nothing can exit 0: a filter that matched nothing, a
    // collect-only or list mode, an npm script that runs no tests. Success
    // needs a result line that shows tests passed, runner by runner.
    let showed = |command: &str, output: &str| {
        let check = engram_check_command(command).expect("a test");
        engram_check_showed_passing_tests(
            &check,
            &engram_check_result_lines(&check.program, output),
            &engram_launcher_output_test_stages(output),
        )
    };
    assert!(showed("cargo test", "test result: ok. 3 passed; 0 failed"));
    assert!(showed(
        "cargo test",
        "test result: ok. 3 passed; 0 failed\ntest result: ok. 0 passed; 0 failed"
    ));
    assert!(!showed("cargo test", "test result: ok. 0 passed; 0 failed"));
    assert!(!showed(
        "cargo test",
        "   Compiling termal\n    Finished test profile"
    ));
    assert!(
        showed(
            "cargo nextest run",
            "     Summary [   0.123s] 42 tests run: 42 passed, 0 skipped"
        ),
        "nextest states its totals in its own summary"
    );
    assert!(showed(
        "pytest",
        "collected 4 items\n===== 4 passed in 0.10s ====="
    ));
    assert!(!showed("pytest", "===== 3 tests collected in 0.10s ====="));
    // `-q` prints a quiet summary in place of the banner.
    assert!(showed("pytest -q", "...\n3 passed in 0.10s"));
    assert!(showed(
        "python -m pytest -q",
        "F..\n1 failed, 2 passed in 0.52s"
    ));
    assert!(!showed("pytest -q", "no tests ran in 0.01s"));
    assert!(!showed("pytest -q", "5 deselected in 0.02s"));
    assert_eq!(
        engram_check_result_lines("pytest", "...\n3 passed in 0.10s"),
        ["3 passed in 0.10s"]
    );
    assert!(!showed(
        "pytest",
        "collected 0 items\n===== no tests ran in 0.01s ====="
    ));
    assert!(showed("go test ./...", "ok  \texample.com/a\t0.01s"));
    assert!(!showed(
        "go test ./...",
        "ok  \texample.com/a\t0.01s [no tests to run]"
    ));
    assert!(!showed(
        "go test ./...",
        "?   \texample.com/a\t[no test files]"
    ));
    assert!(showed(
        "npx vitest run",
        " Test Files  1 passed (1)\n      Tests  3 passed (3)"
    ));
    assert!(!showed("npx vitest run", " Test Files  1 passed (1)"));
    assert!(showed("npm test", "Tests:       3 passed, 3 total"));
    assert!(!showed("npm test", "> app@1.0.0 test\n> echo no tests"));
    assert!(showed(
        "node scripts/test-launcher.mjs full",
        "PASS test-1 exit=0\nrust-tests: passed exit=0 log=C:\\runs\\rust-tests.log"
    ));
    assert!(
        !showed(
            "node scripts/test-launcher.mjs focused -- cargo test nothing_matches",
            "PASS test-1 exit=0\nfocused: passed exit=0 log=C:\\runs\\focused.log"
        ),
        "a focused launcher run's verdict does not say how many tests ran"
    );
    let live = "node scripts/test-launcher.mjs live --engram-binary C:/engram/engram.exe \
                --engram-sha256 abc123";
    assert!(
        showed(
            live,
            "PASS test-2 exit=0\nengram-live: passed exit=0 log=C:\\runs\\engram-live.log"
        ),
        "a live run's own test stage"
    );
    assert!(
        !showed(live, "PASS test-2 exit=0\nrust-tests: passed exit=0"),
        "each mode counts its own test stages only"
    );
    assert!(!showed(
        "node scripts/test-launcher.mjs full",
        "PASS test-3 exit=0\nengram-live: passed exit=0"
    ));
    assert_eq!(
        engram_check_result_lines(
            "node",
            "rust-tests: passed exit=0 log=C:\\secret\\rust-tests.log"
        ),
        ["rust-tests: passed"],
        "a stage line is cut before its log path"
    );

}

#[test]
fn ordinary_trace_keeps_failed_and_indeterminate_facts_without_native_credit() {
    let turn = EOCheckedTurn::start("ordinary-negative-check-facts", true);
    turn.run("failed", SIZE_TEST, EngramCommandExit::Code(101), || {});
    turn.run("unknown", SIZE_TEST, EngramCommandExit::Unknown, || {});
    turn.run("masked", &format!("{SIZE_TEST} || true"), EngramCommandExit::Code(0), || {});
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    for (key, expected) in [("failed", "failed"), ("unknown", "indeterminate"), ("masked", "indeterminate")] {
        let id = unadmitted_host_check_id(turn_ref, key);
        let check = checks.iter().find(|check| check["host_check_id"] == id).unwrap();
        assert_eq!(check["observed_result"], expected, "{key}: {check:#}");
    }
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn ordinary_trace_fanout_keeps_every_check_past_the_inline_limit() {
    let turn = EOCheckedTurn::start("ordinary-inline-check-fanout", true);
    let count = ENGRAM_OBSERVATION_MAX_CHECKS + 2;
    for sequence in 0..count {
        turn.run(&format!("check-{sequence}"), SIZE_TEST, EngramCommandExit::Code(0), || {});
    }
    let request = turn.finish();
    let inline = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(inline.len(), ENGRAM_OBSERVATION_MAX_CHECKS);
    let mut ids = inline.iter().map(|check| check["host_check_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), inline.len());
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    while ids.len() < count {
        let standalone = receive(&turn.observations.lock().unwrap(), "standalone ordinary check");
        assert_eq!(standalone["occurrence"]["kind"], "observed_check");
        assert_eq!(standalone["occurrence"]["host_turn_ref"], turn_ref);
        assert!(ids.insert(standalone["occurrence"]["check"]["host_check_id"].as_str().unwrap().to_owned()),
            "fanout emits each observed check once");
    }
    let expected = (0..count).map(|sequence|
        unadmitted_host_check_id(turn_ref, &format!("check-{sequence}"))).collect::<BTreeSet<_>>();
    assert_eq!(ids, expected);
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_check_keeps_the_toolchain_capture_taken_at_its_start() {
    let mut check = finished_check(0, engram_ready_basis_capture());
    check.toolchain = EngramToolchainCapture::settled(Some("rustc 1.80.0".to_owned()));
    let previous = TEST_ENGRAM_TOOLCHAIN_LABEL.with(|slot|
        slot.replace(Some(Some("a later toolchain".to_owned()))));
    let captured = check.toolchain.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).flatten();
    TEST_ENGRAM_TOOLCHAIN_LABEL.with(|slot| *slot.borrow_mut() = previous);
    assert_eq!(captured.as_deref(), Some("rustc 1.80.0"));
}

/// A basis capture already holding one fixed basis.
fn engram_ready_basis_capture() -> Arc<EngramBasisCapture> {
    let capture = Arc::new(EngramBasisCapture::default());
    capture.finish(Some(EngramExecutionSourceBasis {
        source_root_generation: None,
        source_root_state: None,
        workspace_id: "C:/w".to_owned(),
        source_revision: "revision".to_owned(),
    }));
    capture
}

#[test]
fn only_a_cargo_run_by_name_is_labelled_with_its_toolchain() {
    // TermAl takes a label with its own rights, outside any sandbox the check
    // ran in, so it never names a toolchain by running a program the
    // workspace could have written or picked.
    let selector = |command: &str| {
        engram_cargo_toolchain_selector(&engram_check_command(command).expect("a test"))
    };
    assert_eq!(selector("cargo test"), Some(None));
    assert_eq!(
        selector("cargo +nightly-2026-09-01 test"),
        Some(Some("nightly-2026-09-01".to_owned()))
    );
    for command in [
        // A cargo run by a path is not the cargo TermAl would find.
        "/repo/tools/cargo test",
        "C:\\repo\\tools\\cargo.exe test",
        "./cargo test",
        // A selector that could name a directory.
        "cargo +../toolchain test",
        "cargo +/opt/toolchain test",
        // Interpreters picked by shims from workspace files, or by a path.
        "python -m pytest",
        ".venv/bin/python -m pytest",
        "./go test ./...",
        "npm test",
        "node scripts/test-launcher.mjs full",
    ] {
        assert_eq!(selector(command), None, "{command}");
    }

    assert_eq!(
        engram_rustup_active_toolchain("stable-x86_64-pc-windows-msvc (default)\n"),
        Some("stable-x86_64-pc-windows-msvc".to_owned())
    );
    for output in [
        // A toolchain file that names a directory.
        "/repo/toolchain (overridden by '/repo/rust-toolchain.toml')",
        "C:\\repo\\toolchain (overridden by 'C:\\repo\\rust-toolchain.toml')",
        "",
    ] {
        assert_eq!(engram_rustup_active_toolchain(output), None, "{output}");
    }

    // An older rustup may install a toolchain it is only asked to show.
    assert!(engram_rustup_never_installs(
        "rustup 1.29.1 (d95a37b6a 2026-08-13)"
    ));
    assert!(engram_rustup_never_installs(
        "rustup 1.28.0 (a1b2c3d 2025-03-02)"
    ));
    for version in [
        "rustup 1.27.1 (54dd3d00f 2024-04-24)",
        "rustup",
        "cargo 1.80.0",
        "",
    ] {
        assert!(!engram_rustup_never_installs(version), "{version}");
    }
}

#[test]
fn a_toolchain_program_is_taken_from_the_path_only_outside_the_workspace() {
    let temp = TestTempRoot::create("termal-engram-host-program");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join(".git")).expect("worktree marker should exist");
    let inside = workspace.join("bin");
    let outside = temp.path().join("host-bin");
    let file_name = if cfg!(windows) {
        "rustup.exe"
    } else {
        "rustup"
    };
    for directory in [&inside, &outside] {
        fs::create_dir_all(directory).expect("program directory should exist");
        let program = directory.join(file_name);
        fs::write(&program, "").expect("program should write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
                .expect("program should be executable");
        }
    }
    let workspace_key = engram_worktree_root(&workspace);
    let find = |directories: &[PathBuf], name: &str| {
        let search_path = std::env::join_paths(directories).expect("a search path");
        engram_host_program(&search_path, name, &workspace_key)
    };

    assert_eq!(
        find(&[outside.clone()], "rustup"),
        Some(outside.join(file_name))
    );
    assert_eq!(
        find(&[inside.clone(), outside.clone()], "rustup"),
        None,
        "the first match decides, and the workspace's own could be the agent's"
    );
    assert_eq!(
        find(&[PathBuf::from("bin"), outside.clone()], "rustup"),
        Some(outside.join(file_name)),
        "a relative entry is skipped"
    );
    assert_eq!(find(&[outside.clone()], "cargo"), None);

    assert!(engram_path_within("c:/w/bin", "c:/w"));
    assert!(engram_path_within("c:/w", "c:/w"));
    assert!(!engram_path_within("c:/work", "c:/w"));
    assert_eq!(
        engram_path_key(FsPath::new("\\\\?\\C:\\Repo\\")),
        if cfg!(any(windows, target_os = "macos")) {
            "c:/repo"
        } else {
            "C:/Repo"
        }
    );
    // What a check is credited to keeps the case: a case-sensitive volume
    // may hold `Repo` and `repo` as two worktrees.
    assert_eq!(
        engram_exact_path_key(FsPath::new("\\\\?\\C:\\Repo\\")),
        "C:/Repo"
    );
    // A network path is never resolved; a resolved Windows path to a drive
    // is local.
    for network in [
        "\\\\server\\share",
        "//server/share",
        "\\\\?\\UNC\\server\\share",
        // Verbatim aliases of the network redirector reach a share too.
        "\\\\?\\GLOBALROOT\\Device\\Mup\\server\\share",
        "\\\\?\\Mup\\server\\share",
        "\\\\.\\UNC\\server\\share",
        // So, conservatively, does a local volume mounted without a letter:
        // only a verbatim path to a drive letter counts as local.
        "\\\\?\\Volume{00000000-0000-0000-0000-000000000000}\\repo",
    ] {
        assert!(engram_network_path(network), "{network}");
    }
    for local in ["\\\\?\\C:\\Repo", "C:\\Repo", "/repo", "repo"] {
        assert!(!engram_network_path(local), "{local}");
    }
}

// Windows resolves a path in any case to the case it stores.
#[cfg(windows)]
#[test]
fn a_directory_spelled_in_another_case_still_names_its_worktree() {
    let turn = EOCheckedTurn::start("case-spelling", true);
    let upper = turn.root.to_string_lossy().to_uppercase();
    turn.recorder()
        .command_started_in("upper", SIZE_TEST, Some(SIZE_TEST), Some(&upper))
        .expect("the start should record");
    turn.wait_for_snapshots();

    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        1
    );
}

/// Holds a session's capture workers at their limit until it is opened or
/// dropped. They wait with no time limit: one that ran out would let a worker go
/// while a slow test still counted on it, so the test's verdict would depend on
/// how long it took. Dropping it opens it, so a panicking test strands no worker.
pub(super) struct EOCaptureWorkerGate(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl EOCaptureWorkerGate {
    pub(super) fn new() -> Self {
        Self(Arc::new((Mutex::new(false), std::sync::Condvar::new())))
    }

    pub(super) fn hold_every_worker(
        &self,
        workers: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Vec<Arc<EngramCapture<()>>> {
        (0..ENGRAM_CAPTURE_WORKER_LIMIT)
            .map(|_| {
                let gate = self.0.clone();
                EngramCapture::spawn(workers, &Arc::new(CaptureLifetime::default()), move || {
                    let (open, opened) = &*gate;
                    drop(
                        opened
                            .wait_while(open.lock().expect("gate mutex poisoned"), |open| !*open)
                            .expect("gate mutex poisoned"),
                    );
                })
            })
            .collect()
    }

    pub(super) fn open(&self) {
        let (open, opened) = &*self.0;
        *open.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        opened.notify_all();
    }
}

impl Drop for EOCaptureWorkerGate {
    fn drop(&mut self) {
        self.open();
    }
}

/// An ordinary root turn whose facts are reported after provider execution.
/// The transport exposes work services and observation receipts, never grants.
pub(super) struct EOCheckedTurn {
    pub(super) state: AppState,
    pub(super) session_id: String,
    pub(super) root: PathBuf,
    pub(super) transport: Arc<ScriptedEngramControlTransport>,
    observations: Mutex<mpsc::Receiver<Value>>,
    pub(super) _runtime_rx: Arc<Mutex<mpsc::Receiver<CodexRuntimeCommand>>>,
    /// The host's stop, shared by every fixture on this host. The last of
    /// them to drop signals the host's shutdown, so no worker it started
    /// outlives the test. `None` until a constructor that owns the host, or
    /// a fixture sharing it, sets it.
    pub(super) host: Option<Arc<EOFixtureHost>>,
}

/// The shutdown owners of the host its fixtures share, and nothing else: no
/// `AppState` and no state mutex, so a test can still take that mutex back
/// exclusively. Dropped with the last fixture sharing it, it stops the host
/// exactly once. It does not wait for workers or the root's removal: a test
/// that must prove the host released its root waits on the root's cleanup.
pub(super) struct EOFixtureHost {
    federation: Arc<FederationExecutorWakeSignal>,
    observation: Arc<EngramRuntimeObservationLifetime>,
    captures: Arc<CaptureLifetime>,
    shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl EOFixtureHost {
    fn of(state: &AppState) -> Self {
        Self {
            federation: state.federation_executor_wake_signal.clone(),
            observation: state.runtime_observation_lifetime.clone(),
            captures: {
                *state.capture_lifetime.root.lock().unwrap() = state.test_temp_root.clone();
                state.capture_lifetime.clone()
            },
            shutdown: state.shutdown_signal_tx.clone(),
        }
    }
}

impl Drop for EOFixtureHost {
    /// The production stop sequence, in `AppState::trigger_shutdown_signal`'s
    /// order (sse_broadcast.rs).
    fn drop(&mut self) {
        retire_capture_observation_workers(
            &self.captures, &self.observation, &self.federation, &self.shutdown,
        );
    }
}

pub(super) const SIZE_TEST: &str = "cargo test --test source_file_size -- --nocapture";
const FIXTURE_CHECK_TURN_REF: &str = "fixture-check-turn";
const FIXTURE_TOOLCHAIN: &str =
    "rustc 1.90.0 (fixture 2025-09-14); cargo 1.90.0 (fixture 2025-07-30)";

fn use_toolchain_label(label: Option<&str>) {
    TEST_ENGRAM_TOOLCHAIN_LABEL
        .with(|fixture| *fixture.borrow_mut() = Some(label.map(str::to_owned)));
}

impl EOCheckedTurn {
    pub(super) fn start(label: &str, claimed: bool) -> Self {
        Self::start_in(label, claimed, None, false)
    }

    fn start_named(label: &str) -> Self {
        Self::start_in(label, true, None, true)
    }

    fn start_in(
        label: &str,
        claimed: bool,
        subdirectory: Option<&str>,
        named: bool,
    ) -> Self {
        let turn = Self::prepare_in(label, claimed, subdirectory, named);
        if claimed && !named { turn.acknowledge_unnamed_root(); }
        turn.begin("Run the checks.");
        turn
    }

    // Naming and durability tests need the same real state/work-service
    // owner before any provider handoff. There is no admission or grant.
    pub(super) fn idle(label: &str, claimed: bool) -> Self {
        Self::prepare_in(label, claimed, None, false)
    }

    pub(super) fn idle_real(label: &str, claimed: bool) -> Self {
        Self::prepare_with_clock(label, claimed, None, false, true)
    }

    fn prepare_in(
        label: &str,
        claimed: bool,
        subdirectory: Option<&str>,
        named: bool,
    ) -> Self {
        Self::prepare_with_clock(label, claimed, subdirectory, named, false)
    }

    fn prepare_with_clock(
        label: &str,
        claimed: bool,
        subdirectory: Option<&str>,
        named: bool,
        real_clock: bool,
    ) -> Self {
        use_toolchain_label(Some(FIXTURE_TOOLCHAIN));
        let (state, runtime_rx) =
            test_app_state_with_delegation_codex_runtime(&format!("eo-check-{label}"));
        assert_eq!(state.inner.lock().unwrap().engram_budget_clock_snapshots
            .load(std::sync::atomic::Ordering::SeqCst), 0,
            "the fixture chooses its clock before a target can capture it");
        if real_clock { state.declare_test_real_engram_budget_clock(); }
        else { state.select_test_scripted_engram_budget_clock(); }
        // Capture real contents independently of the scripted RPC budget.
        state.inner.lock().unwrap().test_engram_source_capture =
            Some(Arc::new(|place, _budget| engram_place_source_basis(place)));
        let root = state.test_temp_root.as_ref().unwrap().path()
            .join(format!("eo-check-{label}-project"));
        fs::create_dir_all(&root).unwrap();
        run_git_test_command(&root, &["init", "--quiet"]);
        run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
        run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
        fs::write(root.join("README.md"), "checked\n").unwrap();
        run_git_test_command(&root, &["add", "README.md"]);
        run_git_test_command(&root, &["commit", "--quiet", "-m", "checked"]);
        let project = create_test_project(&state, &root, "Execution Observability checks");
        let workdir = subdirectory.map_or_else(|| root.clone(), |part| root.join(part));
        fs::create_dir_all(&workdir).unwrap();
        let workdir = normalize_user_facing_path(&workdir);
        let session = create_test_project_session(&state, Agent::Codex, &project, &workdir);
        enable_test_project_engram(&state, &project, &root);
        fs::write(root.join(".engram-project"), "github.com/example/source-root\n").unwrap();
        let binding = test_control_work_binding(&format!("eo-check-{label}"), 1);
        {
            let mut inner = state.inner.lock().unwrap();
            let settings = inner.projects.iter_mut().find(|p| p.id == project).unwrap()
                .engram.as_mut().unwrap();
            settings.execution_observability = true;
            settings.authority_store_key = Some(EngramAuthorityStoreKey {
                database_path: root.join("engram.db"),
                project_id: "github.com/example/source-root".to_owned(),
            });
            let record = inner.session_mut(&session).unwrap();
            record.engram.context_nudge_pending = false;
            // Fixture state already has this ordinary work association. Its
            // transport independently checks the current claim and root;
            // native verification still requires its own registration.
            record.engram.work_binding = claimed.then(|| binding.clone());
        }
        let transport = ScriptedEngramControlTransport::new([]);
        transport.set_current_work_binding(claimed.then(|| binding.clone()));
        transport.enable_named_roots(&session, &binding);
        // Explicit positive ordinary service fixture. The transport's absent,
        // invalid and exhausted replies remain real negative cases elsewhere.
        transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply = Some(Ok(json!({
            "project_id": "github.com/example/source-root", "session_id": session,
            "work_session": true, "capabilities": ["host_work_v1"],
        })));
        transport.replace_held_claims([Ok(EngramHeldClaims {
            items: if claimed { vec![EngramHeldClaim {
                work_id: binding.work_id.clone(),
                short_ref: binding.work_id.clone(),
                claim_id: binding.claim_id.clone(),
                claim_fence: binding.claim_fence,
                focused: true,
                control_binding: Some(binding.clone()),
            }] } else { Vec::new() },
            omitted: 0,
        })]);
        state.install_control_test_transport(transport.clone());
        let mut turn = Self::from_session(state, session, root, transport,
            Arc::new(Mutex::new(runtime_rx)));
        turn.host = Some(Arc::new(EOFixtureHost::of(&turn.state)));
        if named {
            let worktree = turn.root.join(".worktrees").join("wt");
            run_git_test_command(&turn.root, &[
                "worktree", "add", "--quiet", "-b", "wt", worktree.to_str().unwrap(),
            ]);
            turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
                work: binding.work_id,
                path: Some(Some(worktree.to_string_lossy().into_owned())),
            }).expect("ordinary work services name the root without admitting a turn");
        }
        turn
    }

    pub(super) fn from_session(
        state: AppState,
        session_id: String,
        root: PathBuf,
        transport: Arc<ScriptedEngramControlTransport>,
        runtime_rx: Arc<Mutex<mpsc::Receiver<CodexRuntimeCommand>>>,
    ) -> Self {
        let (sent, received) = mpsc::channel();
        transport.named_roots.lock().unwrap().as_mut().unwrap()
            .observation_notices = Some(sent);
        Self { state, session_id, root, transport,
            observations: Mutex::new(received), _runtime_rx: runtime_rx, host: None }
    }

    // Positive check fixtures explicitly establish their opening before
    // dispatch. An idle fixture deliberately starts with unknown authority.
    pub(super) fn acknowledge_unnamed_root(&self) {
        let work = self.record(|record| record.engram.work_binding.as_ref().unwrap().work_id.clone());
        self.state.name_engram_source_root(&self.session_id, EngramSourceRootRequest {
            work, path: None,
        }).expect("explicit ordinary clear acknowledges that no root is named");
        assert_eq!(self.state.engram_root_capture(&self.session_id), EngramRootCapture::Unnamed);
    }

    pub(super) fn begin(&self, text: &str) {
        self.begin_prompt(text);
    }

    pub(super) fn begin_prompt(&self, text: &str) -> CodexPromptCommand {
        let dispatch = match self.state.dispatch_turn(&self.session_id, SendMessageRequest {
            text: text.to_owned(), expanded_text: None, attachments: Vec::new(),
            source_session_id: None, source_mailbox: None,
        }).expect("ordinary input should dispatch") {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("the idle ordinary root must dispatch"),
        };
        deliver_turn_dispatch(&self.state, dispatch).expect("ordinary provider handoff");
        match receive(&self._runtime_rx.lock().unwrap(), "ordinary provider prompt") {
            CodexRuntimeCommand::Prompt { session_id, command } => {
                assert_eq!(session_id, self.session_id, "handoff belongs to this session");
                command
            }
            _ => panic!("ordinary dispatch must deliver a provider prompt"),
        }
    }

    pub(super) fn recorder(&self) -> SessionRecorder {
        SessionRecorder::new(self.state.clone(), self.session_id.clone())
    }

    pub(super) fn set_observability(&self, enabled: bool) {
        let mut inner = self.state.inner.lock().unwrap();
        let project = inner.sessions[inner.find_session_index(&self.session_id).unwrap()]
            .session.project_id.clone().unwrap();
        inner.projects.iter_mut().find(|item| item.id == project).unwrap()
            .engram.as_mut().unwrap().execution_observability = enabled;
        self.state.commit_locked(&mut inner).unwrap();
    }

    pub(super) fn run(&self, key: &str, command: &str, exit: EngramCommandExit, before_end: impl FnOnce()) {
        let mut recorder = self.recorder();
        recorder.command_started(key, command).unwrap();
        self.wait_for_snapshots();
        before_end();
        recorder.command_completed_with_exit(key, command,
            "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success, exit).unwrap();
        self.wait_for_snapshots();
    }

    pub(super) fn wait_for_snapshots(&self) {
        let captures = self.record(|record| record.engram.active_turn_checks.iter()
            .flat_map(|check| std::iter::once(check.start_basis.clone())
                .chain(check.end.as_ref().map(|end| end.end_basis.clone())))
            .collect::<Vec<_>>());
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
        let readings = self.record(|record| record.engram.active_turn_checks.iter()
            .filter_map(|check| check.end.as_ref()?.declared.clone()).collect::<Vec<_>>());
        for reading in readings {
            reading.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    }

    pub(super) fn wait_for_other_root_notices(&self) {
        let captures = &self.state.capture_lifetime;
        let mut workers = captures.state.lock().unwrap();
        while workers.workers.iter().any(|worker| {
            worker.thread.thread().name() == Some("termal-other-root-notice")
                && !worker.done.load(std::sync::atomic::Ordering::Acquire)
        }) {
            workers = captures.changed.wait(workers).unwrap();
        }
    }

    fn change_readme(&self, content: &str) {
        fs::write(self.root.join("README.md"), content).unwrap();
    }

    pub(super) fn record<T>(&self, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.state.inner.lock().unwrap();
        read(&inner.sessions[inner.find_session_index(&self.session_id).unwrap()])
    }

    pub(super) fn record_mut<T>(&self, change: impl FnOnce(&mut SessionRecord) -> T) -> T {
        let mut inner = self.state.inner.lock().unwrap();
        change(inner.session_mut(&self.session_id).unwrap())
    }

    pub(super) fn finish(&self) -> Value {
        let turn_ref = self.record(|record| record.engram.unadmitted_active_turn.as_ref()
            .expect("EO captured an ordinary trace").host_turn_ref.clone());
        self.end();
        self.observation_for(&turn_ref)
    }

    pub(super) fn end(&self) {
        let token = self.record(|record| record.runtime.runtime_token().unwrap());
        self.state.finish_turn_ok_if_runtime_matches(&self.session_id, &token).unwrap();
    }

    pub(super) fn observation_for(&self, turn_ref: &str) -> Value {
        let notices = self.observations.lock().unwrap();
        loop {
            let request = receive(&notices, "ordinary execution observation");
            if request["occurrence"]["kind"] == "unadmitted_turn"
                && request["occurrence"]["host_turn_ref"] == turn_ref {
                assert_eq!(request["operation"], "execution_observe");
                assert!(request.get("verification_evidence").is_none());
                return request;
            }
        }
    }

    pub(super) fn revision(&self) -> String {
        content_revision(&self.root).unwrap().1
    }

    fn finished_check(&self, sequence: usize, end_basis: Arc<EngramBasisCapture>) -> EngramTurnCheck {
        let root = engram_worktree_root_path(&self.root);
        EngramTurnCheck {
            host_turn_ref: self.record(|record| record.engram.unadmitted_active_turn.as_ref()
                .expect("the ordinary trace owns the check").host_turn_ref.clone()),
            target: EngramCheckTarget { root: root.clone(), directory: root, common_dir_key: None },
            ..finished_check(sequence, end_basis)
        }
    }
}

pub(super) fn assert_ordinary_observation_has_no_native_credit(turn: &EOCheckedTurn, request: &Value) {
    assert_eq!(request["operation"], "execution_observe");
    assert_eq!(request["occurrence"]["kind"], "unadmitted_turn");
    assert!(request.get("verification_evidence").is_none());
    assert_ordinary_turn_has_no_native_requests(turn);
}

pub(super) fn assert_ordinary_turn_has_no_native_requests(turn: &EOCheckedTurn) {
    for recorded in turn.transport.requests() {
        assert!(!matches!(recorded.request["operation"].as_str(),
            Some("session_bind" | "session_status" | "turn_grant_read"
                | "turn_evaluate" | "turn_begin" | "turn_checkpoint"
                | "verification_record" | "verification_bind")),
            "automatic observation cannot register verification: {}", recorded.request);
    }
}

#[test]
fn opening_root_preparation_sends_nothing_and_preserves_uncertainty() {
    struct UnavailableRoot {
        control: Arc<ScriptedEngramControlTransport>,
        clock: EngramBudgetClock,
        calls: Mutex<usize>,
    }
    impl EngramControlTransport for UnavailableRoot {
        fn read_observation_policy(&self, _: &EngramConnectionConfig, _: &EngramAuthorityStoreKey,
            timeout: Duration, _: &FsPath) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
            *self.calls.lock().unwrap() += 1;
            self.clock.advance(timeout);
            Err(EngramTransportError::deadline("observation policy is unavailable"))
        }
        fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
            timeout: Duration) -> Result<Value, EngramTransportError> {
            let _ = (connection, request);
            *self.calls.lock().unwrap() += 1;
            self.clock.advance(timeout);
            Err(EngramTransportError::deadline("source authority is unavailable"))
        }
        fn read_work_binding(&self, connection: &EngramConnectionConfig,
            preference: EngramBindingPreference<'_>, timeout: Duration)
            -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
            let _ = (connection, preference);
            *self.calls.lock().unwrap() += 1;
            self.clock.advance(timeout);
            Err(EngramTransportError::deadline("work authority is unavailable"))
        }
        fn shutdown_session(&self, session_id: &str) {
            self.control.shutdown_session(session_id);
        }
    }
    let turn = EOCheckedTurn::idle("opening-root-local", true);
    let clock = turn.state.engram_budget_clock();
    assert_eq!(turn.state.engram_root_capture(&turn.session_id), EngramRootCapture::Unconfirmed);
    let transport = Arc::new(UnavailableRoot {
        control: turn.transport.clone(), clock: clock.clone(), calls: Mutex::new(0),
    });
    turn.state.install_test_engram_transport(transport.clone());
    let started = clock.now();
    let prompt = turn.begin_prompt("Continue even when source authority is unavailable.");
    assert!(prompt.prompt.contains("Continue even when source authority is unavailable."));
    assert_eq!(*transport.calls.lock().unwrap(), 0, "handoff performs no authority RPC");
    assert_eq!(clock.elapsed_since(started), Duration::ZERO, "handoff spends no remote budget");
    assert!(prompt.prompt.contains("source root is unconfirmed"));
    assert!(prompt.prompt.contains("evidence is withheld"));
    assert!(!prompt.prompt.contains("no worktree named"), "unknown is not acknowledged absence");
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert!(trace.opening_prepared);
        assert_eq!(trace.opening_root.as_ref().unwrap().provenance, EngramRootCapture::Unconfirmed);
    });
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    let generation = turn.record(|record| record.active_turn_generation);
    let opening = turn.record(|record| record.engram.observed_opening().cloned());
    turn.state.prepare_ordinary_opening_root_before_handoff(&turn.session_id, &token, generation);
    assert_eq!(*transport.calls.lock().unwrap(), 0, "repreparation sends nothing");
    assert_eq!(turn.record(|record| record.engram.observed_opening().cloned()), opening,
        "the same turn cannot upgrade its withheld opening");
    turn.state.trigger_shutdown_signal();
    turn.state.shutdown_persist_blocking();
}

#[test]
fn ordinary_check_facts_do_not_register_native_verification() {
    let turn = EOCheckedTurn::start("ordinary-uncredited-check", true);
    let turn_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().host_turn_ref.clone());
    turn.run("ordinary-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let request = turn.finish();
    assert_eq!(request["occurrence"]["host_turn_ref"], turn_ref);
    assert_eq!(request["occurrence"]["outcome"], "succeeded");
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "{request:#}");
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0]["finished_at"].is_string());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn disabling_observability_keeps_the_current_turn_and_its_captured_facts() {
    let turn = EOCheckedTurn::start("ordinary-observability-toggle", true);
    let (runtime, generation, turn_ref) = turn.record(|record| (
        record.runtime.runtime_token().unwrap(), record.active_turn_generation,
        record.engram.unadmitted_active_turn.as_ref().unwrap().host_turn_ref.clone(),
    ));
    turn.run("captured-before-toggle", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.set_observability(false);
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.runtime.runtime_token().as_ref(), Some(&runtime));
        assert_eq!(record.active_turn_generation, generation);
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        assert_eq!(trace.host_turn_ref, turn_ref);
        assert_eq!(trace.commands.len(), 1, "the toggle cannot erase captured facts");
    });
    let request = turn.finish();
    assert_eq!(request["occurrence"]["host_turn_ref"], turn_ref);
    assert_eq!(request["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn observability_off_does_not_hold_ordinary_work_and_on_starts_a_fresh_trace() {
    let turn = EOCheckedTurn::start("ordinary-observability-new-turn", true);
    let first = turn.finish();
    let first_ref = first["occurrence"]["host_turn_ref"].as_str().unwrap().to_owned();
    turn.set_observability(false);
    turn.begin("Ordinary input with automatic observation disabled.");
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert!(!record.session.queue_paused);
    });
    turn.end();
    turn.set_observability(true);
    turn.begin("Ordinary input with automatic observation enabled again.");
    let fresh_ref = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .expect("a new ordinary turn samples the enabled setting").host_turn_ref.clone());
    assert_ne!(fresh_ref, first_ref, "a setting change cannot revive an old trace");
    let fresh = turn.finish();
    assert_eq!(fresh["occurrence"]["host_turn_ref"], fresh_ref);
    assert_ordinary_observation_has_no_native_credit(&turn, &fresh);
}

#[test]
fn silent_check_outcome_truncated_notices_keep_complete_redacted_host_log() {
    let turn = EOCheckedTurn::start("check-outcome-log-burst", true);
    let mut log = Vec::new();
    let mut notices = Vec::new();
    turn.record_mut(|record| {
        for index in 0..20 {
            let command = engram_check_command(&format!("{SIZE_TEST} private-argument-{index}"))
                .expect("the fixture command is recognised");
            let line = engram_unrecorded_check_line(
                &command,
                "capture capacity reached; wait for earlier workers, then run the check again",
            );
            engram_publish_check_notice(
                &turn.session_id,
                &mut record.engram,
                line.clone(),
                &mut log,
            );
            notices.push(line);
        }
        let pending = record.engram.pending_source_root_line.as_ref().unwrap();
        assert!(pending.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES);
        assert!(pending.contains("earlier lines cut; the host log has them"));
        assert!(!pending.contains(&notices[0]));
    });
    let log = String::from_utf8(log).unwrap();
    for line in notices {
        assert_eq!(
            log.matches(&format!("engram> session={} {line}\n", turn.session_id))
                .count(),
            1
        );
    }
    assert!(!log.contains("private-argument"));
    assert!(!log.contains("source_file_size"));
}

#[test]
fn silent_check_outcome_background_launch_ack_is_not_a_late_completion() {
    let turn = EOCheckedTurn::start("check-outcome-launch-ack", true);
    turn.run(
        "background-check",
        SIZE_TEST,
        EngramCommandExit::NotFinished,
        || {},
    );
    let before_close = turn.record(|record| record.engram.pending_source_root_line.clone());
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("finished_at").is_none(), "a launch is no process completion");
    assert_eq!(turn.record(|record| record.engram.pending_source_root_line.clone()), before_close,
        "a launch acknowledgment cannot produce a late-completion notice");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn ordinary_check_facts_survive_missing_or_changed_diagnostic_snapshots() {
    for shape in ["missing-start", "missing-finish", "different", "pending-finish"] {
        let turn = EOCheckedTurn::start(&format!("ordinary-check-capture-{shape}"), true);
        turn.run("check", SIZE_TEST, EngramCommandExit::Code(0), || {});
        let original = turn.record(|record| record.engram.active_turn_checks[0].start_basis.clone())
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .expect("the actual start capture settled").expect("the capture has a basis");
        let capture = Arc::new(EngramBasisCapture::default());
        if shape == "different" {
            let mut changed = original.clone();
            changed.source_revision = "another-source-revision".to_owned();
            capture.finish(Some(changed));
        } else if shape != "pending-finish" {
            capture.finish(None);
        }
        turn.record_mut(|record| {
            let check = &mut record.engram.active_turn_checks[0];
            if shape == "missing-start" { check.start_basis = capture.clone(); }
            else { check.end.as_mut().unwrap().end_basis = capture.clone(); }
        });
        let request = turn.finish();
        if shape == "pending-finish" { capture.finish(Some(original)); }
        let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1, "{shape}: a diagnostic snapshot cannot erase an observed command");
        assert_eq!(checks[0]["observed_result"], "passed");
        assert!(checks[0]["started_at"].is_string());
        assert!(checks[0]["finished_at"].is_string());
        assert!(checks[0].get("source_basis").is_none(), "no unproved source credit: {shape}");
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
    }
}

#[test]
fn capture_capacity_refusals_are_explained_without_losing_ordinary_facts() {
    for capacity in ["workers", "retained"] {
        let turn = EOCheckedTurn::start(&format!("ordinary-check-capacity-{capacity}"), true);
        let workers = turn.record(|record| record.engram.capture_workers.clone());
        let gate = EOCaptureWorkerGate::new();
        let pending = if capacity == "workers" {
            gate.hold_every_worker(&workers)
        } else {
            let checks = (0..ENGRAM_TURN_CHECK_LIMIT).map(|sequence| {
                let mut check = turn.finished_check(sequence, engram_ready_basis_capture());
                check.end = None;
                check
            }).collect();
            turn.record_mut(|record| {
                record.engram.active_turn_checks = checks;
                record.engram.next_turn_check_sequence = ENGRAM_TURN_CHECK_LIMIT;
            });
            Vec::new()
        };
        turn.run("capacity-refused", SIZE_TEST, EngramCommandExit::Code(0), || {});
        let (refused, notice, trace) = turn.record(|record| (
            record.engram.active_turn_checks.iter().all(|check| check.key != "capacity-refused"),
            record.engram.pending_source_root_line.clone(),
            record.engram.unadmitted_active_turn.clone().unwrap(),
        ));
        // Release every fixture worker before asserting, including on a failing input.
        gate.open();
        for capture in pending {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
                .expect("the released capture finishes");
        }
        assert!(refused, "the diagnostic capture limit still applies: {capacity}");
        let notice = notice.expect("capture refusal is visible to the holder");
        assert!(notice.contains("capacity"), "{capacity}: {notice}");
        assert!(!notice.contains("--nocapture") && !notice.contains("source_file_size"));
        assert_eq!(trace.commands.len(), 1, "ordinary facts are independent of diagnostic capacity");
        assert_eq!(trace.commands[0].check.observed_result, EngramObservedCheckResult::Passed);
        if capacity == "retained" {
            turn.record_mut(|record| record.engram.active_turn_checks.clear());
        }
        assert_eq!(workers.load(std::sync::atomic::Ordering::SeqCst), 0);
        let first = turn.finish();
        assert_eq!(first["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
        assert_ordinary_observation_has_no_native_credit(&turn, &first);
        turn.begin("Run a check after the diagnostic backlog drains.");
        turn.run("after-capacity", SIZE_TEST, EngramCommandExit::Code(0), || {});
        assert!(turn.record(|record| record.engram.active_turn_checks.iter()
            .any(|check| check.key == "after-capacity")), "capture recovers: {capacity}");
        let recovered = turn.finish();
        assert_eq!(recovered["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
        assert_ne!(recovered["occurrence"]["host_turn_ref"], first["occurrence"]["host_turn_ref"]);
        assert_ordinary_observation_has_no_native_credit(&turn, &recovered);
    }
}

#[test]
fn a_missing_end_is_frozen_indeterminate_and_a_late_end_cannot_enter_the_successor() {
    let turn = EOCheckedTurn::start("ordinary-check-no-end", true);
    let command = engram_check_command(SIZE_TEST).unwrap();
    let fingerprint = engram_check_fingerprint(&command);
    let mut recorder = turn.recorder();
    recorder.command_started("lost-end", SIZE_TEST).unwrap();
    turn.wait_for_snapshots();
    let first = turn.finish();
    let turn_ref = first["occurrence"]["host_turn_ref"].as_str().unwrap();
    let checks = first["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["host_check_id"], unadmitted_host_check_id(turn_ref, "lost-end"));
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("finished_at").is_none());
    let closing = turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap();
    assert_eq!(closing.matches("no end was reported").count(), 1, "{closing}");
    assert!(closing.contains("outcome Unknown") && closing.contains(&fingerprint));
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    recorder.command_completed_with_exit("lost-end", SIZE_TEST,
        "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
        EngramCommandExit::Code(0)).unwrap();
    let late = turn.record(|record| record.engram.pending_source_root_line.clone()).unwrap();
    assert!(late.contains("an end (exit code 0)") && late.contains(&fingerprint), "{late}");
    assert!(!late.contains("--nocapture") && !late.contains("source_file_size"));
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    recorder.command_completed_with_exit("lost-end", SIZE_TEST,
        "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
        EngramCommandExit::Code(0)).unwrap();
    assert!(turn.record(|record| record.engram.pending_source_root_line.is_none()),
        "a repeated late end is not announced twice");
    turn.begin("The successor is ordinary work, not another outcome for the lost check.");
    let second = turn.finish();
    assert_ne!(second["occurrence"]["host_turn_ref"], first["occurrence"]["host_turn_ref"]);
    assert!(second["occurrence"]["observed_checks"].as_array().unwrap().is_empty());
    let first_sends = turn.transport.requests().into_iter().filter(|sent|
        sent.request["operation"] == "execution_observe"
            && sent.request["occurrence"]["kind"] == "unadmitted_turn"
            && sent.request["occurrence"]["host_turn_ref"] == turn_ref)
        .map(|sent| sent.request).collect::<Vec<_>>();
    assert!(!first_sends.is_empty());
    assert!(first_sends.iter().all(|request| request == &first),
        "a late end cannot rewrite or add a completed outcome to the frozen first trace");
    assert_ordinary_observation_has_no_native_credit(&turn, &first);
    assert_ordinary_observation_has_no_native_credit(&turn, &second);
}
#[test]
fn a_passing_ordinary_check_keeps_its_capture_but_emits_no_environment_or_verification_credit() {
    let turn = EOCheckedTurn::start("ordinary-passing-check-capture", true);
    turn.record_mut(|record| record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite));
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let (fingerprint, sandbox, toolchain) = turn.record(|record| {
        let check = &record.engram.active_turn_checks[0];
        (engram_check_fingerprint(&check.command), check.sandbox.clone(), check.toolchain.clone())
    });
    assert_eq!(fingerprint, "2d7cc0ce52623c3bde2cd355ea5a9f35001e657b72bf75d3fbca2a15d321b48c");
    assert_eq!(sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(toolchain.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).flatten().as_deref(),
        Some(FIXTURE_TOOLCHAIN));
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0]["started_at"].is_string() && checks[0]["finished_at"].is_string());
    assert!(checks[0].get("source_basis").is_none());
    assert!(request.get("environment_evidence").is_none());
    assert!(request.get("observations").is_none(), "no retired producer/checkpoint envelope");
    assert!(!serde_json::to_string(&request).unwrap().contains(SIZE_TEST),
        "ordinary observations do not disclose the command text");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
#[test]
fn a_change_before_a_check_is_measured_without_claiming_verification() {
    let turn = EOCheckedTurn::start("ordinary-change-before", true);
    turn.change_readme("changed before the test\n");
    let revision = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let capture = turn.record(|record| record.engram.active_turn_checks[0].start_basis.clone());
    let start = capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    assert_eq!(start.source_revision, revision);
    let request = turn.finish();
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], revision);
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none(), "coincidence is not source verification");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_later_source_change_is_not_attributed_to_the_earlier_check() {
    let turn = EOCheckedTurn::start("ordinary-change-after", true);
    let tested = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let capture = turn.record(|record| record.engram.active_turn_checks[0].end.as_ref().unwrap().end_basis.clone());
    let end = capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    assert_eq!(end.source_revision, tested);
    turn.change_readme("changed after the test\n");
    let changed = turn.revision();
    assert_ne!(changed, tested);
    let request = turn.finish();
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], changed);
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none(), "a later revision did not run this check");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn source_changes_during_a_check_keep_its_result_without_verification_credit() {
    let turn = EOCheckedTurn::start("ordinary-moved-under", true);
    let before = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {
        turn.change_readme("changed while the test ran\n");
    });
    let (start, end) = turn.record(|record| {
        let check = &record.engram.active_turn_checks[0];
        (check.start_basis.clone(), check.end.as_ref().unwrap().end_basis.clone())
    });
    let start = start.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    let end = end.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    assert_eq!(start.source_revision, before);
    assert_ne!(start.source_revision, end.source_revision);
    let request = turn.finish();
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], end.source_revision);
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
/// Runs a command beside the check the turn is running, as another command
/// of the same agent.
fn run_beside(turn: &EOCheckedTurn) {
    let mut recorder = turn.recorder();
    recorder
        .command_started("other", "git status")
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            "other",
            "git status",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
}

#[test]
fn an_overlapped_success_is_an_observed_fact_without_verification_credit() {
    let turn = EOCheckedTurn::start("beside", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {
        run_beside(&turn);
    });
    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_failed_test_another_command_ran_beside_is_still_reported() {
    let turn = EOCheckedTurn::start("beside-failed", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(1), || {
        run_beside(&turn);
    });
    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "failed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_zero_test_exit_is_only_an_ordinary_fact_without_native_credit() {
    let turn = EOCheckedTurn::start("no-passing-test", true);
    let mut recorder = turn.recorder();
    recorder
        .command_started("check-1", SIZE_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "check-1",
            SIZE_TEST,
            "running 0 tests\ntest result: ok. 0 passed; 0 failed",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
    assert!(!turn.record(|record| record.engram.active_turn_checks[0]
        .end.as_ref().unwrap().showed_passing_tests), "zero tests is not a verified test run");
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed", "only the observed exit status");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn another_sessions_command_marks_overlap_without_erasing_ordinary_check_facts() {
    // Another writable session in the same workspace that reported a command
    // while the check ran may have written under it, even though it was idle
    // at the check's start and end.
    // A session in a subdirectory shares the same worktree content.
    for subdirectory in [None, Some("ui")] {
        let turn = EOCheckedTurn::start("other-session", true);
        let project_id = turn.record(|record| {
            record
                .session
                .project_id
                .clone()
                .expect("the root belongs to a project")
        });
        let other_workdir = match subdirectory {
            Some(subdirectory) => {
                let workdir = turn.root.join(subdirectory);
                fs::create_dir_all(&workdir).expect("subdirectory should exist");
                workdir
            }
            None => turn.root.clone(),
        };
        let other =
            create_test_project_session(&turn.state, Agent::Codex, &project_id, &other_workdir);
        turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {
            SessionRecorder::new(turn.state.clone(), other.clone())
                .command_started("other-edit", "git checkout -- README.md")
                .expect("the other session's command should record");
        });
        assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
        let request = turn.finish();
        let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["observed_result"], "passed");
        assert!(checks[0].get("source_basis").is_none());
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
    }
}

#[test]
fn a_check_stays_open_to_writes_until_both_snapshots_are_taken() {
    // A test can finish before its snapshots are taken; an edit in between
    // would be read by both snapshots and certified as tested.
    let turn = EOCheckedTurn::start("open-to-writes", true);
    let owner = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().host_turn_ref.clone());
    let pending = Arc::new(EngramBasisCapture::default());
    let settled = || {
        let capture = Arc::new(EngramBasisCapture::default());
        capture.finish(None);
        capture
    };
    let check = |key: &str, end_basis: Arc<EngramBasisCapture>| EngramTurnCheck {
        declared: None,
        host_turn_ref: owner.clone(),
        key: key.to_owned(),
        sequence: 0,
        command: engram_check_command(SIZE_TEST).expect("a test"),
        target: EngramCheckTarget {
            root: PathBuf::from("C:/w"),
            directory: PathBuf::from("C:/w"),
            common_dir_key: None,
        },
        toolchain: EngramToolchainCapture::settled(None),
        started_at: "2026-09-24T00:00:00.000Z".to_owned(),
        started_order: None,
        sandbox: None,
        start_basis: settled(),
        overlapped: false,
        overlap_cause: None,
        fenced_by_outstanding: None,
        watcher_fence: None,
        watcher_provenance: None,
        watcher_cargo_candidates: Vec::new(),
        watcher_exempted: None,
        runtime_output_cut: false,
        ended_at: None,
        keyed_by_line: false,
        end: Some(EngramTurnCheckEnd {
            declared: None,
            exit: EngramCommandExit::Code(0),
            build_diagnostic: None,
            result_lines: Vec::new(),
            showed_passing_tests: true,
            focused_stage_line_missing: false,
            launcher_ran_no_stage: false,
            end_basis,
        }),
    };
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![
            check("pending", pending.clone()),
            check("settled", settled()),
        ];
    });
    turn.state.note_engram_workspace_edit(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        None,
    );
    let overlapped = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .map(|check| (check.key.clone(), check.overlapped))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        overlapped,
        [("pending".to_owned(), true), ("settled".to_owned(), false)]
    );
    pending.finish(None);
}

/// A finished unit-fixture check at `sequence` whose closing snapshot is
/// `end_basis`.
/// Also used by the source-root naming tests, a sibling module.
pub(super) fn finished_check(
    sequence: usize,
    end_basis: Arc<EngramBasisCapture>,
) -> EngramTurnCheck {
    EngramTurnCheck {
        declared: None,
        host_turn_ref: FIXTURE_CHECK_TURN_REF.to_owned(),
        key: format!("check-{sequence}"),
        sequence,
        command: engram_check_command(SIZE_TEST).expect("a test"),
        target: EngramCheckTarget {
            root: PathBuf::from("C:/w"),
            directory: PathBuf::from("C:/w"),
            common_dir_key: None,
        },
        toolchain: EngramToolchainCapture::settled(None),
        started_at: "2026-09-24T00:00:00.000Z".to_owned(),
        started_order: None,
        sandbox: None,
        start_basis: engram_ready_basis_capture(),
        overlapped: false,
        overlap_cause: None,
        fenced_by_outstanding: None,
        watcher_fence: None,
        watcher_provenance: None,
        watcher_cargo_candidates: Vec::new(),
        watcher_exempted: None,
        runtime_output_cut: false,
        ended_at: None,
        keyed_by_line: false,
        end: Some(EngramTurnCheckEnd {
            declared: None,
            exit: EngramCommandExit::Code(0),
            build_diagnostic: None,
            result_lines: Vec::new(),
            showed_passing_tests: true,
            focused_stage_line_missing: false,
            launcher_ran_no_stage: false,
            end_basis,
        }),
    }
}

/// A session of `turn`'s project working in `turn`'s worktree.
fn other_session_in_worktree(turn: &EOCheckedTurn) -> String {
    let project_id = turn.record(|record| {
        record
            .session
            .project_id
            .clone()
            .expect("the root belongs to a project")
    });
    create_test_project_session(&turn.state, Agent::Codex, &project_id, &turn.root)
}

#[test]
fn another_sessions_write_counts_while_the_snapshots_are_open_however_it_ends() {
    // The other session starts a command after the check's end hook, while
    // its closing snapshot is still being taken, and reports the command's
    // end only after the snapshot settles. The start is what counts: a later
    // report must not erase it.
    let turn = EOCheckedTurn::start("late-overlap", true);
    let other = other_session_in_worktree(&turn);
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    let mut other_recorder = SessionRecorder::new(turn.state.clone(), other);
    other_recorder
        .command_started("other-write", "git checkout -- README.md")
        .expect("the other session's start should record");
    pending.finish(None);
    other_recorder
        .command_completed_with_exit(
            "other-write",
            "git checkout -- README.md",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other session's end should record");

    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
}

#[test]
fn a_writer_in_another_worktree_leaves_an_open_check_alone() {
    // A session's worktree is resolved off the state lock as it reports a
    // command; under the lock its remembered worktree decides.
    let turn = EOCheckedTurn::start("other-worktree-writer", true);
    let elsewhere = sibling_worktree(&turn, "other-worktree-writer");
    let other = test_session_id(&turn.state, Agent::Codex);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.workdir = elsewhere.to_string_lossy().into_owned();
    }
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    SessionRecorder::new(turn.state.clone(), other.clone())
        .command_started("other-write", "git checkout -- README.md")
        .expect("the other session's start should record");
    assert!(!turn.record(|record| record.engram.active_turn_checks[0].overlapped));

    // The key is kept on the session, not in the process-wide cache, which
    // other sessions' paths may empty at any time: a mark made after the
    // cache lost it still knows where the session works.
    {
        engram_worktree_keys()
            .lock()
            .expect("Engram worktree key cache mutex poisoned")
            .clear();
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        engram_mark_checks_overlapped_by(&mut inner, index, EngramWriterAct::Presence);
    }
    assert!(
        !turn.record(|record| record.engram.active_turn_checks[0].overlapped),
        "the session's own key decides, whatever the cache holds"
    );
    // A workdir it moved to since has no key yet, and may be any worktree.
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.workdir =
            elsewhere.join("sub").to_string_lossy().into_owned();
        engram_mark_checks_overlapped_by(&mut inner, index, EngramWriterAct::Presence);
    }
    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    pending.finish(None);
}

#[test]
fn a_command_run_in_a_checks_worktree_overlaps_it_wherever_its_session_works() {
    // Another session works in a sibling worktree, but its runtime reports a
    // command running in the check's worktree: where the command runs
    // decides, for a check open as it starts and for one that starts while
    // it runs.
    let turn = EOCheckedTurn::start("cwd-override-writer", true);
    let elsewhere = sibling_worktree(&turn, "cwd-override-writer");
    let other = test_session_id(&turn.state, Agent::Codex);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.workdir = elsewhere.to_string_lossy().into_owned();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let overlapped = |key: &str| {
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .find(|check| check.key == key)
                .unwrap_or_else(|| panic!("check {key}"))
                .overlapped
        })
    };
    let mut other_recorder = SessionRecorder::new(turn.state.clone(), other);

    // A command in its own worktree leaves the checks here alone.
    other_recorder
        .command_started_in(
            "own-write",
            "git checkout -- Cargo.toml",
            Some("git checkout -- Cargo.toml"),
            None,
        )
        .expect("the other session's start should record");
    turn.run(
        "check-beside-own",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(
        !overlapped("check-beside-own"),
        "a command in another worktree"
    );

    // One its runtime says runs here overlaps the open check at once.
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    let root = turn.root.to_string_lossy().into_owned();
    other_recorder
        .command_started_in(
            "root-write",
            "git checkout -- README.md",
            Some("git checkout -- README.md"),
            Some(&root),
        )
        .expect("the other session's start should record");
    assert!(
        overlapped("check-0"),
        "a command running in the check's worktree"
    );
    pending.finish(None);

    // A check that starts while it runs overlaps it too.
    turn.run(
        "check-beside-root",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(
        overlapped("check-beside-root"),
        "a check beside a command here"
    );

    // Once it ended, it no longer counts.
    other_recorder
        .command_completed_with_exit(
            "root-write",
            "git checkout -- README.md",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the other session's end should record");
    turn.run(
        "check-after-root",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    assert!(!overlapped("check-after-root"), "a command that ended");
}

#[test]
fn a_command_writes_where_it_runs_and_where_its_own_cd_leads() {
    let turn = EOCheckedTurn::start("command-worktrees", true);
    let elsewhere = sibling_worktree(&turn, "command-worktrees");
    let workdir = elsewhere.to_string_lossy().into_owned();
    let root = turn.root.to_string_lossy().replace('\\', "/");
    let key = |path: &FsPath| Some(engram_worktree_root(path));
    let mut both = vec![key(&elsewhere), key(&turn.root)];
    both.sort();
    let in_workdir = &EngramCommandPlaces::at(vec![None]);

    assert_eq!(
        engram_command_worktrees(&workdir, in_workdir, Some("git status"), None),
        [key(&elsewhere)]
    );
    assert_eq!(
        engram_command_worktrees(
            &workdir,
            &EngramCommandPlaces::at(vec![Some(root.clone())]),
            Some("git status"),
            None
        ),
        [key(&turn.root)],
        "a reported directory"
    );
    assert_eq!(
        engram_command_worktrees(
            &workdir,
            &EngramCommandPlaces::at(vec![None, Some(root.clone())]),
            None,
            None
        ),
        both,
        "the workdir and where the shell is presumed to be"
    );
    for line in [
        format!("cd '{root}' && git checkout -- README.md"),
        format!("bash -lc \"cd '{root}' && git checkout -- README.md\""),
    ] {
        assert_eq!(
            engram_command_worktrees(&workdir, in_workdir, Some(&line), None),
            both,
            "{line}"
        );
    }
    for line in [
        "cd \"$OTHER\" && git checkout -- README.md",
        "bash -lc 'cd \"$OTHER\" && git checkout -- README.md'",
    ] {
        assert!(
            engram_command_worktrees(&workdir, in_workdir, Some(line), None).contains(&None),
            "{line}"
        );
    }
    assert_eq!(
        engram_command_worktrees(
            &workdir,
            &EngramCommandPlaces {
                anywhere: true,
                ..EngramCommandPlaces::at(vec![None])
            },
            Some("git status"),
            None
        ),
        [None, key(&elsewhere)],
        "a shell TermAl lost where it cannot read"
    );
    for line in [
        "pwsh -wd ../elsewhere -Command git checkout -- README.md",
        "bash -lc 'pwsh -WorkingDirectory ../elsewhere -Command git checkout .'",
    ] {
        assert!(
            engram_command_worktrees(&workdir, in_workdir, Some(line), None).contains(&None),
            "a PowerShell wrapper started elsewhere may write anywhere: {line}"
        );
    }
    assert_eq!(
        engram_worktree_root(FsPath::new(r"\\server\share\repo")),
        engram_path_key(FsPath::new(r"\\server\share\repo")),
        "a network path is keyed unresolved"
    );
}

#[test]
fn another_session_starting_a_turn_marks_an_open_check_of_its_own_worktree_only() {
    // Its first write may land before it reports anything, so its turn
    // start marks the check, in the worktree it works in. It has reported
    // nothing yet (a chat-only session, or one TermAl just restarted with),
    // so its worktree is resolved as the turn starts rather than taken to be
    // every one.
    for same_worktree in [true, false] {
        let turn = EOCheckedTurn::start("turn-start-overlap", true);
        let other = test_session_id(&turn.state, Agent::Codex);
        let other_workdir = if same_worktree {
            turn.root.join("ui")
        } else {
            sibling_worktree(&turn, "turn-start-elsewhere")
        };
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            inner.sessions[index].session.workdir = other_workdir.to_string_lossy().into_owned();
        }
        let pending = Arc::new(EngramBasisCapture::default());
        let check = turn.finished_check(0, pending.clone());
        turn.record_mut(|record| {
            record.engram.active_turn_checks = vec![check];
        });
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
            turn.record(|record| record.engram.active_turn_checks[0].overlapped),
            same_worktree,
            "same_worktree={same_worktree}"
        );
        let resolved = {
            let inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            engram_session_worktree(&inner.sessions[index])
        };
        assert_eq!(
            resolved,
            Some(engram_worktree_root(&other_workdir)),
            "its worktree is resolved as its turn starts"
        );
        pending.finish(None);
    }
}

#[test]
fn a_cd_through_a_link_and_back_up_is_not_followed_physically() {
    // Bash takes `..` against the path as written: after `cd other/link/..`
    // the shell is in `other`, although on disk the link's parent is the
    // session's worktree. Where the two readings disagree, TermAl does not
    // guess; it never places the shell where the file system alone says.
    let turn = EOCheckedTurn::start("logical-cd", true);
    let other = sibling_worktree(&turn, "logical-cd");
    fs::create_dir_all(turn.root.join("sub")).expect("subdirectory should exist");
    link_directory(&other.join("link"), &turn.root.join("sub"));
    let root = fs::canonicalize(&turn.root).expect("the root resolves");
    let through_link = other.join("link").join("..");
    let moved = engram_resolve_shell_move(None, &through_link.to_string_lossy());
    assert_ne!(
        moved.as_deref().map(PathBuf::from),
        Some(root.clone()),
        "the shell is not placed in the worktree the link leads into"
    );
    if cfg!(unix) {
        assert_eq!(moved, None, "the readings disagree: the shell is lost");
    }
    // Without a link, both readings agree.
    assert_eq!(
        engram_resolve_shell_move(None, &turn.root.join("sub").join("..").to_string_lossy())
            .map(PathBuf::from),
        Some(root)
    );
    assert_eq!(
        engram_lexical_path(FsPath::new("/a/b/../c/./d")),
        PathBuf::from("/a/c/d")
    );
}

#[test]
fn an_acp_tool_call_ends_its_check_with_the_exit_code_it_reported() {
    // The ACP handler hands the recorder the exit code from `rawOutput`,
    // which a check needs to claim an outcome.
    let turn = EOCheckedTurn::start("acp-exit", true);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    for update in [
        json!({
            "sessionUpdate": "tool_call", "toolCallId": "acp-check",
            "title": "Run tests", "rawInput": {"command": SIZE_TEST}
        }),
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "acp-check",
            "status": "completed", "rawInput": {"command": SIZE_TEST},
            "rawOutput": {"exitCode": 0, "stdout": "running 3 tests\ntest result: ok. 3 passed"}
        }),
    ] {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
    }
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| check.end.as_ref().map(|end| end.exit))
                .collect::<Vec<_>>()
        }),
        [Some(EngramCommandExit::Code(0))]
    );
    turn.wait_for_snapshots();
    // A tool call that says it runs in another repository is no check here.
    let elsewhere = sibling_worktree(&turn, "acp-elsewhere");
    handle_acp_session_update(
        &json!({
            "sessionUpdate": "tool_call", "toolCallId": "acp-elsewhere",
            "title": "Run tests",
            "rawInput": {"command": SIZE_TEST, "cwd": elsewhere.to_string_lossy()}
        }),
        &turn.state,
        &turn.session_id,
        &input_tx,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .expect("the update should apply");
    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        1
    );
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    let completed = checks.iter().find(|check| check["host_check_id"] ==
        unadmitted_host_check_id(turn_ref, "acp-check")).unwrap();
    assert_eq!(completed["observed_result"], "passed");
    assert!(completed["finished_at"].is_string());
    assert!(completed.get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_background_command_counts_as_running_for_the_rest_of_the_turn() {
    let turn = EOCheckedTurn::start("background-writer", true);
    let mut recorder = turn.recorder();
    recorder.command_started("watch", "npm run watch").unwrap();
    recorder.command_completed_with_exit("watch", "npm run watch", "",
        CommandStatus::Success, EngramCommandExit::NotFinished).unwrap();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped),
        "a launch result cannot prove the writer stopped");
    let request = turn.finish();
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    let check = request["occurrence"]["observed_checks"].as_array().unwrap().iter()
        .find(|check| check["host_check_id"] == unadmitted_host_check_id(turn_ref, "check-1")).unwrap();
    assert_eq!(check["observed_result"], "passed");
    assert!(check.get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_denied_command_stops_counting_as_running() {
    let turn = EOCheckedTurn::start("denied", true);
    let mut recorder = turn.recorder();
    recorder.command_started("denied", "rm -rf target").unwrap();
    recorder.command_abandoned("denied").unwrap();
    // A proved denial did not execute; it cannot leave a live check or EO fact.
    recorder.command_started("denied-test", SIZE_TEST).unwrap();
    turn.wait_for_snapshots();
    recorder.command_abandoned("denied-test").unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert!(!turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "neither denied command actually ran");
    assert_eq!(checks[0]["host_check_id"], unadmitted_host_check_id(
        request["occurrence"]["host_turn_ref"].as_str().unwrap(), "check-1"));
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn checks_that_share_a_runtime_key_keep_distinct_observation_ids() {
    // Codex falls back to the command text when an item has no id. Two fresh
    // invocations remain distinct; this is not a duplicated start/update.
    let turn = EOCheckedTurn::start("same-key", true);
    turn.run(SIZE_TEST, SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.run(SIZE_TEST, SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert_eq!(turn.record(|record| record.engram.active_turn_checks.len()), 2);
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2, "a reused fallback key cannot erase an actual invocation");
    let ids = checks.iter().map(|check| check["host_check_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 2, "each invocation has its own immutable identity");
    for check in checks {
        assert_eq!(check["observed_result"], "passed");
        assert!(check["started_at"].is_string() && check["finished_at"].is_string());
    }
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
#[test]
fn duplicate_start_description_and_end_do_not_duplicate_an_ordinary_check() {
    let turn = EOCheckedTurn::start("ordinary-duplicate-check-events", true);
    let mut recorder = turn.recorder();
    recorder.command_started("check", SIZE_TEST).unwrap();
    recorder.command_started("check", SIZE_TEST).unwrap();
    recorder.command_described("check", Some(SIZE_TEST), None).unwrap();
    turn.wait_for_snapshots();
    let initial = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone());
    assert_eq!(initial.len(), 1);
    recorder.command_completed_with_exit("check", SIZE_TEST,
        "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
        EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    let completed = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone());
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].check.host_check_id, initial[0].check.host_check_id);
    recorder.command_described("check", Some(SIZE_TEST), None).unwrap();
    recorder.command_completed_with_exit("check", SIZE_TEST,
        "test result: FAILED. 0 passed; 1 failed", CommandStatus::Error,
        EngramCommandExit::Code(1)).unwrap();
    assert_eq!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone()), completed, "duplicate terminal events cannot revise the first result");
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn abandoning_a_second_same_key_invocation_keeps_the_completed_first_fact() {
    let turn = EOCheckedTurn::start("ordinary-abandoned-second-check", true);
    turn.run(SIZE_TEST, SIZE_TEST, EngramCommandExit::Code(0), || {});
    let first = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone());
    assert_eq!(first.len(), 1);
    let mut recorder = turn.recorder();
    recorder.command_started(SIZE_TEST, SIZE_TEST).unwrap();
    let second = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone());
    assert_eq!(second.len(), 2);
    assert_ne!(second[0].check.host_check_id, second[1].check.host_check_id);
    recorder.command_abandoned(SIZE_TEST).unwrap();
    assert_eq!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.clone()), first, "denying a successor cannot erase a completed invocation");
    let request = turn.finish();
    assert_eq!(request["occurrence"]["observed_checks"].as_array().unwrap().len(), 1);
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn repeated_detached_launcher_invocations_have_distinct_unfinished_check_facts() {
    let turn = EOCheckedTurn::start("ordinary-repeated-launch-only", true);
    let command = "node scripts/test-launcher.mjs full --detach";
    let mut recorder = turn.recorder();
    for _ in 0..2 {
        recorder.command_started("launch", command).unwrap();
        turn.wait_for_snapshots();
        recorder.command_completed_with_exit("launch", command,
            "STARTED (launcher acknowledgment only)", CommandStatus::Success,
            EngramCommandExit::Code(0)).unwrap();
        turn.wait_for_snapshots();
    }
    let trace = turn.record(|record| record.engram.unadmitted_active_turn.clone().unwrap());
    assert_eq!(trace.commands.len(), 2, "two shell invocations are distinct from two completed gates");
    assert_ne!(trace.commands[0].check.host_check_id, trace.commands[1].check.host_check_id);
    for entry in &trace.commands {
        assert_eq!(entry.check.observed_result, EngramObservedCheckResult::Indeterminate);
        assert!(entry.check.finished_at.is_none(), "a launcher exit is not a test-process end");
    }
    let saved = serde_json::to_value(&trace).unwrap();
    let restored: EngramRuntimeObservedTurn = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), saved,
        "serializing the retained trace preserves occurrence identity and unfinished facts");
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    assert!(checks.iter().all(|check| check["observed_result"] == "indeterminate"
        && check.get("finished_at").is_none()));
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn late_recognition_of_a_running_command_does_not_invent_its_start_time() {
    for reveal in ["start", "description"] {
        let turn = EOCheckedTurn::start(&format!("ordinary-late-recognition-{reveal}"), true);
        let mut recorder = turn.recorder();
        recorder.command_started_in("late-check", "Run tests", None, None).unwrap();
        assert!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
            .unwrap().commands.is_empty()));
        if reveal == "start" {
            recorder.command_started("late-check", SIZE_TEST).unwrap();
        } else {
            recorder.command_described("late-check", Some(SIZE_TEST), None).unwrap();
        }
        turn.wait_for_snapshots();
        recorder.command_completed_with_exit("late-check", SIZE_TEST,
            "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
            EngramCommandExit::Code(0)).unwrap();
        turn.wait_for_snapshots();
        let request = turn.finish();
        let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert!(checks[0].get("started_at").is_none(), "recognition is not a newly observed start: {reveal}");
        assert!(checks[0]["finished_at"].is_string());
        assert_eq!(checks[0]["observed_result"], "passed");
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
    }
}

#[test]
fn ordinary_claim_association_cannot_relabel_or_replay_a_historical_checkpoint() {
    let turn = EOCheckedTurn::start("ordinary-claim-beside-old-checkpoint", true);
    let current = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let historical = test_control_work_binding("historical-other-claim", 7);
    assert_ne!(historical, current);
    let mut operation = checkpoint_result_recovery::passive_checkpoint(&turn.root, &turn.session_id);
    operation.original.work_binding = Some(json!(historical));
    operation.protection.frozen_request = serde_json::to_value(&operation.original).unwrap();
    operation.validate().unwrap();
    let frozen = serde_json::to_value(&operation).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        checkpoint_result_recovery::install_historical_checkpoint(&mut inner, &turn.session_id, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    turn.run("current-claim-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let report = turn.finish();
    assert_eq!(report["binding"], json!(current));
    assert_ne!(report["binding"], json!(historical));
    assert!(report.get("routing_token").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
    turn.record(|record| {
        assert_eq!(record.engram.work_binding.as_ref(), Some(&current));
        assert_eq!(record.engram.historical_control["engramCheckpointOperation"], frozen);
    });
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(serde_json::to_value(&inner.engram_session_obligations.checkpoint_operations[0]).unwrap(), frozen);
    assert_eq!(inner.engram_session_obligations.checkpoint_operations[0].original.work_binding,
        Some(json!(historical)), "a current claim cannot rewrite old checkpoint custody");
}

#[test]
fn a_failing_test_and_a_masked_exit_are_reported_honestly() {
    let turn = EOCheckedTurn::start("honest", true);
    turn.run("check-1", "cargo test", EngramCommandExit::Code(101), || {});
    turn.run("check-2", "cargo test 2>&1 | tail -5", EngramCommandExit::Code(0), || {});
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 2);
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    for (key, outcome) in [("check-1", "failed"), ("check-2", "indeterminate")] {
        let check = checks.iter().find(|check| check["host_check_id"] ==
            unadmitted_host_check_id(turn_ref, key)).unwrap();
        assert_eq!(check["observed_result"], outcome,
            "a pipe's status cannot prove the underlying test succeeded");
    }
    assert!(request.get("environment_evidence").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
#[test]
fn a_background_test_stays_indeterminate_and_a_non_check_adds_no_check_fact() {
    let turn = EOCheckedTurn::start("ordinary-launch-and-non-check", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::NotFinished, || {});
    assert_eq!(
        turn.record(|record| {
            record.engram.active_turn_checks[0]
                .end
                .as_ref()
                .expect("the launch ends the check")
                .end_basis
                .wait_until(std::time::Instant::now())
        }),
        Some(None),
        "a launch result takes no fake process-completion snapshot"
    );
    assert_eq!(engram_check_command("cargo check"), None);
    assert_eq!(engram_check_command("git status"), None);
    turn.run("non-check", "git status", EngramCommandExit::Code(0), || {});
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("finished_at").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn an_unclaimed_turn_keeps_no_claim_diagnostics_and_sends_no_unattributed_report() {
    let turn = EOCheckedTurn::start("unbound", false);
    turn.recorder()
        .command_started("check-1", SIZE_TEST)
        .expect("the start should record");
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    assert_eq!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.len()), 1, "ordinary process facts do not require admission");
    let (idle, idle_rx) = mpsc::channel();
    *turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() =
        Some(Arc::new(move |phase| {
            if phase == "idle" { let _ = idle.send(()); }
        }));
    turn.end();
    receive(&idle_rx, "unclaimed observation worker settles without a report");
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(!record.session.queue_paused && !record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.unadmitted_active_turn.is_none());
        assert!(record.engram.unadmitted_closing_jobs.is_empty());
        assert!(record.engram.unadmitted_reports.is_empty());
    });
    assert!(turn.transport.requests().iter().all(|sent|
        !matches!(sent.request["operation"].as_str(), Some("execution_observe"
            | "verification_record" | "verification_bind" | "session_bind" | "session_status"
            | "turn_grant_read" | "turn_evaluate" | "turn_begin" | "turn_checkpoint"))),
        "no claimed authority is invented for an ordinary unclaimed turn");
}

/// A second Git repository beside `turn`'s, with a manifest a command can
/// name.
fn sibling_worktree(turn: &EOCheckedTurn, name: &str) -> PathBuf {
    let root_name = turn
        .root
        .file_name()
        .expect("the root has a name")
        .to_string_lossy()
        .into_owned();
    let other = turn.root.with_file_name(format!("{root_name}-{name}"));
    fs::create_dir_all(&other).expect("sibling worktree should exist");
    run_git_test_command(&other, &["init", "--quiet"]);
    fs::write(other.join("Cargo.toml"), "[package]\nname = \"other\"\n")
        .expect("sibling manifest should write");
    other
}

#[test]
fn a_check_is_captured_only_in_the_worktree_it_ran_in() {
    // A test that ran in, or named, another repository must not become
    // evidence at this worktree's revision.
    let turn = EOCheckedTurn::start("which-worktree", true);
    fs::write(
        turn.root.join("Cargo.toml"),
        "[package]\nname = \"checked\"\n",
    )
    .expect("manifest should write");
    let other = sibling_worktree(&turn, "other");
    let other_name = other
        .file_name()
        .expect("the sibling has a name")
        .to_string_lossy()
        .into_owned();
    let other_directory = other.to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    for (key, command, cwd) in [
        (
            "manifest",
            format!(
                "cargo test --manifest-path \"{}\"",
                other.join("Cargo.toml").display()
            ),
            None,
        ),
        (
            "climb",
            format!("cargo test --manifest-path=../{other_name}/Cargo.toml"),
            None,
        ),
        (
            "elsewhere",
            "cargo test".to_owned(),
            Some(other_directory.as_str()),
        ),
        (
            "home",
            "cargo test --manifest-path ~/other/Cargo.toml".to_owned(),
            None,
        ),
        // A variable anywhere in an argument may lead out, whatever shell
        // expands it.
        ("variable", "pytest tests/${SUITE}".to_owned(), None),
        ("variable-bare", "pytest tests/$SUITE".to_owned(), None),
        ("variable-cmd", "pytest tests\\%SUITE%".to_owned(), None),
        ("variable-pwsh", "pytest $env:SUITE".to_owned(), None),
        // cmd expands `!SUITE!` when delayed expansion is on.
        (
            "variable-delayed",
            "cmd /v:on /c pytest !SUITE!".to_owned(),
            None,
        ),
        (
            "variable-delayed-bare",
            "pytest tests/!SUITE!".to_owned(),
            None,
        ),
        (
            "variable-flag",
            "cargo test --manifest-path=crates/${CRATE}/Cargo.toml".to_owned(),
            None,
        ),
        // Syntax a shell evaluates is not read as written: a PowerShell
        // expression, a bash brace expansion, a PowerShell array, a bash
        // backslash escape.
        (
            "expression",
            "pwsh -Command \"pytest ('../outside/test_ok.py')\"".to_owned(),
            None,
        ),
        ("braces", "pytest tests/{a,../../outside}".to_owned(), None),
        (
            "array",
            "pwsh -Command pytest tests,../../outside".to_owned(),
            None,
        ),
        ("escaped", "pytest .\\./outside".to_owned(), None),
        // A value attached to a one-letter option is the option's argument:
        // pytest reads `-c../x/pytest.ini` as the configuration there, whose
        // options may select that repository's tests.
        (
            "attached",
            format!("pytest -c../{other_name}/pytest.ini"),
            None,
        ),
        // A setting's own value is a path too: pytest collects `testpaths`.
        (
            "override-nested",
            format!("pytest --override-ini=testpaths=../{other_name}/tests"),
            None,
        ),
        (
            "override-attached",
            format!("pytest -otestpaths=../{other_name}/tests"),
            None,
        ),
        // A setting may list paths: pytest splits `testpaths` on whitespace.
        (
            "override-list",
            format!("pytest -o \"testpaths=tests ../{other_name}/tests\""),
            None,
        ),
        // A network path is never resolved, so never inside.
        (
            "network",
            "pytest \\\\server\\share\\tests".to_owned(),
            None,
        ),
        // In this worktree: from a subdirectory, and naming a path in it.
        (
            "subdirectory",
            "cargo test --manifest-path crates/core/Cargo.toml".to_owned(),
            Some("ui"),
        ),
        (
            "inside",
            format!(
                "cargo test --manifest-path \"{}\"",
                turn.root.join("Cargo.toml").display()
            ),
            None,
        ),
        // PowerShell keeps a backslash, so an unquoted Windows path is the
        // path it names.
        (
            "inside-pwsh",
            format!(
                "pwsh -Command cargo test --manifest-path {}",
                turn.root.join("Cargo.toml").display()
            ),
            None,
        ),
        // Flags with attached values that stay inside.
        (
            "attached-inside",
            "pytest -rA -ctests/pytest.ini tests".to_owned(),
            None,
        ),
    ] {
        recorder
            .command_started_in(key, &command, Some(&command), cwd)
            .expect("the start should record");
    }
    turn.wait_for_snapshots();

    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| check.key.clone())
                .collect::<Vec<_>>()
        }),
        ["subdirectory", "inside", "inside-pwsh", "attached-inside"]
    );
    let check = engram_check_command(SIZE_TEST).expect("a test");
    let target =
        engram_check_worktree(&check, &turn.root, Some("ui")).expect("a check of this worktree");
    assert!(
        target.directory.ends_with("ui"),
        "its toolchain is named in the directory it ran in"
    );
    // A session working on a network share is never resolved, with or
    // without a directory from its runtime: resolving can block for a
    // network timeout on the runtime's event reader.
    for workdir in [r"\\server\share\repo", "//server/share/repo"] {
        for cwd in [None, Some("crates"), Some(r"\\server\share\repo")] {
            assert_eq!(
                engram_check_worktree(&check, FsPath::new(workdir), cwd),
                None,
                "{workdir} {cwd:?}"
            );
        }
        assert_eq!(
            engram_resolve_shell_move(Some(workdir), "crates"),
            None,
            "a cd from {workdir}"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_git_bash_drive_path_names_its_windows_directory() {
    // Claude's shell on Windows is Git Bash, which spells `C:/x` as `/c/x`;
    // a `cd` there moves the shell rather than losing it.
    let here = std::env::current_dir().expect("a current directory");
    let text = here.to_string_lossy().replace('\\', "/");
    let (drive, rest) = text.split_once(':').expect("a drive path");
    let msys = format!("/{}{rest}", drive.to_ascii_lowercase());
    assert_eq!(
        engram_msys_drive_path(&msys),
        format!("{}:{rest}", drive.to_ascii_uppercase())
    );
    assert_eq!(
        engram_resolve_shell_move(None, &msys),
        Some(
            fs::canonicalize(&here)
                .expect("the current directory resolves")
                .to_string_lossy()
                .into_owned()
        )
    );
    assert_eq!(engram_msys_drive_path("/c"), "C:/");
    assert!(engram_is_msys_drive_path(&msys) && engram_is_msys_drive_path("/c"));
    for other in ["/cd/x", "c/x", "//server/share", "/", "C:/x", r"C:\x"] {
        assert_eq!(engram_msys_drive_path(other), other);
        assert!(!engram_is_msys_drive_path(other), "{other}");
    }
}

#[test]
fn a_session_starts_no_check_while_its_capture_workers_are_at_their_limit() {
    // Snapshots of checks already dropped may still run; however many tests
    // finish meanwhile, none adds to them.
    let turn = EOCheckedTurn::start("capture-limit", true);
    let workers = turn.record(|record| record.engram.capture_workers.clone());
    let gate = EOCaptureWorkerGate::new();
    let pending = gate.hold_every_worker(&workers);
    let running = || workers.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(running(), ENGRAM_CAPTURE_WORKER_LIMIT);

    for index in 0..ENGRAM_TURN_CHECK_LIMIT + 4 {
        turn.run(
            &format!("past-limit-{index}"),
            SIZE_TEST,
            EngramCommandExit::Code(0),
            || {},
        );
    }
    assert!(
        turn.record(|record| record.engram.active_turn_checks.is_empty()),
        "no test starts a check while the workers are at their limit"
    );
    assert_eq!(running(), ENGRAM_CAPTURE_WORKER_LIMIT, "and none adds one");

    gate.open();
    for capture in pending {
        capture
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .expect("a released worker finishes");
    }
    assert_eq!(running(), 0, "a finished worker no longer counts");
    turn.run("after-limit", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| check.key.clone())
                .collect::<Vec<_>>()
        }),
        ["after-limit"]
    );
    assert_eq!(running(), 0, "its snapshots finished too");
}

#[test]
fn a_command_line_moves_its_shell_only_where_termal_can_follow() {
    use EngramShellMove::{Lost, Stays, To};
    for (line, moved) in [
        ("cargo test", Stays),
        ("cargo test 2>&1 | tail -5", Stays),
        // A quoted script runs in another shell.
        ("bash -lc 'cd x && cargo test'", Stays),
        ("pwsh -Command \"Set-Location x; cargo test\"", Stays),
        ("cd ui && npm test", To("ui".to_owned())),
        ("cd \"a b\"", To("a b".to_owned())),
        (
            "Set-Location \"C:\\w\\ui\"; cargo test",
            To("C:\\w\\ui".to_owned()),
        ),
        // Bash reads an unquoted backslash as an escape (`C:wui`).
        ("Set-Location C:\\w\\ui; cargo test", Lost),
        ("cd lin\\ked", Lost),
        ("builtin cd ..", To("..".to_owned())),
        // Where TermAl cannot follow.
        ("cd", Lost),
        ("cd -", Lost),
        ("cd ~/other", Lost),
        ("cd $OTHER", Lost),
        ("cd !OTHER!", Lost),
        ("cd src*", Lost),
        ("popd", Lost),
        ("cargo test && cd x", Lost),
        ("cd a && cd b", Lost),
        ("(cd x && make)", Lost),
        ("cd x & cargo test", Lost),
        ("cd /d D:\\x", Lost),
        ("eval \"$MOVE\"", Lost),
        ("cd 'unbalanced", Lost),
        // A change behind a keyword, a loop or a wrapper, and any word that
        // could be one, loses the shell too.
        ("if true; then cd ../other; fi", Lost),
        ("for d in a b; do cd $d; done", Lost),
        ("find . -type d | xargs cd", Lost),
        ("echo cd", Lost),
        // Bash reads `c\d` as `cd`.
        ("c\\d ../other", Lost),
        // A network path is never resolved.
        ("cd \\\\server\\share", Lost),
        ("cd //server/share", Lost),
    ] {
        assert_eq!(engram_shell_move(line), moved, "{line}");
    }
}

#[test]
fn a_check_runs_where_the_commands_before_it_left_a_shell_that_reports_no_directory() {
    // Claude reports no directory, and its shell keeps a `cd` between calls:
    // a test runs where the calls before it left the shell.
    let turn = EOCheckedTurn::start("shell-directory", true);
    let other = sibling_worktree(&turn, "shell-directory");
    fs::create_dir_all(turn.root.join("ui")).expect("subdirectory should exist");
    let mut recorder = turn.recorder();
    // Runs `command` to the end `exit`, or has it denied with `None`.
    let mut run_to = |key: &str, command: &str, exit: Option<i64>| {
        recorder
            .command_started(key, command)
            .expect("the start should record");
        match exit {
            Some(code) => recorder
                .command_completed_with_exit(
                    key,
                    command,
                    "",
                    CommandStatus::Success,
                    EngramCommandExit::Code(code),
                )
                .expect("the end should record"),
            None => recorder
                .command_abandoned(key)
                .expect("the denial should record"),
        }
    };
    let quoted = |path: &FsPath| format!("cd \"{}\"", path.display());

    // Into another repository, then back into this one's subdirectory.
    run_to("leave", &quoted(&other), Some(0));
    run_to("elsewhere", SIZE_TEST, Some(0));
    run_to("return", &quoted(&turn.root.join("ui")), Some(0));
    run_to("in-ui", SIZE_TEST, Some(0));
    // The shell may not keep a `cd` (an ACP runtime's, or Claude's set to
    // return to its project): an argument is judged from the workdir too.
    run_to("climb", "pytest ../x", Some(0));
    // Lost, until an absolute `cd` places the shell again.
    run_to("lose", "cd $OTHER", Some(0));
    run_to("lost", SIZE_TEST, Some(0));
    run_to("relative", "cd ..", Some(0));
    run_to("still-lost", SIZE_TEST, Some(0));
    run_to("place", &quoted(&turn.root), Some(0));
    run_to("placed", SIZE_TEST, Some(0));
    // A `cd` counts only once its command ran: a denied one moves nothing,
    // and a failed one may have stopped before or after it.
    run_to("away", &quoted(&other), Some(0));
    run_to("denied-return", &quoted(&turn.root), None);
    run_to("after-denied", SIZE_TEST, Some(0));
    run_to("failed-return", &quoted(&turn.root), Some(1));
    run_to("after-failed", SIZE_TEST, Some(0));
    run_to("back", &quoted(&turn.root), Some(0));
    run_to("after-back", SIZE_TEST, Some(0));
    turn.wait_for_snapshots();
    let checks = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .map(|check| (check.key.clone(), check.target.directory.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        checks
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["in-ui", "placed", "after-back"]
    );
    assert!(checks[0].1.ends_with("ui"), "{checks:?}");

    // A new runtime starts its shell afresh in the workdir, and a runtime
    // that reports its directory needs no presumption.
    run_to("lose-again", "cd $OTHER", Some(0));
    run_to("lost-again", SIZE_TEST, Some(0));
    turn.record_mut(|record| {
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: Some(RuntimeToken::Claude("an-earlier-runtime".to_owned())),
            directory: None,
            pending: None,
            lost_among: Vec::new(),
            lost_unbounded: true,
        });
    });
    run_to("new-runtime", SIZE_TEST, Some(0));
    turn.record_mut(|record| {
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: record.runtime.runtime_token(),
            directory: None,
            pending: None,
            lost_among: Vec::new(),
            lost_unbounded: true,
        });
    });
    turn.recorder()
        .command_started_in("reported", SIZE_TEST, Some(SIZE_TEST), Some("."))
        .expect("the start should record");
    turn.wait_for_snapshots();
    assert_eq!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| check.key.clone())
                .collect::<Vec<_>>()
        }),
        ["in-ui", "placed", "after-back", "new-runtime", "reported"]
    );
}

#[test]
fn late_file_tracking_does_not_invent_a_source_change_after_an_unchanged_check() {
    let turn = EOCheckedTurn::start("tracking-after-check", true);
    let (idle, idle_rx) = mpsc::channel();
    *turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() =
        Some(Arc::new(move |phase| {
            if phase == "idle" { let _ = idle.send(()); }
        }));
    let first = turn.finish();
    receive(&idle_rx, "initial ordinary observation and measured sighting settle");
    // Ordinary observation writes no baseline of its own. Install a measured
    // passive baseline of the actual content, as
    // `install_passive_measured_baseline` (engram_turn_observations.rs) does.
    let measured = engram_place_source_basis(
        &EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned())).unwrap();
    let baseline = EngramRuntimeCheckpointBasis {
        binding: turn.record(|record| record.engram.work_binding.clone().unwrap()),
        endpoint: EngramMeasuredBaseline { workspace_id: measured.workspace_id,
            source_revision: measured.source_revision, observed_at: unadmitted_observation_timestamp() },
        source_root_generation: measured.source_root_generation,
        source_root_state: measured.source_root_state,
    };
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.session_mut(&turn.session_id).unwrap().engram.runtime_checkpoint = Some(baseline.clone());
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()]
        .engram.runtime_checkpoint.as_ref(), Some(&baseline));
    turn.record(|record| assert_eq!(record.engram.runtime_checkpoint.as_ref(), Some(&baseline)));
    turn.begin("Check the same source after the first measured checkpoint.");
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.record_mut(|record| {
        record.active_turn_file_changes.insert(
            "target/debug/out.log".to_owned(),
            WorkspaceFileChangeKind::Modified,
        );
    });
    let request = turn.finish();
    assert_ne!(request["occurrence"]["host_turn_ref"], first["occurrence"]["host_turn_ref"]);
    assert!(request["occurrence"]["source_change"].is_null(),
        "a late tracker entry cannot turn unchanged measured content into a mutation");
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

/// Links `link` to the directory `target`: a symbolic link on Unix, a
/// junction on Windows, which needs no privilege.
fn link_directory(link: &FsPath, target: &FsPath) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("the symbolic link should be created");
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink should run");
        assert!(status.success(), "the junction should be created");
    }
}

#[test]
fn a_test_naming_a_link_out_of_the_worktree_starts_no_local_diagnostic_capture() {
    // A relative argument can lead elsewhere through a link: the snapshots
    // see only the link, not what the test ran.
    let turn = EOCheckedTurn::start("linked-worktree", true);
    let other = sibling_worktree(&turn, "linked");
    fs::write(other.join("test_widget.py"), "def test_ok():\n    pass\n")
        .expect("the linked test file should write");
    link_directory(&turn.root.join("linked"), &other);
    let mut recorder = turn.recorder();
    for (key, command) in [
        ("manifest", "cargo test --manifest-path linked/Cargo.toml"),
        // A selector or a glob does not exist as a path, but it leads
        // through the link all the same.
        ("selector", "pytest linked/test_widget.py::test_ok"),
        ("glob", "pytest linked/test_*.py"),
        // Bash passes `lin\ked` on as `linked`.
        ("escaped", "pytest lin\\ked/test_widget.py"),
    ] {
        recorder
            .command_started(key, command)
            .expect("the start should record");
    }
    turn.wait_for_snapshots();

    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
}

// Windows needs a privilege to link a single file, so this case runs where
// file links are unprivileged; the directory cases above run everywhere.
#[cfg(unix)]
#[test]
fn a_node_selector_on_a_linked_test_file_starts_no_local_diagnostic_capture() {
    // The selector names a file that is itself a link out of the worktree.
    let turn = EOCheckedTurn::start("linked-file", true);
    let other = sibling_worktree(&turn, "linked-file");
    fs::write(other.join("test_real.py"), "def test_ok():\n    pass\n")
        .expect("the linked test file should write");
    std::os::unix::fs::symlink(other.join("test_real.py"), turn.root.join("test_alias.py"))
        .expect("the file link should be created");
    turn.recorder()
        .command_started("selector", "pytest test_alias.py::test_ok")
        .expect("the start should record");
    turn.wait_for_snapshots();

    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
}

#[test]
fn repeated_acp_starts_keep_a_check_only_while_it_names_the_same_test_in_the_same_place() {
    // ACP starts a tool call more than once (the call, then pending or in
    // progress updates), each saying only what changed.
    let turn = EOCheckedTurn::start("acp-repeated-starts", true);
    let elsewhere = sibling_worktree(&turn, "acp-repeated")
        .to_string_lossy()
        .into_owned();
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let mut update = |update: Value| {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
        turn.wait_for_snapshots();
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| check.key.clone())
                .collect::<Vec<_>>()
        })
    };

    // A title-only call becomes a check once an update names the test, and a
    // later title-only update leaves it as it is.
    assert!(
        update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "named-late", "title": "Run tests"
        }))
        .is_empty()
    );
    assert_eq!(
        update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "named-late",
            "status": "in_progress", "rawInput": {"command": SIZE_TEST}
        })),
        ["named-late"]
    );
    assert_eq!(
        update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "named-late",
            "status": "in_progress", "title": "Run tests"
        })),
        ["named-late"]
    );

    // An update that moves the command to another repository drops its
    // check, whether it repeats the command or gives the directory alone.
    for (key, moved) in [
        (
            "moved-with-command",
            json!({"command": SIZE_TEST, "cwd": elsewhere.clone()}),
        ),
        ("moved-alone", json!({"cwd": elsewhere.clone()})),
    ] {
        let checks = update(json!({
            "sessionUpdate": "tool_call", "toolCallId": key,
            "title": "Run tests", "rawInput": {"command": SIZE_TEST}
        }));
        assert!(checks.iter().any(|check| check == key), "{key}");
        let checks = update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": key,
            "status": "pending", "title": "Run tests", "rawInput": moved
        }));
        assert!(!checks.iter().any(|check| check == key), "{key}");
    }

    // A command line an update gives names what runs now, even one that is
    // not a test: the check's test no longer says what ran.
    assert!(
        update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "replaced",
            "rawInput": {"command": SIZE_TEST}
        }))
        .iter()
        .any(|check| check == "replaced")
    );
    assert!(
        !update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "replaced",
            "status": "in_progress",
            "rawInput": {"command": format!("cd ../other && {SIZE_TEST}")}
        }))
        .iter()
        .any(|check| check == "replaced")
    );
}

#[test]
fn an_acp_update_without_a_start_still_moves_a_check_out_of_its_worktree() {
    // What an ACP call runs, or where, can arrive in an update with no status
    // or with the one that ends the call; the check must answer to it.
    let turn = EOCheckedTurn::start("acp-described", true);
    let elsewhere = sibling_worktree(&turn, "acp-described")
        .to_string_lossy()
        .into_owned();
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let mut update = |update: Value| {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
        turn.wait_for_snapshots();
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.key.clone(), check.end.is_some()))
                .collect::<Vec<_>>()
        })
    };
    let started = |key: &str| {
        json!({
            "sessionUpdate": "tool_call", "toolCallId": key,
            "title": "Run tests", "rawInput": {"command": SIZE_TEST}
        })
    };
    let has = |checks: &[(String, bool)], key: &str| checks.iter().any(|(check, _)| check == key);

    // An update without a status that repeats the place, or says nothing of
    // it, keeps the check; one naming another place drops it.
    assert!(has(&update(started("statusless")), "statusless"));
    for kept in [
        json!({"cwd": "."}),
        json!({"command": SIZE_TEST}),
        json!({}),
    ] {
        let checks = update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "statusless",
            "rawInput": kept
        }));
        assert!(has(&checks, "statusless"), "{checks:?}");
    }
    let checks = update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "statusless",
        "rawInput": {"cwd": elsewhere.clone()}
    }));
    assert!(!has(&checks, "statusless"));

    // The end names the place: elsewhere, the check is dropped before it
    // ends; the same place, it ends as usual.
    let ended = |key: &str, cwd: &str| {
        json!({
            "sessionUpdate": "tool_call_update", "toolCallId": key,
            "status": "completed", "rawInput": {"command": SIZE_TEST, "cwd": cwd},
            "rawOutput": {"exitCode": 0, "stdout": "test result: ok. 3 passed"}
        })
    };
    assert!(has(&update(started("ended-elsewhere")), "ended-elsewhere"));
    assert!(!has(
        &update(ended("ended-elsewhere", &elsewhere)),
        "ended-elsewhere"
    ));
    assert!(has(&update(started("ended-here")), "ended-here"));
    assert!(
        update(ended("ended-here", "."))
            .iter()
            .any(|(check, finished)| check == "ended-here" && *finished)
    );

    // A description of a call that never started starts nothing.
    assert!(!has(
        &update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "never-started",
            "rawInput": {"command": SIZE_TEST}
        })),
        "never-started"
    ));
}

#[test]
fn an_acp_check_a_later_start_invalidates_stays_withheld_until_its_call_ends() {
    // A check keeps its record from its first snapshot: a later start that
    // moves it, even within the worktree, cannot begin a new one that would
    // miss what the old one saw.
    let turn = EOCheckedTurn::start("acp-withheld", true);
    fs::create_dir_all(turn.root.join("ui")).expect("subdirectory should exist");
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let mut update = |update: Value| {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
        turn.wait_for_snapshots();
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.key.clone(), check.overlapped))
                .collect::<Vec<_>>()
        })
    };

    assert_eq!(
        update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "corrected",
            "rawInput": {"command": SIZE_TEST}
        })),
        [("corrected".to_owned(), false)]
    );
    // An edit while the test runs makes it unknown.
    turn.state.note_engram_workspace_edit(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        None,
    );
    // A correction to another place in the worktree drops the check, and
    // later starts of the same call start none.
    for moved in [
        json!({"command": SIZE_TEST, "cwd": "ui"}),
        json!({"command": SIZE_TEST, "cwd": "ui"}),
        json!({"command": SIZE_TEST}),
    ] {
        assert!(
            update(json!({
                "sessionUpdate": "tool_call_update", "toolCallId": "corrected",
                "status": "in_progress", "rawInput": moved
            }))
            .is_empty()
        );
    }
    update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "corrected",
        "status": "completed", "rawOutput": {"exitCode": 0}
    }));
    // Once the call has ended, a new call with the key checks again.
    assert_eq!(
        update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "corrected",
            "rawInput": {"command": SIZE_TEST}
        })),
        [("corrected".to_owned(), false)]
    );

    // A test a call names only after it started began before its first
    // snapshot, so its outcome is unknown.
    update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "named-late", "title": "Run tests"
    }));
    let checks = update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "named-late",
        "status": "in_progress", "rawInput": {"command": SIZE_TEST}
    }));
    assert!(
        checks.contains(&("named-late".to_owned(), true)),
        "{checks:?}"
    );
}

#[test]
fn an_acp_call_moves_its_shell_as_its_command_line_says() {
    // An ACP runtime that reports no directory may keep a `cd` in its
    // shell, whichever update of a call names it.
    let turn = EOCheckedTurn::start("acp-shell", true);
    let other = sibling_worktree(&turn, "acp-shell");
    fs::create_dir_all(turn.root.join("ui")).expect("subdirectory should exist");
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut turn_state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let mut update = |update: Value| {
        handle_acp_session_update(
            &update,
            &turn.state,
            &turn.session_id,
            &input_tx,
            &mut turn_state,
            &mut recorder,
            AcpAgent::Cursor,
        )
        .expect("the update should apply");
        turn.wait_for_snapshots();
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .map(|check| (check.key.clone(), check.target.directory.clone()))
                .collect::<Vec<_>>()
        })
    };

    // Each report of one relative `cd` is the same move, from where the
    // shell was before it.
    for status in ["pending", "in_progress", "completed"] {
        update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "move-ui",
            "status": status, "rawInput": {"command": "cd ui"},
            "rawOutput": {"exitCode": 0}
        }));
    }
    let checks = update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "in-ui",
        "rawInput": {"command": SIZE_TEST}
    }));
    assert!(
        checks
            .iter()
            .any(|(key, directory)| key == "in-ui" && directory.ends_with("ui")),
        "{checks:?}"
    );

    // A `cd` a call names only after it started moves the shell all the
    // same.
    update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "late-move", "title": "Change directory"
    }));
    update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "late-move",
        "rawInput": {"command": format!("cd \"{}\"", other.display())}
    }));
    update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "late-move",
        "status": "completed", "rawOutput": {"exitCode": 0}
    }));
    let checks = update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "after-late-move",
        "rawInput": {"command": SIZE_TEST}
    }));
    assert!(
        !checks.iter().any(|(key, _)| key == "after-late-move"),
        "{checks:?}"
    );
}

#[test]
fn a_missing_toolchain_label_cannot_be_invented_for_an_ordinary_check() {
    let turn = EOCheckedTurn::start("no-toolchain", true);
    use_toolchain_label(None);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let capture = turn.record(|record| record.engram.active_turn_checks[0].toolchain.clone());
    assert_eq!(capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD), Some(None));
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(request.get("environment_evidence").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_workdir_spelled_through_a_link_still_captures_its_worktree() {
    // A workdir kept in a user-facing spelling (`/var` for `/private/var`, a
    // Windows short name) names the same worktree as its resolved root.
    let turn = EOCheckedTurn::start("aliased-workdir", true);
    let root_name = turn
        .root
        .file_name()
        .expect("the root has a name")
        .to_string_lossy()
        .into_owned();
    let alias = turn.root.with_file_name(format!("{root_name}-alias"));
    link_directory(&alias, &turn.root);
    turn.record_mut(|record| record.session.workdir = alias.to_string_lossy().into_owned());
    turn.recorder()
        .command_started("aliased", SIZE_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();

    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        1
    );
}

/// Creates an empty program file `name` in `directory`, where
/// `engram_host_program` finds it; nothing runs it.
fn fixture_program(directory: &FsPath, name: &str) -> PathBuf {
    fs::create_dir_all(directory).expect("program directory should exist");
    let program = directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    });
    fs::write(&program, "").expect("program should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("program should be executable");
    }
    program
}

#[cfg(windows)]
#[test]
fn a_shim_ahead_of_cargo_exe_on_path_leaves_the_toolchain_unnamed() {
    // A shell tries each PATH directory's PATHEXT extensions in order, so a
    // cargo.cmd shim ahead of cargo.exe is what runs: TermAl names only a
    // plain .exe, and nothing when a shim comes first.
    let base = test_temp_dir().join(format!("termal-pathext-{}", Uuid::new_v4()));
    let shims = base.join("shims");
    let tools = base.join("tools");
    fs::create_dir_all(&shims).expect("shim directory should exist");
    fs::create_dir_all(&tools).expect("tool directory should exist");
    fs::write(shims.join("cargo.cmd"), "@echo off\r\n").expect("shim should write");
    fs::write(tools.join("cargo.exe"), "").expect("tool should write");
    let path = |directories: &[&PathBuf]| {
        std::env::join_paths(directories.iter().copied()).expect("PATH should join")
    };
    let workspace = "c:/unrelated-workspace";

    assert_eq!(
        engram_host_program(&path(&[&shims, &tools]), "cargo", workspace),
        None,
        "the shim comes first"
    );
    assert_eq!(
        engram_host_program(&path(&[&tools, &shims]), "cargo", workspace),
        Some(tools.join("cargo.exe")),
        "the plain executable comes first"
    );
    fs::remove_dir_all(base).expect("temporary directories should be removed");
}

#[test]
fn backslashes_inside_double_quotes_are_read_as_each_shell_passes_them() {
    // PowerShell and cmd keep backslashes; a Windows program folds a run of
    // them only where it meets a quote. Bash folds `\\` to `\`.
    assert_eq!(
        engram_shell_words(r#"pytest "\\server\share\tests""#),
        Some(vec![
            "pytest".to_owned(),
            r"\\server\share\tests".to_owned()
        ]),
        "a quoted UNC path keeps its prefix"
    );
    assert_eq!(
        engram_shell_words(r#"x "a\\\"b" "c\\""#),
        Some(vec!["x".to_owned(), r#"a\"b"#.to_owned(), r"c\".to_owned()]),
        "2n+1 before a quote are n and a quote; 2n before the closing quote are n"
    );
    assert_eq!(
        engram_shell_words_as(r#"pytest "\\server\share""#, true),
        Some(vec!["pytest".to_owned(), r"\server\share".to_owned()]),
        "bash folds an escaped backslash"
    );
}

#[cfg(windows)]
#[test]
fn a_quoted_unc_path_is_a_network_path_under_powershell_and_cmd() {
    // Read as bash would, `"\\Users\…\project\tests"` loses its UNC prefix
    // and names the worktree's own directory on the current drive; PowerShell
    // and cmd pass the UNC path on, which is never credited to the worktree.
    let turn = EOCheckedTurn::start("quoted-unc", true);
    let root = turn.root.to_string_lossy().into_owned();
    let (_, beneath_drive) = root.split_once(':').expect("a drive path");
    let unc = format!(r"\{beneath_drive}\tests");
    assert!(unc.starts_with(r"\\"), "{unc}");
    for line in [
        format!("pwsh -Command pytest \"{unc}\""),
        format!("cmd /c pytest \"{unc}\""),
        format!("pytest \"{unc}\""),
    ] {
        let check = engram_check_command(&line).expect("a test");
        assert_eq!(
            engram_check_worktree(&check, &turn.root, None),
            None,
            "{line}"
        );
    }
}

#[test]
fn a_checks_toolchain_is_named_in_the_directory_it_ran_in() {
    // Rustup names the toolchain the check's directory selects (toolchain
    // overrides are per directory) and where its tools are; every other
    // version command runs beside the program it runs.
    let temp = TestTempRoot::create("termal-engram-toolchain-label");
    let workspace = temp.path().join("workspace");
    let checked = workspace.join("crates").join("core");
    fs::create_dir_all(workspace.join(".git")).expect("worktree marker should exist");
    fs::create_dir_all(&checked).expect("check directory should exist");
    let host = temp.path().join("host");
    let rustup = fixture_program(&host.join("rustup-bin"), "rustup");
    // rustup's proxies lie beside it.
    fixture_program(&host.join("rustup-bin"), "cargo");
    fixture_program(&host.join("rustup-bin"), "rustc");
    let toolchain = host.join("toolchains").join("stable");
    let search_path = std::env::join_paths([host.join("rustup-bin")]).expect("a search path");
    let calls = std::cell::RefCell::new(Vec::new());
    let label_on = |search_path: &std::ffi::OsStr,
                    selector: Option<&str>,
                    rustup_version: &str,
                    tools: &FsPath| {
        calls.borrow_mut().clear();
        let probe = |program: &FsPath, args: &[&str], directory: &FsPath| {
            calls.borrow_mut().push((
                program.to_path_buf(),
                args.join(" "),
                directory.to_path_buf(),
            ));
            match args {
                ["--version"] => Some(rustup_version.to_owned()),
                ["show", "active-toolchain"] => {
                    Some("stable-x86_64-unknown-linux-gnu (default)".to_owned())
                }
                ["which", "--toolchain", _, tool] => {
                    Some(tools.join(tool).to_string_lossy().into_owned())
                }
                ["-V"] => Some(format!(
                    "{} 1.90.0 (fixture)",
                    program.file_name()?.to_string_lossy()
                )),
                _ => None,
            }
        };
        engram_cargo_toolchain_label(search_path, selector, &checked, &probe)
    };
    let label = |selector: Option<&str>, rustup_version: &str, tools: &FsPath| {
        label_on(&search_path, selector, rustup_version, tools)
    };

    assert_eq!(
        label(None, "rustup 1.29.1 (fixture)", &toolchain).as_deref(),
        Some("rustc 1.90.0 (fixture); cargo 1.90.0 (fixture)")
    );
    let calls_made = calls.borrow().clone();
    let rustup_directory = rustup.parent().expect("rustup has a directory");
    assert_eq!(
        calls_made
            .iter()
            .map(|(program, args, directory)| (
                program
                    .file_name()
                    .expect("a program")
                    .to_string_lossy()
                    .into_owned(),
                args.clone(),
                directory.clone()
            ))
            .collect::<Vec<_>>(),
        [
            (
                rustup
                    .file_name()
                    .expect("rustup")
                    .to_string_lossy()
                    .into_owned(),
                "--version".to_owned(),
                rustup_directory.to_path_buf()
            ),
            (
                rustup
                    .file_name()
                    .expect("rustup")
                    .to_string_lossy()
                    .into_owned(),
                "show active-toolchain".to_owned(),
                checked.clone()
            ),
            (
                rustup
                    .file_name()
                    .expect("rustup")
                    .to_string_lossy()
                    .into_owned(),
                "which --toolchain stable-x86_64-unknown-linux-gnu rustc".to_owned(),
                rustup_directory.to_path_buf()
            ),
            (
                rustup
                    .file_name()
                    .expect("rustup")
                    .to_string_lossy()
                    .into_owned(),
                "which --toolchain stable-x86_64-unknown-linux-gnu cargo".to_owned(),
                rustup_directory.to_path_buf()
            ),
            ("rustc".to_owned(), "-V".to_owned(), toolchain.clone()),
            ("cargo".to_owned(), "-V".to_owned(), toolchain.clone()),
        ],
        "only rustup's `show` runs in the check's directory"
    );

    // A `+` selector names the toolchain itself.
    assert!(label(Some("nightly"), "rustup 1.29.1 (fixture)", &toolchain).is_some());
    assert!(
        calls
            .borrow()
            .iter()
            .all(|(_, args, _)| !args.starts_with("show"))
    );
    assert!(
        calls
            .borrow()
            .iter()
            .any(|(_, args, _)| args == "which --toolchain nightly rustc")
    );
    // An older rustup may install what it is asked to show, so it is not asked.
    assert_eq!(label(None, "rustup 1.27.1 (fixture)", &toolchain), None);
    assert_eq!(calls.borrow().len(), 1, "only its version is read");
    // Tools rustup places inside the worktree may be the agent's.
    assert_eq!(
        label(None, "rustup 1.29.1 (fixture)", &workspace.join("bin")),
        None
    );
    assert!(calls.borrow().iter().all(|(_, args, _)| args != "-V"));

    // A standalone cargo ahead of rustup on the `PATH` is the one a check
    // runs: it and the rustc beside it are labelled, and rustup is not
    // asked.
    let standalone = host.join("standalone");
    fixture_program(&standalone, "cargo");
    fixture_program(&standalone, "rustc");
    let mixed =
        std::env::join_paths([standalone.clone(), host.join("rustup-bin")]).expect("a search path");
    assert!(label_on(&mixed, None, "rustup 1.29.1 (fixture)", &toolchain).is_some());
    let probed = calls
        .borrow()
        .iter()
        .map(|(program, args, _)| (program.parent().map(FsPath::to_path_buf), args.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        probed,
        [
            (Some(standalone.clone()), "-V".to_owned()),
            (Some(standalone.clone()), "-V".to_owned()),
        ]
    );
    assert_eq!(
        label_on(
            &mixed,
            Some("nightly"),
            "rustup 1.29.1 (fixture)",
            &toolchain
        ),
        None,
        "a `+` selector names nothing for a cargo rustup does not run"
    );
}

#[test]
fn a_turn_bounds_diagnostic_captures_without_bounding_ordinary_facts() {
    let turn = EOCheckedTurn::start("check-cap", true);
    let checks = (0..ENGRAM_TURN_CHECK_LIMIT)
        .map(|sequence| turn.finished_check(sequence, engram_ready_basis_capture()))
        .collect();
    turn.record_mut(|record| {
        record.engram.active_turn_checks = checks;
        // As the turn's own checks would have left the counter.
        record.engram.next_turn_check_sequence = ENGRAM_TURN_CHECK_LIMIT;
    });
    turn.recorder()
        .command_started("one-more", SIZE_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();

    let sequences = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .map(|check| check.sequence)
            .collect::<Vec<_>>()
    });
    assert_eq!(sequences.len(), ENGRAM_TURN_CHECK_LIMIT);
    assert_eq!(
        sequences.first(),
        Some(&1),
        "the oldest finished check is dropped"
    );
    assert_eq!(sequences.last(), Some(&ENGRAM_TURN_CHECK_LIMIT));

    // While every kept check still runs, further tests start none, and take
    // no snapshots.
    let mut recorder = turn.recorder();
    for index in 0..ENGRAM_TURN_CHECK_LIMIT + 4 {
        recorder
            .command_started(&format!("running-{index}"), SIZE_TEST)
            .expect("the start should record");
    }
    turn.wait_for_snapshots();
    let running = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .filter(|check| check.end.is_none())
            .map(|check| check.key.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        ENGRAM_TURN_CHECK_LIMIT
    );
    assert_eq!(running.len(), ENGRAM_TURN_CHECK_LIMIT);
    assert_eq!(
        running.last().map(String::as_str),
        Some(format!("running-{}", ENGRAM_TURN_CHECK_LIMIT - 2).as_str()),
        "the starts past the limit keep none"
    );
    assert_eq!(turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().commands.len()), ENGRAM_TURN_CHECK_LIMIT + 5,
        "all real starts remain ordinary facts even after diagnostic capture is full");
}

#[test]
fn a_session_in_a_subdirectory_captures_its_worktree_without_verification_credit() {
    let turn = EOCheckedTurn::start_in("subdirectory", true, Some("ui"), false);
    let revision = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let start = turn.record(|record| record.engram.active_turn_checks[0].start_basis.clone());
    assert_eq!(start.wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .unwrap().unwrap().source_revision, revision);
    let request = turn.finish();
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], revision);
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0].get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

#[test]
fn a_new_command_reusing_a_finished_checks_key_overlaps_it() {
    // Codex falls back to the command as the key, so a new command can reuse
    // a finished check's key while that check's snapshots are still open.
    let turn = EOCheckedTurn::start("reused-key", true);
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    turn.recorder()
        .command_started("check-0", "git checkout -- README.md")
        .expect("the start should record");

    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    pending.finish(None);
}

#[test]
fn a_read_only_delegation_child_does_not_overlap_a_check() {
    let turn = EOCheckedTurn::start("read-only-child", true);
    let (_, child) = install_required_review_delegation(&turn.state, &turn.session_id);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&child).expect("child session");
        inner.sessions[index].session.workdir = turn.root.to_string_lossy().into_owned();
    }
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    SessionRecorder::new(turn.state.clone(), child)
        .command_started("child-read", "git status")
        .expect("the child's start should record");

    assert!(!turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    pending.finish(None);
}

#[test]
fn a_write_through_termal_overlaps_open_checks_of_its_worktree_only() {
    let turn = EOCheckedTurn::start("host-write", true);
    let other = sibling_worktree(&turn, "elsewhere");
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    let overlapped = || turn.record(|record| record.engram.active_turn_checks[0].overlapped);

    turn.state.note_engram_host_write(&other.join("README.md"));
    assert!(!overlapped(), "a write in another worktree");

    // A save from the editor goes through the file route.
    let project_id = turn.record(|record| record.session.project_id.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime should build");
    let saved = runtime.block_on(write_file(
        State(turn.state.clone()),
        Json(WriteFileRequest {
            path: turn.root.join("notes.md").to_string_lossy().into_owned(),
            content: "notes\n".to_owned(),
            base_hash: None,
            overwrite: false,
            project_id: project_id.clone(),
            session_id: None,
        }),
    ));
    assert!(saved.is_ok(), "the save should succeed");
    assert!(overlapped(), "a save in the check's worktree");

    // A terminal command goes through the terminal route.
    turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
    let ran = runtime.block_on(run_terminal_command(
        State(turn.state.clone()),
        Json(TerminalCommandRequest {
            command: "git status".to_owned(),
            workdir: turn.root.to_string_lossy().into_owned(),
            project_id,
            session_id: None,
        }),
    ));
    assert!(ran.is_ok(), "the terminal command should run");
    assert!(overlapped(), "a terminal command in the check's worktree");
    pending.finish(None);
}

#[test]
fn git_terminal_and_review_writes_through_termal_overlap_an_open_check() {
    // A commit's hooks may rewrite files, a file action or a sync rewrites
    // them, a streamed command runs on a worker of its own, and a review
    // document lands under the worktree; each marks the check open there,
    // even when it fails.
    let turn = EOCheckedTurn::start("host-commit-stream", true);
    let pending = Arc::new(EngramBasisCapture::default());
    let check = turn.finished_check(0, pending.clone());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![check];
    });
    let overlapped = || turn.record(|record| record.engram.active_turn_checks[0].overlapped);
    let project_id = turn.record(|record| record.session.project_id.clone());
    let root = turn.root.to_string_lossy().into_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime should build");

    turn.change_readme("committed through TermAl\n");
    run_git_test_command(&turn.root, &["add", "README.md"]);
    let committed = runtime.block_on(commit_git_changes(
        State(turn.state.clone()),
        Json(GitCommitRequest {
            message: "Commit through TermAl".to_owned(),
            workdir: root.clone(),
            project_id: project_id.clone(),
            session_id: None,
        }),
    ));
    assert!(committed.is_ok(), "the commit should succeed");
    assert!(overlapped(), "a commit in the check's worktree");

    turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
    let app = app_router(turn.state.clone());
    let events = runtime.block_on(async {
        let response = request_response(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/terminal/run/stream")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "command": "git status",
                        "projectId": project_id,
                        "workdir": root,
                    })
                    .to_string(),
                ))
                .expect("the request should build"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        collect_sse_events(response).await
    });
    assert!(
        events.iter().any(|(name, _)| name == "complete"),
        "the streamed command should complete: {events:?}"
    );
    assert!(
        overlapped(),
        "a streamed terminal command in the check's worktree"
    );

    turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
    turn.change_readme("staged through TermAl\n");
    let staged = runtime.block_on(apply_git_file_action(
        State(turn.state.clone()),
        Json(GitFileActionRequest {
            action: GitFileAction::Stage,
            original_path: None,
            path: "README.md".to_owned(),
            status_code: None,
            workdir: root.clone(),
            project_id: project_id.clone(),
            session_id: None,
        }),
    ));
    assert!(staged.is_ok(), "the file action should succeed");
    assert!(overlapped(), "a Git file action in the check's worktree");

    // This repository has no remote, so the sync fails; it still counts.
    turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
    let synced = runtime.block_on(sync_git_changes(
        State(turn.state.clone()),
        Json(GitRepoActionRequest {
            workdir: root.clone(),
            project_id: project_id.clone(),
            session_id: None,
        }),
    ));
    assert!(synced.is_err(), "a repository without a remote cannot sync");
    assert!(overlapped(), "a Git sync in the check's worktree");

    turn.record_mut(|record| record.engram.active_turn_checks[0].overlapped = false);
    let change_set_id = "host-write-review";
    let saved = runtime.block_on(async {
        request_response(
            &app,
            Request::builder()
                .method("PUT")
                .uri(format!(
                    "/api/reviews/{change_set_id}?projectId={}",
                    project_id.as_deref().expect("the turn's project")
                ))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&default_review_document(change_set_id))
                        .expect("the review should serialize"),
                ))
                .expect("the request should build"),
        )
        .await
    });
    assert_eq!(saved.status(), StatusCode::OK, "the review should save");
    assert!(overlapped(), "a review saved under the check's worktree");
    pending.finish(None);
}

// Unit fixture for writer-scope selectors: give another row an ordinary
// trace with a captured root. This does not claim a naming-service RPC or
// provider dispatch happened; public naming is exercised by the helper below.
fn name_other_session_source_root(turn: &EOCheckedTurn, session: &str, root: &FsPath) {
    let (root, common_dir_key) = validate_engram_source_root(
        &root.to_string_lossy(), &turn.root.to_string_lossy(), &turn.root.to_string_lossy(),
    ).expect("the worktree can be captured");
    let opening = turn.record(|record| record.engram.unadmitted_active_turn.as_ref()
        .unwrap().opening_root.clone().unwrap());
    let mut inner = turn.state.inner.lock().unwrap();
    let provenance = engram_root_capture_locked(&inner, session);
    let record = inner.session_mut(session).unwrap();
    let mut trace = EngramRuntimeObservedTurn::started(
        "other-session-root-capture-fixture".to_owned(), record.active_turn_generation);
    let binding = opening.binding;
    let source_root = EngramTurnSourceRoot {
        root, common_dir_key, short_ref: binding.work_id.clone(), claim_id: binding.claim_id.clone(),
        generation: 0, sealed_revision: None,
    };
    trace.opening_root = Some(EngramRuntimeOpeningRoot {
        store: opening.store, binding: binding.clone(), provenance, source_root: Some(source_root),
    });
    record.engram.work_binding = Some(binding);
    record.engram.unadmitted_active_turn = Some(trace);
}

// Name through the actual work service before starting the check's ordinary
// turn. No grant or direct mutation of a turn's captured root is involved.
fn name_turn_source_root(turn: &EOCheckedTurn) -> PathBuf {
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.unadmitted_active_turn.as_ref().unwrap().commands.is_empty(),
            "the fixture names a root before checks, never rewrites a running check's owner");
    });
    turn.finish();
    let guard = phase_sync::PollGuard::new();
    loop {
        if turn.record(|record| !record.engram.unadmitted_worker_running
            && !runtime_observation_has_removal_duty(record)) { break; }
        guard.wait(format_args!("the initial ordinary observation settles before naming a new root"));
    }
    let worktree = turn.root.join(".worktrees").join("wt");
    run_git_test_command(&turn.root, &[
        "worktree", "add", "--quiet", "-b", "wt", worktree.to_str().unwrap(),
    ]);
    let binding = turn.record(|record| record.engram.work_binding.clone().unwrap());
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work: binding.work_id, path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).expect("ordinary work services name the root");
    turn.begin("Run the checks in the named source root.");
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    worktree
}
#[test]
fn a_test_outside_the_named_root_is_withheld_and_the_agent_is_told() {
    let turn = EOCheckedTurn::start_named("withheld-outside-root");
    let worktree = turn.root.join(".worktrees").join("wt");
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    let mut recorder = turn.recorder();

    recorder
        .command_started_in("main", "cargo test", Some("cargo test"), None)
        .expect("the start should record");
    turn.wait_for_snapshots();

    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the agent is told why the test gets no credit");
    assert!(line.contains("started in"), "{line}");
    assert!(line.contains("gets no credit"), "{line}");
    assert!(line.contains("run the tests in that root"), "{line}");
    assert!(
        turn.record(|record| record.engram.active_turn_checks.is_empty()),
        "a test in the main checkout is no check of the named root"
    );

    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    let worktree_directory = worktree.to_string_lossy().into_owned();
    recorder
        .command_started_in(
            "root",
            "cargo test",
            Some("cargo test"),
            Some(&worktree_directory),
        )
        .expect("the start should record");
    turn.wait_for_snapshots();

    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        None,
        "a test in the named root acquires its diagnostic capture silently"
    );
    let targets = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .map(|check| (check.key.clone(), check.target.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(targets.len(), 1, "{targets:?}");
    assert_eq!(targets[0].0, "root");
    assert_eq!(
        engram_exact_path_key(&targets[0].1.root),
        engram_exact_path_key(&fs::canonicalize(&worktree).expect("the worktree canonicalizes")),
        "the diagnostic check captures the named root"
    );
}

#[test]
fn a_session_in_a_turn_on_a_named_root_is_a_writer_there_too() {
    let turn = EOCheckedTurn::start_named("writer-in-named-root");
    let worktree = turn.root.join(".worktrees").join("wt");
    let root_key =
        engram_path_key(&fs::canonicalize(&worktree).expect("the worktree canonicalizes"));

    let worktrees = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&turn.session_id)
            .expect("root should exist");
        engram_writer_worktrees(&inner, index)
    };

    assert!(
        worktrees.contains(&Some(root_key.clone())),
        "{worktrees:?} holds {root_key}"
    );
    assert!(engram_worktrees_may_hold(&worktrees, &root_key));

    // The ordinary trace pins this named root only for its actual turn.
    // Ending that turn leaves no active trace working in the named root.
    turn.end();
    let worktrees = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&turn.session_id)
            .expect("root should exist");
        engram_writer_worktrees(&inner, index)
    };
    assert!(
        !worktrees.contains(&Some(root_key.clone())),
        "{worktrees:?} leaves out {root_key}"
    );
}

#[test]
fn historical_closing_scope_fields_roundtrip_without_becoming_current_credentials() {
    let turn = EOCheckedTurn::start("historical-closing-scope", true);
    let current = {
        let inner = turn.state.inner.lock().unwrap();
        EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap()
    };
    let fresh = serde_json::to_value(&current).unwrap();
    assert!(fresh.get("routing_token").is_none());
    for token in [json!("opaque-original-history"), Value::Null, json!({"oldShape":[null,"żółw 🐢"]})] {
        let mut historical = fresh.clone();
        historical["routing_token"] = token;
        historical["future_old_scope"] = json!({"nullable":null,"exact":[false,17,"🐢"]});
        let restored: EngramRuntimeClosingScope = serde_json::from_value(historical.clone()).unwrap();
        assert_eq!(serde_json::to_value(&restored).unwrap(), historical,
            "unrecognized historical scope fields are data, including explicit null");
        let inner = turn.state.inner.lock().unwrap();
        restored.require_current(&inner, &turn.session_id).unwrap();
        drop(inner);
        let original = turn.record(|record| record.engram.work_binding.clone().unwrap());
        turn.record_mut(|record| record.engram.work_binding.as_mut().unwrap().claim_fence += 1);
        {
            let inner = turn.state.inner.lock().unwrap();
            assert!(restored.require_current(&inner, &turn.session_id).is_err(),
                "historical token/unknown fields cannot authorize a different current claim");
        }
        turn.record_mut(|record| record.engram.work_binding = Some(original));
        let captured = {
            let inner = turn.state.inner.lock().unwrap();
            EngramRuntimeClosingScope::capture(&inner, &turn.session_id).unwrap()
        };
        assert_eq!(serde_json::to_value(captured).unwrap(), fresh,
            "a fresh capture does not inherit credentials or extra fields from old scope data");
    }
    let report = turn.finish();
    assert!(report.get("routing_token").is_none());
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
}

#[test]
fn retained_claude_work_without_a_new_frame_withholds_only_ordinary_source_attribution() {
    for case in ["none", "same-root", "unrelated-root"] {
        let turn = EOCheckedTurn::start_named(&format!("retained-close-cut-{case}"));
        let worktree = turn.root.join(".worktrees").join("wt");
        let hazard_root = if case == "unrelated-root" {
            sibling_worktree(&turn, "unrelated-hazard")
        } else {
            worktree.clone()
        };
        let project = turn.record(|record| record.session.project_id.clone().unwrap());
        let other = create_test_project_session(&turn.state, Agent::Claude, &project, &hazard_root);
        let since = EngramHost::interference_tick();
        let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
        let mut recorder = turn.recorder();
        recorder.command_started_in("finished-check", SIZE_TEST, Some(SIZE_TEST), Some(&cwd)).unwrap();
        turn.wait_for_snapshots();
        recorder.command_completed_with_exit("finished-check", SIZE_TEST,
            "running 3 tests\ntest result: ok. 3 passed", CommandStatus::Success,
            EngramCommandExit::Code(0)).unwrap();
        turn.wait_for_snapshots();
        if case != "none" {
            // Retained work can become visible without another command frame
            // on this consumer. Closing must still reconcile its source claim.
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&other).unwrap();
            inner.sessions[index].claude_outstanding.register(ClaudeOutstandingEntry {
                token: RuntimeToken::Claude("retained-close-cut-gone".to_owned()),
                key: "orphaned".to_owned(),
                origin: ClaudeWorkOrigin::Unattributed,
                background: false,
                nested: false,
                self_gate: false,
                locations: vec![Some(engram_worktree_root(&hazard_root))],
                registered_at: since,
            });
        }
        turn.record(|record| {
            assert_eq!(record.engram.active_turn_checks[0].fenced_by_outstanding, None,
                "no later frame has reconciled this finished diagnostic");
        });
        let request = turn.finish();
        let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["observed_result"], "passed",
            "uncertain source ownership does not erase the observed process result");
        assert!(checks[0]["finished_at"].is_string());
        // `source_change` is always present: withholding is an explicit null.
        // No baseline was installed, so an attributed change is reported as
        // measured without one.
        let source_change = &request["occurrence"]["source_change"];
        assert!(request["occurrence"].as_object().unwrap().contains_key("source_change"));
        if case == "same-root" {
            assert!(source_change.is_null(),
                "retained work at the closing cut restricts only source attribution: {request:#}");
        } else {
            assert_eq!(source_change["detection"], "assumed_missing_baseline",
                "unrelated or absent retained work leaves source attribution: {request:#}");
        }
        assert_ordinary_observation_has_no_native_credit(&turn, &request);
    }
}

// The one-call form's tests, a child module so this file does not grow
// further; it uses the ordinary-turn fixture above.
#[path = "engram_one_call_checks.rs"]
mod one_call_checks;

// A launcher check that ran no test stage, likewise a child module.
#[path = "engram_launcher_not_run.rs"]
mod launcher_not_run;

// Where a command of a shell TermAl lost may write, likewise a child module.
#[path = "engram_lost_shell_overlap.rs"]
mod lost_shell_overlap;

// Background and detached gates carried past their turn, likewise a child
// module.
#[path = "engram_carried_checks.rs"]
mod carried_checks;

// Turns Claude Code starts by itself: ownership, results and what reaches a
// grant, likewise a child module.
#[path = "engram_claude_runtime_turns.rs"]
mod claude_runtime_turns;

// An adopted runtime-started turn's provenance through every lifecycle path
// that ends it, likewise a child module.
#[path = "engram_claude_adopted_terminalization.rs"]
mod claude_adopted_terminalization;

// Which launcher stages are test stages, likewise a child module.
#[path = "engram_launcher_stages.rs"]
mod launcher_stages;

// The first extraction's end-to-end proof against its base, likewise a child
// module.
#[path = "engram_first_extraction.rs"]
mod first_extraction;

// Kimi's ACP shell-call output and exit recognition against the captured
// frames, likewise a child module.
#[path = "engram_kimi_acp_exit.rs"]
mod kimi_acp_exit;

#[path = "engram_claude_output_cut.rs"]
mod claude_output_cut;

#[path = "engram_build_checks.rs"]
mod build_checks;

#[path = "engram_focused_node_checks.rs"]
mod focused_node_checks;

#[path = "engram_declared_checks.rs"]
mod declared_checks;

// Late ends of checks closed with outcome Unknown, a child module so this
// file does not grow further.
#[path = "engram_unended_closures.rs"]
mod unended_closures;

#[path = "engram_registered_launcher_closures.rs"]
mod registered_launcher_closures;

#[path = "engram_checkpoint_non_application.rs"]
mod checkpoint_non_application;

#[path = "engram_checkpoint_result_recovery.rs"]
pub(super) mod checkpoint_result_recovery;
