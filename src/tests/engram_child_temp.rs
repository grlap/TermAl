// Where a test's Engram fixture child keeps its temporary files. PowerShell
// writes a startup policy probe (__PSScriptPolicyTest_*.ps1 and .psm1) to its
// TEMP and deletes it moments later; a child terminated in between leaves the
// files behind. Under the test launcher TEMP is the shared run root, which a
// test's own temp root cannot clean, so a test's fixture child is given a
// TEMP inside its Engram home (src/engram_test_child_temp.rs). These tests
// check that placement on the launch paths and that nothing is created for a
// home outside the test temp root. New file, not split from another.
use super::*;

/// Runs the control fixture's readiness through the production diagnostic
/// launch path in the mode that reports the child's TEMP and TMP.
fn fixture_child_temp(root: &FsPath, home: &FsPath) -> (String, String) {
    let marker = root.join(".engram-project");
    fs::write(&marker, "fixture-report-temp\n").expect("fixture declaration should be written");
    let output = run_engram_diagnostic_within(
        &super::engram_host_adapter::real_engram_control_fixture_path(),
        &marker,
        home,
        root,
        "readiness",
        // A hang guard, not a timing assertion: the fixture prints and exits.
        super::phase_sync::DEADLOCK_GUARD,
        root,
    )
    .unwrap_or_else(|error| panic!("fixture readiness should run: {}", error.message));
    assert!(output.status.success(), "fixture readiness failed");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let value = |name: &str| {
        stdout
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix(&format!("{name}="))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| panic!("fixture should report {name}: {stdout}"))
    };
    (value("TEMP"), value("TMP"))
}

/// A connection whose Engram home is `home`.
fn connection_with_home(home: &FsPath) -> EngramConnectionConfig {
    EngramConnectionConfig {
        binary_path: super::engram_host_adapter::real_engram_control_fixture_path(),
        project_file: home.join(".engram-project"),
        home: home.to_path_buf(),
        project_root: home.to_path_buf(),
        actor_id: "child-temp-actor".to_owned(),
        actor_context: None,
        session_id: "child-temp-session".to_owned(),
    }
}

/// The environment apply_engram_connection_environment (the control process
/// and CLI command paths) gives a command, by variable name.
fn connection_environment(home: &FsPath) -> std::collections::HashMap<String, Option<String>> {
    let connection = connection_with_home(home);
    let mut command = engram_command(&connection.binary_path);
    apply_engram_connection_environment(&mut command, &connection);
    command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

#[test]
fn an_engram_fixture_child_keeps_its_temp_inside_its_engram_home() {
    let root = TestTempRoot::create("engram-child-temp");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("fixture home should exist");
    let canonical_home = fs::canonicalize(&home).expect("home should canonicalize");

    let (temp, tmp) = fixture_child_temp(root.path(), &home);

    for (name, value) in [("TEMP", temp), ("TMP", tmp)] {
        let reported = fs::canonicalize(&value)
            .unwrap_or_else(|error| panic!("{name} {value} should exist: {error}"));
        assert!(
            reported.starts_with(&canonical_home),
            "the fixture child's {name} must lie inside its Engram home {} so a \
             PowerShell startup probe it leaves is removed with the test's root, not \
             left in the shared run root: {}",
            canonical_home.display(),
            reported.display()
        );
    }
}

#[test]
fn the_connection_environment_keeps_a_test_child_temp_inside_its_engram_home() {
    let root = TestTempRoot::create("engram-child-temp-connection");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("fixture home should exist");
    let expected = fs::canonicalize(&home)
        .expect("home should canonicalize")
        .join("child-temp");

    let environment = connection_environment(&home);

    for name in ["TEMP", "TMP", "TMPDIR"] {
        let value = environment
            .get(name)
            .cloned()
            .flatten()
            .unwrap_or_else(|| panic!("the connection environment should set {name}"));
        assert_eq!(
            PathBuf::from(&value),
            expected,
            "{name} should be the home's child temp"
        );
    }
    assert!(
        expected.is_dir(),
        "the child temp should exist inside the home"
    );
    assert!(
        environment
            .get("TERMAL_TEST_USER_TEMP")
            .cloned()
            .flatten()
            .is_some(),
        "the fixture should be told the test's user temp, so its containment check still \
         finds the test temp root"
    );
}

/// Makes `link` a directory link (a junction on Windows, a symlink elsewhere)
/// to `target`.
pub(super) fn link_directory(link: &FsPath, target: &FsPath) {
    #[cfg(windows)]
    {
        // A junction needs no symlink privilege; std has no API for one.
        let output = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("mklink should run");
        assert!(
            output.status.success(),
            "the junction should be created: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("the symlink should be created");
}

#[test]
fn a_child_temp_that_links_out_of_its_engram_home_is_refused() {
    // Both the link and its target live in this test's own temp root.
    let root = TestTempRoot::create("engram-child-temp-linked");
    let home = root.path().join("home");
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(&home).expect("fixture home should exist");
    fs::create_dir_all(&elsewhere).expect("link target should exist");
    link_directory(&home.join("child-temp"), &elsewhere);

    assert!(
        engram_test_child_temp_env(&home).is_empty(),
        "a child-temp that resolves outside its home must give no environment"
    );
    let environment = connection_environment(&home);
    for name in ["TEMP", "TMP", "TMPDIR"] {
        assert!(
            !environment.contains_key(name),
            "the connection environment must not redirect {name} through a linked child-temp"
        );
    }
    assert!(
        fs::read_dir(&elsewhere)
            .expect("link target should be readable")
            .next()
            .is_none(),
        "nothing may be written through the link"
    );
}

#[test]
fn nothing_is_created_or_redirected_for_a_home_outside_the_test_temp_root() {
    // An existing directory outside <user temp>/termal/tests: this worktree.
    let outside = FsPath::new(env!("CARGO_MANIFEST_DIR"));
    let child_temp = outside.join("child-temp");
    assert!(
        !child_temp.exists(),
        "precondition: {} must not exist",
        child_temp.display()
    );

    assert!(
        engram_test_child_temp_env(outside).is_empty(),
        "a home outside the test temp root must get no child temp environment"
    );
    let environment = connection_environment(outside);
    for name in ["TEMP", "TMP", "TMPDIR", "TERMAL_TEST_USER_TEMP"] {
        assert!(
            !environment.contains_key(name),
            "the connection environment must not redirect {name} for a home outside the \
             test temp root"
        );
    }
    assert!(
        !child_temp.exists(),
        "nothing may be created in a home outside the test temp root: {}",
        child_temp.display()
    );
}
