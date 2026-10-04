// Where a test's PowerShell children keep their temporary files. Windows
// PowerShell writes a startup policy probe (__PSScriptPolicyTest_*.ps1 and
// .psm1) to its TEMP at every start and deletes it moments later; a child
// terminated in between leaves the pair behind. Under the test launcher TEMP
// is the shared run root, which no test's own temp root can clean, so a test
// that may end a PowerShell child during its startup gives it a TEMP inside
// that test's own temp root.
//
// What it owns: the checks that the terminal shell, the PowerShell children it
// starts, and the review-freeze deadline fixture get such a TEMP.
// What it does not own: the Engram fixture children (src/tests/engram_child_temp.rs)
// and the launcher's temp guard (unchanged).
//
// New file, not split from another.
use super::*;

/// Runs a terminal command in a fresh test temp root (the workdir) and returns
/// that root, removed when it drops, and the shell's stdout.
#[cfg(windows)]
fn terminal_output_in_test_root(
    label: &str,
    command: impl FnOnce(&FsPath) -> String,
) -> (TestTempRoot, String) {
    let root = TestTempRoot::create(label);
    let command = command(root.path());
    let response =
        run_terminal_shell_command_with_timeout(&command, root.path(), Duration::from_secs(60))
            .expect("the terminal command should run");
    assert!(
        response.success && !response.timed_out,
        "the terminal command should succeed: exit {:?}, stdout {:?}, stderr {:?}",
        response.exit_code,
        response.stdout,
        response.stderr
    );
    (root, response.stdout)
}

/// Whether `value` names a directory inside `root`, compared canonically.
#[cfg(windows)]
fn lies_inside(value: &str, root: &FsPath) -> bool {
    let root = fs::canonicalize(root).expect("the test root should canonicalize");
    match fs::canonicalize(value.trim()) {
        Ok(path) => path != root && path.starts_with(&root),
        Err(_) => false,
    }
}

#[cfg(windows)]
#[test]
fn terminal_shell_and_its_powershell_children_keep_temp_inside_their_test_root() {
    let (root, stdout) = terminal_output_in_test_root("termal-terminal-child-temp", |root| {
        let marker = root
            .join("grandchild-temp.txt")
            .to_string_lossy()
            .replace('\'', "''");
        // The grandchild is started the way the terminal timeout and teardown
        // tests start theirs, and reports the TEMP it was given.
        format!(
            "Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-Command','[IO.File]::WriteAllText(''{marker}'', $env:TEMP + ''|'' + $env:TMP)') -WindowStyle Hidden -Wait; [Console]::Out.Write($env:TEMP + '|' + $env:TMP)"
        )
    });
    let grandchild = fs::read_to_string(root.path().join("grandchild-temp.txt"))
        .expect("the grandchild should report its TEMP");
    for (who, reported) in [
        ("shell", stdout.as_str()),
        ("grandchild", grandchild.as_str()),
    ] {
        let (temp, tmp) = reported
            .trim()
            .split_once('|')
            .unwrap_or_else(|| panic!("{who} should report TEMP|TMP, got {reported:?}"));
        for (name, value) in [("TEMP", temp), ("TMP", tmp)] {
            assert!(
                lies_inside(value, root.path()),
                "the {who}'s {name} {value:?} should lie inside its test root {}, not the shared \
                 run root (process TEMP {:?})",
                root.path().display(),
                std::env::var_os("TEMP")
            );
        }
    }
}

#[test]
fn review_freeze_deadline_fixture_keeps_temp_inside_its_test_root() {
    let state = test_app_state();
    let test_root = state
        .test_temp_root
        .as_ref()
        .expect("test state should own a temp root")
        .path()
        .to_owned();
    let command = super::review_freeze::freeze_deadline_fixture_command(
        &super::review_freeze::freeze_fixture_child_temp(&state),
    );
    let canonical_root = fs::canonicalize(&test_root).expect("the test root should canonicalize");
    for name in ["TEMP", "TMP"] {
        let value = command
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
            .unwrap_or_else(|| panic!("the deadline fixture should be given {name}"));
        let value = fs::canonicalize(value).expect("the fixture's TEMP should exist");
        assert!(
            value != canonical_root && value.starts_with(&canonical_root),
            "the deadline fixture's {name} {} should lie inside its test root {}",
            value.display(),
            canonical_root.display()
        );
    }
}

#[test]
fn terminal_child_temp_is_not_created_for_a_workdir_outside_a_test_root() {
    let base = test_temp_dir();
    assert!(
        terminal_test_child_temp_env(&base).is_empty(),
        "the test temp directory itself is no test's root"
    );
    assert!(!base.join("child-temp").exists());
    let outside = std::env::current_dir().expect("the test process should have a directory");
    assert!(
        terminal_test_child_temp_env(&outside).is_empty(),
        "a workdir outside the test temp directory gets the inherited TEMP"
    );
    assert!(
        !outside.join("child-temp").exists(),
        "nothing may be created outside the test temp directory"
    );
}

#[test]
fn terminal_child_temp_sits_in_the_test_root_not_in_a_nested_workdir() {
    let root = TestTempRoot::create("termal-terminal-nested");
    let workdir = root.path().join("project").join("sub");
    fs::create_dir_all(&workdir).expect("the nested workdir should be created");
    let environment = terminal_test_child_temp_env(&workdir);
    let names: Vec<_> = environment.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["TEMP", "TMP"]);
    for (_, value) in &environment {
        assert_eq!(PathBuf::from(value), root.path().join("child-temp"));
    }
    assert!(
        !root.path().join("project").join("child-temp").exists()
            && !workdir.join("child-temp").exists(),
        "a repository workdir must gain no untracked entry"
    );
}

/// Asserts that a `child-temp` link in `root`, pointing at `target`, gives
/// the shell of a workdir in `root` no TEMP and has nothing written through
/// it. The link is left for the root's guard, whose removal does not follow
/// links (as in src/tests/engram_child_temp.rs).
fn assert_linked_child_temp_is_refused(root: &FsPath, target: &FsPath) {
    let workdir = root.join("project");
    fs::create_dir_all(&workdir).expect("the workdir should be created");
    fs::create_dir_all(target).expect("the link target should be created");
    super::engram_child_temp::link_directory(&root.join("child-temp"), target);
    assert!(
        terminal_test_child_temp_env(&workdir).is_empty(),
        "a child-temp linked to {} must give no environment",
        target.display()
    );
    assert!(
        fs::read_dir(target)
            .expect("the link target should be readable")
            .next()
            .is_none(),
        "nothing may be written through the link"
    );
}

#[test]
fn terminal_child_temp_linked_to_a_sibling_in_its_test_root_is_refused() {
    // The workdir itself is the sibling: a link must not carry TEMP into a
    // repository workdir of the same test root.
    let root = TestTempRoot::create("termal-terminal-child-temp-sibling");
    assert_linked_child_temp_is_refused(root.path(), &root.path().join("project"));
}

#[test]
fn terminal_child_temp_linked_out_of_its_test_root_is_refused() {
    let elsewhere = TestTempRoot::create("termal-terminal-elsewhere");
    let root = TestTempRoot::create("termal-terminal-child-temp-outside");
    assert_linked_child_temp_is_refused(root.path(), elsewhere.path());
    // The linking root drops first, so the link goes before its target.
    drop(root);
}
