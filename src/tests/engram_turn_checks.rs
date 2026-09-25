//! Tests a mediated turn ran, reported as verification and environment
//! evidence on the checkpoint that closes it (Engram w-108a13d58018
//! criterion 2, tm-winf step 2): which commands count as tests and with what
//! fingerprint, the outcome each runtime's end may claim, the environment
//! fingerprint, and the report's feed order on a claimed root session in a
//! real Git repository.
//!
//! Owns the check and evidence tests. Does not own the turn's own
//! observation (`engram_turn_observations.rs`) or the checkpoint lifecycle
//! (`engram_host_adapter.rs`). New module beside the adapter tests, created
//! instead of growing them.

use super::super::delegation_support::install_required_review_delegation;
use super::super::run_git_test_command;
use super::*;

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
        assert_eq!(check.normalized, normalized, "{command}");
        assert!(check.simple, "{command}");
    }
    assert_eq!(recognised("cargo test").program, "cargo");
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

    for command in [
        "cargo build",
        "cargo check --tests",
        "git status",
        "cd crate && cargo test",
        "npm install",
        "echo cargo test",
        "node scripts/test-launcher.mjs full --detach --notify session-1",
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
        engram_acp_command_exit(&json!({"rawOutput": {"exitCode": 2}})),
        Code(2)
    );
    assert_eq!(
        engram_acp_command_exit(&json!({"rawOutput": {"stdout": "ok"}})),
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
fn the_environment_fingerprint_is_the_rfc_8785_form_engram_recomputes() {
    // Pinned against independent computations of the canonical JSON: members
    // in key order, no whitespace, a sandbox only when present.
    let components = EngramEnvironmentComponents {
        toolchain: "rustc 1.90.0 (1159e78c4 2025-09-14); cargo 1.90.0 (840b83a10 2025-07-30)"
            .to_owned(),
        sandbox: None,
        workspace_id: "C:/github/Personal/Engram".to_owned(),
        capability_map_revision: 1,
    };
    assert_eq!(
        engram_environment_fingerprint(&components),
        "e200baa4fa31da4afd6d1bd09ad0d4c7d27c94c07e5be037d5f0eccf0864b5e6"
    );
    let components = EngramEnvironmentComponents {
        toolchain: "node v22.1.0".to_owned(),
        sandbox: Some("workspace-write".to_owned()),
        workspace_id: "/w/\"q\"".to_owned(),
        capability_map_revision: 1,
    };
    assert_eq!(
        engram_environment_fingerprint(&components),
        "f6139e9ae1133d26a75d95bddbf77cbcb05f807520320beff51a759e9d37af07"
    );
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
        engram_check_refs(&check, EngramCommandExit::Code(0)),
        ["command:cargo test", "exit:0"]
    );
    assert_eq!(
        engram_check_refs(&check, EngramCommandExit::ReportedSuccess),
        ["command:cargo test"]
    );
    let long = engram_check_command(&format!("cargo test {}", "x".repeat(1100))).expect("a test");
    assert_eq!(
        engram_check_refs(&long, EngramCommandExit::Code(0)),
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

    // At report time a succeeded run without that evidence is unknown.
    let check = EngramTurnCheck {
        grant_id: "grant".to_owned(),
        key: "check".to_owned(),
        sequence: 0,
        command: engram_check_command("cargo test nothing_matches").expect("a test"),
        target: EngramCheckTarget {
            root: PathBuf::from("C:/w"),
            directory: PathBuf::from("C:/w"),
        },
        toolchain: EngramToolchainCapture::settled(None),
        started_at: "2026-09-24T00:00:00.000Z".to_owned(),
        sandbox: None,
        start_basis: engram_ready_basis_capture(),
        overlapped: false,
        end: Some(EngramTurnCheckEnd {
            completed_at: "2026-09-24T00:00:01.000Z".to_owned(),
            exit: EngramCommandExit::Code(0),
            result_lines: vec!["test result: ok. 0 passed; 0 failed".to_owned()],
            showed_passing_tests: false,
            end_basis: engram_ready_basis_capture(),
        }),
    };
    let resolved = engram_resolve_turn_checks(
        "session",
        "grant",
        vec![check],
        std::time::Instant::now() + DEADLOCK_GUARD,
    );
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].outcome, EngramExecutionOutcome::Unknown);
}

#[test]
fn only_the_latest_sixteen_checks_are_reported() {
    let checks = (0..ENGRAM_TURN_CHECK_LIMIT + 2)
        .map(|sequence| finished_check(sequence, engram_ready_basis_capture()))
        .collect::<Vec<_>>();
    let resolved = engram_resolve_turn_checks(
        "session",
        CHECK_GRANT,
        checks,
        std::time::Instant::now() + DEADLOCK_GUARD,
    );

    assert_eq!(
        resolved
            .iter()
            .map(|check| check.check.sequence)
            .collect::<Vec<_>>(),
        (2..ENGRAM_TURN_CHECK_LIMIT + 2).collect::<Vec<_>>(),
        "the latest kept, each with its own place in the turn"
    );
}

#[test]
fn a_check_is_labelled_with_the_toolchain_it_captured_as_it_started() {
    // The overrides can change before the turn closes, so the checkpoint
    // takes the label the check captured, not a fresh one.
    let mut check = finished_check(0, engram_ready_basis_capture());
    check.toolchain = EngramToolchainCapture::settled(Some("rustc 1.80.0".to_owned()));
    let resolved = engram_resolve_turn_checks(
        "session",
        CHECK_GRANT,
        vec![check],
        std::time::Instant::now() + DEADLOCK_GUARD,
    );

    assert_eq!(resolved[0].toolchain.as_deref(), Some("rustc 1.80.0"));
}

/// A basis capture already holding one fixed basis.
fn engram_ready_basis_capture() -> Arc<EngramBasisCapture> {
    let capture = Arc::new(EngramBasisCapture::default());
    capture.finish(Some(EngramExecutionSourceBasis {
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
    let turn = CheckedTurn::start("case-spelling", true);
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

#[test]
fn environment_labels_stay_within_engram_bounds() {
    let components = engram_environment_components("rustc 1.90.0", Some("workspace-write"), "C:/w")
        .expect("valid labels");
    assert_eq!(components.sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(
        components.capability_map_revision,
        ENGRAM_CAPABILITY_MAP_REVISION
    );
    assert!(
        engram_environment_components("rustc", None, &format!("C:/{}", "w".repeat(300))).is_none(),
        "a workspace root over 256 bytes cannot be a component"
    );
    assert!(engram_environment_components("  ", None, "C:/w").is_none());
    assert!(engram_environment_components("rustc", Some(""), "C:/w").is_none());
}

/// A root session bound to claimed work, in a Git repository of its own,
/// with one turn begun and its prompt delivered.
struct CheckedTurn {
    state: AppState,
    session_id: String,
    root: PathBuf,
    transport: Arc<ScriptedEngramControlTransport>,
    _runtime_rx: std::sync::mpsc::Receiver<CodexRuntimeCommand>,
}

const CHECK_GRANT: &str = "turn-check-grant";
const SIZE_TEST: &str = "cargo test --test source_file_size -- --nocapture";
/// The toolchain label every check a `CheckedTurn` starts gets, whatever
/// toolchain the machine running the test has.
const FIXTURE_TOOLCHAIN: &str =
    "rustc 1.90.0 (fixture 2025-09-14); cargo 1.90.0 (fixture 2025-07-30)";

/// Gives the checks this test thread starts the toolchain label `label`
/// (`None`: TermAl cannot name one) instead of running the host's rustup.
fn use_toolchain_label(label: Option<&str>) {
    TEST_ENGRAM_TOOLCHAIN_LABEL
        .with(|fixture| *fixture.borrow_mut() = Some(label.map(str::to_owned)));
}

impl CheckedTurn {
    fn start(label: &str, claimed: bool) -> Self {
        Self::start_with(label, claimed, None, vec![checkpoint_reply(CHECK_GRANT)])
    }

    /// As `start`, with the session working in `subdirectory` of the
    /// repository when given, and Engram answering the turn's checkpoints
    /// with `checkpoints`.
    fn start_with(
        label: &str,
        claimed: bool,
        subdirectory: Option<&str>,
        checkpoints: Vec<ScriptedEngramControlResponse>,
    ) -> Self {
        use_toolchain_label(Some(FIXTURE_TOOLCHAIN));
        let (state, runtime_rx) =
            test_app_state_with_delegation_codex_runtime(&format!("engram-turn-check-{label}"));
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("engram-turn-check-{label}-project"));
        fs::create_dir_all(&root).expect("project root should exist");
        run_git_test_command(&root, &["init", "--quiet"]);
        run_git_test_command(&root, &["config", "user.email", "termal-tests@example.com"]);
        run_git_test_command(&root, &["config", "user.name", "TermAl tests"]);
        fs::write(root.join("README.md"), "checked\n").expect("fixture file should write");
        run_git_test_command(&root, &["add", "README.md"]);
        run_git_test_command(&root, &["commit", "--quiet", "-m", "checked"]);
        let project_id = create_test_project(&state, &root, "Engram turn checks");
        let workdir = subdirectory.map_or_else(
            || root.clone(),
            |subdirectory| {
                let workdir = root.join(subdirectory);
                fs::create_dir_all(&workdir).expect("session subdirectory should exist");
                workdir
            },
        );
        let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &workdir);
        enable_test_project_engram(&state, &project_id, &root);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("root should exist");
            inner.sessions[index].engram.context_nudge_pending = false;
        }
        let binding = claimed.then(|| test_control_work_binding(&format!("turn-check-{label}"), 1));
        let transport = ScriptedEngramControlTransport::new_with_work_bindings(
            [
                bind_reply("turn-check-token"),
                grant_reply(CHECK_GRANT),
                begin_reply(CHECK_GRANT),
            ]
            .into_iter()
            .chain(checkpoints),
            [Ok(binding)],
        );
        install_control_only_transport(&state, transport.clone());
        let dispatch = match state
            .dispatch_turn(
                &session_id,
                SendMessageRequest {
                    text: "Run the checks.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .expect("root should reach admission")
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("an idle root should dispatch"),
        };
        deliver_turn_dispatch(&state, dispatch).expect("the begun turn should be delivered");
        assert!(matches!(
            receive(&runtime_rx, "runtime should receive the prompt"),
            CodexRuntimeCommand::Prompt { .. }
        ));
        Self {
            state,
            session_id,
            root,
            transport,
            _runtime_rx: runtime_rx,
        }
    }

    fn recorder(&self) -> SessionRecorder {
        SessionRecorder::new(self.state.clone(), self.session_id.clone())
    }

    /// Runs `command` as the agent would, with `before_end` in the middle,
    /// once the check's first snapshot is taken.
    fn run(&self, key: &str, command: &str, exit: EngramCommandExit, before_end: impl FnOnce()) {
        let mut recorder = self.recorder();
        recorder
            .command_started(key, command)
            .expect("the start should record");
        self.wait_for_snapshots();
        before_end();
        recorder
            .command_completed_with_exit(
                key,
                command,
                "running 3 tests\ntest result: ok. 3 passed",
                CommandStatus::Success,
                exit,
            )
            .expect("the end should record");
        self.wait_for_snapshots();
    }

    /// Waits until every snapshot the record's checks have started is taken.
    fn wait_for_snapshots(&self) {
        let captures = self.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .flat_map(|check| {
                    std::iter::once(check.start_basis.clone())
                        .chain(check.end.as_ref().map(|end| end.end_basis.clone()))
                })
                .collect::<Vec<_>>()
        });
        for capture in captures {
            capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
        }
    }

    fn change_readme(&self, content: &str) {
        fs::write(self.root.join("README.md"), content).expect("tracked file should change");
    }

    fn record<T>(&self, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("root should exist");
        read(&inner.sessions[index])
    }

    fn record_mut(&self, change: impl FnOnce(&mut SessionRecord)) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("root should exist");
        change(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
        );
    }

    /// Completes the turn and returns the checkpoint request that closed it.
    fn finish(&self) -> Value {
        let runtime_token = self.record(|record| {
            record
                .runtime
                .runtime_token()
                .expect("the begun turn should own the runtime")
        });
        self.state
            .finish_turn_ok_if_runtime_matches(&self.session_id, &runtime_token)
            .expect("the turn should complete");
        self.transport
            .requests()
            .into_iter()
            .find(|request| request.request["operation"] == "turn_checkpoint")
            .expect("the turn is checkpointed")
            .request
    }

    fn revision(&self) -> String {
        review_freeze_fingerprint(&self.root)
            .expect("the worktree should freeze")
            .1
    }

    /// `finished_check`, run in this turn's worktree, which overlap marking
    /// compares a writer's worktree with.
    fn finished_check(
        &self,
        sequence: usize,
        end_basis: Arc<EngramBasisCapture>,
    ) -> EngramTurnCheck {
        let root = engram_worktree_root_path(&self.root);
        EngramTurnCheck {
            target: EngramCheckTarget {
                root: root.clone(),
                directory: root,
            },
            ..finished_check(sequence, end_basis)
        }
    }
}

fn observations(checkpoint: &Value) -> Vec<Value> {
    checkpoint["observations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_passing_test_run_is_reported_as_test_evidence_with_its_environment() {
    let turn = CheckedTurn::start("passing", true);
    let revision = turn.revision();
    turn.record_mut(|record| {
        record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
    });
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 2, "{checkpoint:#}");
    let producer = &observations[0];
    assert_eq!(producer["effect"], "observe");
    assert_eq!(producer["outcome"], "succeeded");
    assert_eq!(producer["source_changed"], false);
    assert_eq!(
        producer["action_fingerprint"],
        "2d7cc0ce52623c3bde2cd355ea5a9f35001e657b72bf75d3fbca2a15d321b48c",
        "the check fingerprint an author can pin"
    );
    assert_eq!(producer["source_basis"]["source_revision"], revision);
    let turn_observation = &observations[1];
    assert_eq!(turn_observation["effect"], "observe");
    assert_eq!(turn_observation["source_changed"], false);

    let evidence = &checkpoint["verification_evidence"][0];
    assert_eq!(
        evidence["producer_observation"],
        json!({"kind": "observation_id", "observation_id": producer["observation_id"]})
    );
    assert_eq!(evidence["check_kind"], "test");
    assert_eq!(
        evidence["environment"],
        json!({"kind": "index", "index": 0})
    );
    let summary = evidence["summary"].as_str().expect("a summary");
    assert!(
        summary.starts_with(&format!("`{SIZE_TEST}` exited 0\n")),
        "{summary}"
    );
    assert!(summary.ends_with("test result: ok. 3 passed"), "{summary}");
    assert_eq!(
        evidence["refs"],
        json!([format!("command:{SIZE_TEST}"), "exit:0"])
    );

    let environment = &checkpoint["environment_evidence"][0];
    assert_eq!(environment["source_basis"], producer["source_basis"]);
    assert_eq!(environment["observed_at"], producer["observed_at"]);
    let components: EngramEnvironmentComponents =
        serde_json::from_value(environment["components"].clone()).expect("components");
    assert_eq!(
        components.capability_map_revision,
        ENGRAM_CAPABILITY_MAP_REVISION
    );
    assert_eq!(
        components.sandbox.as_deref(),
        Some("workspace-write"),
        "the Codex sandbox the turn ran under"
    );
    assert_eq!(
        json!(components.workspace_id),
        producer["source_basis"]["workspace_id"]
    );
    assert_eq!(components.toolchain, FIXTURE_TOOLCHAIN);
    assert_eq!(
        environment["environment_fingerprint"],
        engram_environment_fingerprint(&components)
    );
    assert!(
        turn.record(|record| record.engram.active_turn_checks.is_empty()),
        "the report holds the checks once built"
    );
}

#[test]
fn a_change_before_the_test_is_reported_ahead_of_its_evidence() {
    // Engram needs the evidence at or after the change it answers, in time
    // and in the feed: the change is reported at the test's start revision,
    // timed at the test's start, before the producer.
    let turn = CheckedTurn::start("change-before", true);
    turn.change_readme("changed before the test\n");
    let revision = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 3, "{checkpoint:#}");
    let (change, producer, closing) = (&observations[0], &observations[1], &observations[2]);
    assert_eq!(change["effect"], "mutate_local");
    assert_eq!(change["source_changed"], true);
    assert_eq!(change["source_basis"]["source_revision"], revision);
    assert!(
        change["observed_at"].as_str() <= producer["observed_at"].as_str(),
        "the change is timed no later than the test's completion"
    );
    assert_eq!(producer["source_basis"]["source_revision"], revision);
    assert_eq!(
        closing["source_changed"], false,
        "nothing changed after the test, so the turn's own observation reports no change"
    );
    assert_eq!(
        checkpoint["verification_evidence"][0]["producer_observation"]["observation_id"],
        producer["observation_id"]
    );
}

#[test]
fn a_change_after_the_test_is_reported_after_its_evidence() {
    let turn = CheckedTurn::start("change-after", true);
    let tested = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.change_readme("changed after the test\n");
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 2, "{checkpoint:#}");
    assert_eq!(observations[0]["source_basis"]["source_revision"], tested);
    assert_eq!(observations[1]["effect"], "mutate_local");
    assert_eq!(observations[1]["source_changed"], true);
    assert_ne!(
        observations[1]["source_basis"]["source_revision"], tested,
        "the later change is at the later revision, so it does not count as tested"
    );
}

#[test]
fn a_test_the_source_moved_under_is_withheld() {
    let turn = CheckedTurn::start("moved-under", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {
        turn.change_readme("changed while the test ran\n");
    });
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    assert!(checkpoint.get("environment_evidence").is_none());
    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 1, "only the turn's own observation");
    assert_eq!(observations[0]["source_changed"], true);
}

#[test]
fn a_test_another_command_ran_beside_is_reported_as_unknown() {
    let turn = CheckedTurn::start("beside", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {
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
    });
    let checkpoint = turn.finish();

    assert_eq!(observations(&checkpoint)[0]["outcome"], "unknown");
    assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
}

#[test]
fn a_test_another_session_in_the_workspace_was_busy_during_is_unknown() {
    // Another writable session in the same workspace that reported a command
    // while the check ran may have written under it, even though it was idle
    // at the check's start and end.
    // A session in a subdirectory shares the same worktree content.
    for subdirectory in [None, Some("ui")] {
        let turn = CheckedTurn::start("other-session", true);
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
        let checkpoint = turn.finish();

        assert_eq!(
            observations(&checkpoint)[0]["outcome"],
            "unknown",
            "{subdirectory:?}"
        );
    }
}

#[test]
fn a_check_stays_open_to_writes_until_both_snapshots_are_taken() {
    // A test can finish before its snapshots are taken; an edit in between
    // would be read by both snapshots and certified as tested.
    let turn = CheckedTurn::start("open-to-writes", true);
    let pending = Arc::new(EngramBasisCapture::default());
    let settled = || {
        let capture = Arc::new(EngramBasisCapture::default());
        capture.finish(None);
        capture
    };
    let check = |key: &str, end_basis: Arc<EngramBasisCapture>| EngramTurnCheck {
        grant_id: CHECK_GRANT.to_owned(),
        key: key.to_owned(),
        sequence: 0,
        command: engram_check_command(SIZE_TEST).expect("a test"),
        target: EngramCheckTarget {
            root: PathBuf::from("C:/w"),
            directory: PathBuf::from("C:/w"),
        },
        toolchain: EngramToolchainCapture::settled(None),
        started_at: "2026-09-24T00:00:00.000Z".to_owned(),
        sandbox: None,
        start_basis: settled(),
        overlapped: false,
        end: Some(EngramTurnCheckEnd {
            completed_at: "2026-09-24T00:00:01.000Z".to_owned(),
            exit: EngramCommandExit::Code(0),
            result_lines: Vec::new(),
            showed_passing_tests: true,
            end_basis,
        }),
    };
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![
            check("pending", pending.clone()),
            check("settled", settled()),
        ];
    });
    turn.state.note_engram_workspace_edit(&turn.session_id);
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

/// A finished check of `CHECK_GRANT` at `sequence` whose closing snapshot is
/// `end_basis`.
fn finished_check(sequence: usize, end_basis: Arc<EngramBasisCapture>) -> EngramTurnCheck {
    EngramTurnCheck {
        grant_id: CHECK_GRANT.to_owned(),
        key: format!("check-{sequence}"),
        sequence,
        command: engram_check_command(SIZE_TEST).expect("a test"),
        target: EngramCheckTarget {
            root: PathBuf::from("C:/w"),
            directory: PathBuf::from("C:/w"),
        },
        toolchain: EngramToolchainCapture::settled(None),
        started_at: "2026-09-24T00:00:00.000Z".to_owned(),
        sandbox: None,
        start_basis: engram_ready_basis_capture(),
        overlapped: false,
        end: Some(EngramTurnCheckEnd {
            completed_at: "2026-09-24T00:00:01.000Z".to_owned(),
            exit: EngramCommandExit::Code(0),
            result_lines: Vec::new(),
            showed_passing_tests: true,
            end_basis,
        }),
    }
}

/// A session of `turn`'s project working in `turn`'s worktree.
fn other_session_in_worktree(turn: &CheckedTurn) -> String {
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
    let turn = CheckedTurn::start("late-overlap", true);
    let other = other_session_in_worktree(&turn);
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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
    let turn = CheckedTurn::start("other-worktree-writer", true);
    let elsewhere = sibling_worktree(&turn, "other-worktree-writer");
    let other = test_session_id(&turn.state, Agent::Codex);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        inner.sessions[index].session.workdir = elsewhere.to_string_lossy().into_owned();
    }
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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
        engram_mark_checks_overlapped_by(&mut inner, index);
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
        engram_mark_checks_overlapped_by(&mut inner, index);
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
    let turn = CheckedTurn::start("cwd-override-writer", true);
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
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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
    let turn = CheckedTurn::start("command-worktrees", true);
    let elsewhere = sibling_worktree(&turn, "command-worktrees");
    let workdir = elsewhere.to_string_lossy().into_owned();
    let root = turn.root.to_string_lossy().replace('\\', "/");
    let key = |path: &FsPath| Some(engram_worktree_root(path));
    let mut both = vec![key(&elsewhere), key(&turn.root)];
    both.sort();
    let in_workdir: &[Option<String>] = &[None];

    assert_eq!(
        engram_command_worktrees(&workdir, Some(in_workdir), Some("git status")),
        [key(&elsewhere)]
    );
    assert_eq!(
        engram_command_worktrees(&workdir, Some(&[Some(root.clone())]), Some("git status")),
        [key(&turn.root)],
        "a reported directory"
    );
    assert_eq!(
        engram_command_worktrees(&workdir, Some(&[None, Some(root.clone())]), None),
        both,
        "the workdir and where the shell is presumed to be"
    );
    for line in [
        format!("cd '{root}' && git checkout -- README.md"),
        format!("bash -lc \"cd '{root}' && git checkout -- README.md\""),
    ] {
        assert_eq!(
            engram_command_worktrees(&workdir, Some(in_workdir), Some(&line)),
            both,
            "{line}"
        );
    }
    for line in [
        "cd \"$OTHER\" && git checkout -- README.md",
        "bash -lc 'cd \"$OTHER\" && git checkout -- README.md'",
    ] {
        assert!(
            engram_command_worktrees(&workdir, Some(in_workdir), Some(line)).contains(&None),
            "{line}"
        );
    }
    assert_eq!(
        engram_command_worktrees(&workdir, None, Some("git status")),
        [None],
        "a shell TermAl lost"
    );
    for line in [
        "pwsh -wd ../elsewhere -Command git checkout -- README.md",
        "bash -lc 'pwsh -WorkingDirectory ../elsewhere -Command git checkout .'",
    ] {
        assert!(
            engram_command_worktrees(&workdir, Some(in_workdir), Some(line)).contains(&None),
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
        let turn = CheckedTurn::start("turn-start-overlap", true);
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
        turn.record_mut(|record| {
            record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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
    let turn = CheckedTurn::start("logical-cd", true);
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
fn an_overlap_marked_while_the_checkpoint_waited_is_carried_into_the_report() {
    // The checkpoint resolves copies of the checks off the lock; a mark the
    // live record gained meanwhile still makes its check unknown.
    let resolved = |sequence| EngramResolvedCheck {
        check: finished_check(sequence, engram_ready_basis_capture()),
        end: finished_check(sequence, engram_ready_basis_capture())
            .end
            .expect("a finished check"),
        outcome: EngramExecutionOutcome::Succeeded,
        basis: EngramExecutionSourceBasis {
            workspace_id: "C:/w".to_owned(),
            source_revision: "revision".to_owned(),
        },
        toolchain: None,
    };
    let mut marked = finished_check(0, engram_ready_basis_capture());
    marked.overlapped = true;
    let live = [marked, finished_check(1, engram_ready_basis_capture())];
    let mut checks = vec![resolved(0), resolved(1)];

    engram_merge_live_overlaps(&live, &mut checks);

    assert_eq!(
        checks.iter().map(|check| check.outcome).collect::<Vec<_>>(),
        [
            EngramExecutionOutcome::Unknown,
            EngramExecutionOutcome::Succeeded
        ]
    );
}

#[test]
fn an_acp_tool_call_ends_its_check_with_the_exit_code_it_reported() {
    // The ACP handler hands the recorder the exit code from `rawOutput`,
    // which a check needs to claim an outcome.
    let turn = CheckedTurn::start("acp-exit", true);
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
    let checkpoint = turn.finish();

    assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded");
}

#[test]
fn a_background_command_counts_as_running_for_the_rest_of_the_turn() {
    // Its result marks its launch, so it may still be writing when a later
    // test runs.
    let turn = CheckedTurn::start("background-writer", true);
    let mut recorder = turn.recorder();
    recorder
        .command_started("watch", "npm run watch")
        .expect("the start should record");
    recorder
        .command_completed_with_exit(
            "watch",
            "npm run watch",
            "",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch should record");
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    assert_eq!(observations(&checkpoint)[0]["outcome"], "unknown");
}

#[test]
fn a_denied_command_stops_counting_as_running() {
    let turn = CheckedTurn::start("denied", true);
    let mut recorder = turn.recorder();
    recorder
        .command_started("denied", "rm -rf target")
        .expect("the start should record");
    recorder
        .command_abandoned("denied")
        .expect("the abandon should record");
    // A denied test never ran: its check is dropped rather than left open to
    // writes until the next grant.
    recorder
        .command_started("denied-test", SIZE_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();
    recorder
        .command_abandoned("denied-test")
        .expect("the abandon should record");
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    assert_eq!(observations(&checkpoint)[0]["outcome"], "succeeded");
}

#[test]
fn checks_that_share_a_runtime_key_keep_distinct_observation_ids() {
    // Codex falls back to the command as the key when an item has no id.
    let turn = CheckedTurn::start("same-key", true);
    turn.run(SIZE_TEST, SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.run(SIZE_TEST, SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let ids = observations(&checkpoint)
        .iter()
        .map(|observation| {
            observation["observation_id"]
                .as_str()
                .expect("an observation id")
                .to_owned()
        })
        .collect::<Vec<_>>();
    let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
    assert_eq!(
        checkpoint["verification_evidence"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn a_refused_report_with_checks_falls_back_to_the_turns_own_observation() {
    let turn = CheckedTurn::start("refused-fallback", true);
    let with_checks = EngramTurnReport {
        observations: vec![EngramExecutionObservationInput {
            observation_id: "check".to_owned(),
            action_fingerprint: "a".repeat(64),
            effect: EngramEffect::Observe,
            outcome: EngramExecutionOutcome::Succeeded,
            source_changed: false,
            source_basis: None,
            observed_at: None,
        }],
        ..EngramTurnReport::default()
    };
    let own = EngramTurnReport {
        observations: vec![EngramExecutionObservationInput {
            observation_id: "own".to_owned(),
            ..with_checks.observations[0].clone()
        }],
        ..EngramTurnReport::default()
    };
    turn.record_mut(|record| {
        record.engram.active_turn_report = Some((CHECK_GRANT.to_owned(), with_checks.clone()));
        record.engram.active_turn_report_fallback = Some((CHECK_GRANT.to_owned(), own.clone()));
    });

    turn.state
        .forget_refused_engram_turn_report(&turn.session_id, CHECK_GRANT, Some("redacted"));
    assert_eq!(
        turn.state
            .cached_engram_turn_report(&turn.session_id, CHECK_GRANT),
        own,
        "the first refusal falls back to the turn's own observation"
    );
    turn.state
        .forget_refused_engram_turn_report(&turn.session_id, CHECK_GRANT, Some("redacted"));
    assert!(
        turn.state
            .cached_engram_turn_report(&turn.session_id, CHECK_GRANT)
            .is_empty(),
        "a refused fallback leaves nothing to resend"
    );
    let _ = turn.finish();
}

#[test]
fn a_failing_test_and_a_masked_exit_are_reported_honestly() {
    let turn = CheckedTurn::start("honest", true);
    turn.run("check-1", "cargo test", EngramCommandExit::Code(101), || {});
    turn.run(
        "check-2",
        "cargo test 2>&1 | tail -5",
        EngramCommandExit::Code(0),
        || {},
    );
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations[0]["outcome"], "failed");
    assert_eq!(
        observations[1]["outcome"], "unknown",
        "a pipe's exit status is not the test's"
    );
    let evidence = checkpoint["verification_evidence"]
        .as_array()
        .expect("evidence");
    assert_eq!(evidence.len(), 2);
    assert_eq!(
        evidence[0]["refs"],
        json!(["command:cargo test", "exit:101"])
    );
    assert_eq!(
        checkpoint["environment_evidence"].as_array().map(Vec::len),
        Some(1),
        "both checks ran on one revision with one toolchain"
    );
    assert_eq!(
        evidence[1]["environment"],
        json!({"kind": "index", "index": 0})
    );
}

#[test]
fn a_background_run_and_a_non_test_command_are_not_reported() {
    let turn = CheckedTurn::start("not-reported", true);
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
        "a background run is never reported, so it takes no closing snapshot"
    );
    turn.run("build", "cargo build", EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "{checkpoint:#}"
    );
    assert_eq!(observations(&checkpoint).len(), 1);
}

#[test]
fn an_unbound_session_keeps_no_checks() {
    // Engram admits evidence only through an exact work binding, so an
    // unbound session has nothing to keep them for.
    let turn = CheckedTurn::start("unbound", false);
    turn.recorder()
        .command_started("check-1", SIZE_TEST)
        .expect("the start should record");
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none());
    assert!(checkpoint.get("observations").is_none());
}

/// Every checkpoint request that closed `turn`'s grant, in order.
fn checkpoints(turn: &CheckedTurn) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .map(|request| request.request)
        .collect()
}

/// A second Git repository beside `turn`'s, with a manifest a command can
/// name.
fn sibling_worktree(turn: &CheckedTurn, name: &str) -> PathBuf {
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

/// A resolved check at `sequence` on `revision`, labelled `toolchain`.
fn resolved_check(sequence: usize, revision: &str, toolchain: &str) -> EngramResolvedCheck {
    let check = finished_check(sequence, engram_ready_basis_capture());
    EngramResolvedCheck {
        end: check.end.clone().expect("a finished check"),
        check,
        outcome: EngramExecutionOutcome::Succeeded,
        basis: EngramExecutionSourceBasis {
            workspace_id: "C:/w".to_owned(),
            source_revision: revision.to_owned(),
        },
        toolchain: Some(toolchain.to_owned()),
    }
}

#[test]
fn a_test_is_credited_only_to_the_worktree_it_ran_in() {
    // A test that ran in, or named, another repository must not become
    // evidence at this worktree's revision.
    let turn = CheckedTurn::start("which-worktree", true);
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
    for other in ["/cd/x", "c/x", "//server/share", "/"] {
        assert_eq!(engram_msys_drive_path(other), other);
    }
}

#[test]
fn a_session_starts_no_check_while_its_capture_workers_are_at_their_limit() {
    // Snapshots of checks already dropped may still run; however many tests
    // finish meanwhile, none adds to them.
    let turn = CheckedTurn::start("capture-limit", true);
    let workers = turn.record(|record| record.engram.capture_workers.clone());
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let pending = (0..ENGRAM_CAPTURE_WORKER_LIMIT)
        .map(|_| {
            let gate = gate.clone();
            EngramCapture::spawn(&workers, move || {
                let (open, opened) = &*gate;
                let _ = opened
                    .wait_timeout_while(
                        open.lock().expect("gate mutex poisoned"),
                        DEADLOCK_GUARD,
                        |open| !*open,
                    )
                    .expect("gate mutex poisoned");
            })
        })
        .collect::<Vec<_>>();
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

    let (open, opened) = &*gate;
    *open.lock().expect("gate mutex poisoned") = true;
    opened.notify_all();
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
    let turn = CheckedTurn::start("shell-directory", true);
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
        });
    });
    run_to("new-runtime", SIZE_TEST, Some(0));
    turn.record_mut(|record| {
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: record.runtime.runtime_token(),
            directory: None,
            pending: None,
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
fn file_change_tracking_does_not_reopen_a_change_a_reported_check_answered() {
    // After a reported check the turn's own observation is judged by the
    // revisions alone: the tracking can arrive late, and would reopen a
    // change the check already answered.
    let turn = CheckedTurn::start("tracking-after-check", true);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.record_mut(|record| {
        record.active_turn_file_changes.insert(
            "target/debug/out.log".to_owned(),
            WorkspaceFileChangeKind::Modified,
        );
    });
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 2, "{checkpoint:#}");
    assert_eq!(observations[1]["source_changed"], false);
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
fn a_test_naming_a_link_out_of_the_worktree_is_not_credited_to_it() {
    // A relative argument can lead elsewhere through a link: the snapshots
    // see only the link, not what the test ran.
    let turn = CheckedTurn::start("linked-worktree", true);
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
fn a_node_selector_on_a_linked_test_file_is_not_credited_to_the_worktree() {
    // The selector names a file that is itself a link out of the worktree.
    let turn = CheckedTurn::start("linked-file", true);
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
    let turn = CheckedTurn::start("acp-repeated-starts", true);
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
    let turn = CheckedTurn::start("acp-described", true);
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
    let turn = CheckedTurn::start("acp-withheld", true);
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
    turn.state.note_engram_workspace_edit(&turn.session_id);
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
    let turn = CheckedTurn::start("acp-shell", true);
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
fn a_check_left_by_an_ended_grant_no_longer_counts_as_open() {
    // A grant can end without its report (a compensating close, a reset);
    // its checks stay until the next grant clears them, but they will never
    // be reported, so they cost no worktree lookups and no snapshots.
    let turn = CheckedTurn::start("ended-grant", true);
    let mut recorder = turn.recorder();
    recorder
        .command_started("left", SIZE_TEST)
        .expect("the start should record");
    turn.wait_for_snapshots();
    assert!(turn.record(engram_has_open_check));

    turn.record_mut(|record| record.engram.active_grant_id = None);
    assert!(!turn.record(engram_has_open_check));
    recorder
        .command_completed_with_exit(
            "left",
            SIZE_TEST,
            "test result: ok. 3 passed",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    assert!(
        turn.record(|record| {
            record
                .engram
                .active_turn_checks
                .iter()
                .all(|check| check.end.is_none())
        }),
        "it takes no closing snapshot"
    );
}

#[test]
fn a_check_whose_toolchain_cannot_be_named_is_reported_without_an_environment() {
    let turn = CheckedTurn::start("no-toolchain", true);
    use_toolchain_label(None);
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let evidence = &checkpoint["verification_evidence"][0];
    assert_eq!(evidence["check_kind"], "test", "{checkpoint:#}");
    assert!(evidence.get("environment").is_none(), "{checkpoint:#}");
    assert!(checkpoint.get("environment_evidence").is_none());
}

#[test]
fn a_workdir_spelled_through_a_link_still_credits_its_worktree() {
    // A workdir kept in a user-facing spelling (`/var` for `/private/var`, a
    // Windows short name) names the same worktree as its resolved root.
    let turn = CheckedTurn::start("aliased-workdir", true);
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
    let turn = CheckedTurn::start("quoted-unc", true);
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
fn checks_under_an_observe_only_grant_do_not_hide_a_change() {
    // Under a grant that mediates no local mutation no change observation
    // precedes the checks, so the turn's own observation is judged against
    // its begin-time basis and withheld when the source changed.
    let turn = CheckedTurn::start("observe-only", true);
    let moved = EngramExecutionSourceBasis {
        workspace_id: "C:/w".to_owned(),
        source_revision: "moved".to_owned(),
    };
    let (report, fallback) = turn.record(|record| {
        engram_turn_report(
            record,
            &turn.session_id,
            CHECK_GRANT,
            EngramExecutionOutcome::Succeeded,
            false,
            Some(moved),
            vec![resolved_check(0, "moved", "rustc 1")],
        )
    });

    assert_eq!(
        report
            .observations
            .iter()
            .map(|observation| observation.effect.clone())
            .collect::<Vec<_>>(),
        [EngramEffect::Observe],
        "the check's producer only; the turn's own observation is withheld"
    );
    assert!(
        fallback.is_none(),
        "a withheld turn has nothing to fall back to"
    );
}

#[test]
fn a_turn_keeps_no_more_checks_than_it_can_report() {
    let turn = CheckedTurn::start("check-cap", true);
    turn.record_mut(|record| {
        record.engram.active_turn_checks = (0..ENGRAM_TURN_CHECK_LIMIT)
            .map(|sequence| finished_check(sequence, engram_ready_basis_capture()))
            .collect();
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
}

#[test]
fn a_session_in_a_subdirectory_reports_its_worktrees_evidence() {
    let turn = CheckedTurn::start_with(
        "subdirectory",
        true,
        Some("ui"),
        vec![checkpoint_reply(CHECK_GRANT)],
    );
    let revision = turn.revision();
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let checkpoint = turn.finish();

    let observations = observations(&checkpoint);
    assert_eq!(observations.len(), 2, "{checkpoint:#}");
    assert_eq!(observations[0]["outcome"], "succeeded");
    for observation in &observations {
        assert_eq!(
            observation["source_basis"]["source_revision"], revision,
            "the check and the turn both have the worktree's basis"
        );
    }
}

#[test]
fn a_new_command_reusing_a_finished_checks_key_overlaps_it() {
    // Codex falls back to the command as the key, so a new command can reuse
    // a finished check's key while that check's snapshots are still open.
    let turn = CheckedTurn::start("reused-key", true);
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
    });
    turn.recorder()
        .command_started("check-0", "git checkout -- README.md")
        .expect("the start should record");

    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    pending.finish(None);
}

#[test]
fn a_read_only_delegation_child_does_not_overlap_a_check() {
    let turn = CheckedTurn::start("read-only-child", true);
    let (_, child) = install_required_review_delegation(&turn.state, &turn.session_id);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&child).expect("child session");
        inner.sessions[index].session.workdir = turn.root.to_string_lossy().into_owned();
    }
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
    });
    SessionRecorder::new(turn.state.clone(), child)
        .command_started("child-read", "git status")
        .expect("the child's start should record");

    assert!(!turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    pending.finish(None);
}

#[test]
fn a_write_through_termal_overlaps_open_checks_of_its_worktree_only() {
    let turn = CheckedTurn::start("host-write", true);
    let other = sibling_worktree(&turn, "elsewhere");
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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
    let turn = CheckedTurn::start("host-commit-stream", true);
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        record.engram.active_turn_checks = vec![turn.finished_check(0, pending.clone())];
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

#[test]
fn a_check_past_the_environment_limit_is_reported_without_one() {
    let turn = CheckedTurn::start("environment-limit", true);
    let start = turn.revision();
    let checks = (0..=ENGRAM_TURN_ENVIRONMENT_LIMIT)
        .map(|sequence| resolved_check(sequence, &start, &format!("rustc 1.{sequence}")))
        .collect::<Vec<_>>();
    let (report, _) = turn.record(|record| {
        engram_turn_report(
            record,
            &turn.session_id,
            CHECK_GRANT,
            EngramExecutionOutcome::Succeeded,
            true,
            None,
            checks,
        )
    });

    assert_eq!(
        report.environment_evidence.len(),
        ENGRAM_TURN_ENVIRONMENT_LIMIT
    );
    let evidence = &report.verification_evidence;
    assert_eq!(evidence.len(), ENGRAM_TURN_ENVIRONMENT_LIMIT + 1);
    assert!(
        evidence[ENGRAM_TURN_ENVIRONMENT_LIMIT]
            .environment
            .is_none(),
        "the check is still reported, without an environment"
    );
}

#[test]
fn a_check_after_a_later_change_cites_a_fresh_environment() {
    // The source goes A, B, then back to A. The third check follows a change
    // observation at A, so it may not cite the environment observed for the
    // first check, before that change.
    let turn = CheckedTurn::start("environment-floor", true);
    let start = turn.revision();
    let checks = vec![
        resolved_check(0, &start, "rustc 1"),
        resolved_check(1, "moved", "rustc 1"),
        resolved_check(2, &start, "rustc 1"),
    ];
    let (report, _) = turn.record(|record| {
        engram_turn_report(
            record,
            &turn.session_id,
            CHECK_GRANT,
            EngramExecutionOutcome::Succeeded,
            true,
            None,
            checks,
        )
    });

    assert_eq!(
        report
            .verification_evidence
            .iter()
            .map(|evidence| evidence.environment.clone())
            .collect::<Vec<_>>(),
        (0..3)
            .map(|index| Some(EngramEnvironmentReference::Index { index }))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_refused_report_with_evidence_is_retried_as_the_turns_own_observation() {
    let turn = CheckedTurn::start_with(
        "refused-evidence",
        true,
        None,
        vec![
            remote_error_reply("summary_redacted"),
            checkpoint_reply(CHECK_GRANT),
        ],
    );
    turn.run("check-1", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let refused = turn.finish();
    assert!(
        refused.get("verification_evidence").is_some(),
        "{refused:#}"
    );
    turn.state
        .kill_session(&turn.session_id)
        .expect("terminal cleanup closes the grant again");

    let closes = checkpoints(&turn);
    assert_eq!(closes.len(), 2);
    let retry = &closes[1];
    assert!(retry.get("verification_evidence").is_none(), "{retry:#}");
    assert!(retry.get("environment_evidence").is_none());
    assert_eq!(
        observations(retry).len(),
        1,
        "the turn's own observation only"
    );
    let key = |checkpoint: &Value| {
        checkpoint["idempotency_key"]
            .as_str()
            .expect("an idempotency key")
            .to_owned()
    };
    assert!(key(&refused).contains(":evidence:"));
    assert!(!key(retry).contains(":evidence:"));
    assert_ne!(key(&refused), key(retry));
}
