//! Tests write only inside the repository. The user temp every test temporary
//! tree lives under, and any home a test resolves, lie in this checkout or its
//! main checkout; the user's real home and the platform temp directory are
//! refused or never consulted. Owns the behavioural proof of that boundary,
//! not the boundary itself (src/test_temp_paths.rs, src/codex_home.rs).
use super::*;

const BOUNDARY_CHILD: &str = "TERMAL_TEST_REPOSITORY_BOUNDARY_CHILD";

fn manifest() -> &'static FsPath {
    FsPath::new(env!("CARGO_MANIFEST_DIR"))
}

/// A path at the file system's root, outside any checkout, standing in for
/// the user's home or the platform temp directory. Never created.
fn outside_the_repository(name: &str) -> PathBuf {
    manifest()
        .ancestors()
        .last()
        .expect("an absolute path has a root")
        .join(format!("{name}-{}", Uuid::new_v4()))
}

fn boundary_child(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "tests::test_repository_boundary::repository_boundary_child",
            "--nocapture",
        ])
        .env(BOUNDARY_CHILD, mode);
    command
}

#[test]
fn a_user_temp_inside_the_repository_is_accepted() {
    for inside in [
        manifest().join("target").join("tmp").join("termal"),
        manifest().join(".tmp").join("fixture"),
        manifest().to_path_buf(),
    ] {
        assert_eq!(
            check_test_user_temp(&inside),
            Ok(()),
            "{}",
            inside.display()
        );
    }
}

#[test]
fn a_user_temp_outside_the_repository_is_refused_with_the_path_and_the_rule() {
    let main = test_repository_roots(manifest())
        .pop()
        .expect("the checkout is always a root");
    let mut sibling = main.into_os_string();
    sibling.push("-sibling");
    for outside in [
        outside_the_repository("termal-platform-temp-stand-in"),
        // Shares the main checkout's prefix: containment is per component.
        PathBuf::from(sibling),
        // Starts inside and climbs out.
        manifest()
            .join("target")
            .join("..")
            .join("..")
            .join("escape"),
        PathBuf::from("target").join("tmp").join("termal"),
    ] {
        let rule = check_test_user_temp(&outside)
            .expect_err(&format!("{} must be refused", outside.display()));
        assert!(rule.contains(&outside.display().to_string()), "{rule}");
        assert!(rule.contains("outside the repository"), "{rule}");
    }
}

#[test]
fn a_linked_worktree_admits_its_main_checkout_read_from_the_git_files() {
    let sandbox = TestTempRoot::create("repository-roots");
    let main = sandbox.path().join("main");
    let gitdir = main.join(".git").join("worktrees").join("linked");
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    let linked = sandbox.path().join("linked");
    fs::create_dir(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .unwrap();
    assert_eq!(
        test_repository_roots(&linked),
        vec![linked.clone(), main.clone()]
    );

    let relative = sandbox.path().join("relative");
    fs::create_dir(&relative).unwrap();
    fs::write(
        relative.join(".git"),
        "gitdir: ../main/.git/worktrees/linked\n",
    )
    .unwrap();
    assert_eq!(
        test_repository_roots(&relative),
        vec![relative.clone(), main.clone()]
    );

    // A main checkout keeps its .git directory and is its own only root.
    assert_eq!(test_repository_roots(&main), vec![main.clone()]);
}

#[test]
fn a_fixture_home_passes_through_production_home_resolution() {
    home_fixture::run(
        "tests::test_repository_boundary::a_fixture_home_passes_through_production_home_resolution",
        |home| {
            assert_eq!(resolve_home_dir_from_environment().as_deref(), Some(home));
            assert_eq!(resolve_home_dir().as_deref(), Some(home));
            assert_eq!(
                resolve_termal_codex_discovery_root("unused-default-workdir"),
                home.join(".termal").join("codex-home")
            );
        },
    );
}

#[test]
fn a_home_outside_the_repository_does_not_resolve_under_test() {
    let outside = outside_the_repository("termal-real-home-stand-in");
    let mut command = boundary_child("outside-home");
    command
        .env("HOME", &outside)
        .env("USERPROFILE", &outside)
        .env_remove("CODEX_HOME");
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("outside home resolution");
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("outside home cases passed"), "{stderr}");
    assert!(
        !outside.exists(),
        "nothing may be created at {}",
        outside.display()
    );
}

/// HOME, USERPROFILE, TMP, TEMP and TMPDIR name sentinel directories this test
/// owns, and the user temp is left to its default. The child creates test
/// temporary trees, runs an Engram fixture child and prepares a TermAl Codex
/// home; all of it lands under the repository and the sentinels stay empty.
#[test]
fn a_test_run_leaves_the_home_and_the_platform_temp_untouched() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let sandbox = TestTempRoot::create("repository-boundary-sentinels");
    let home = sandbox.path().join("home");
    let temp = sandbox.path().join("temp");
    for sentinel in [&home, &temp] {
        fs::create_dir(sentinel).expect("sentinel");
    }
    let mut command = boundary_child("proof");
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TEMP", &temp)
        .env("TMP", &temp)
        .env("TMPDIR", &temp)
        .env_remove("TERMAL_TEST_USER_TEMP")
        .env_remove("TERMAL_TEST_RUN_ROOT")
        .env_remove("CODEX_HOME");
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("repository boundary proof");
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains("repository boundary cases passed"),
        "the child ran its cases: {stderr}"
    );
    for sentinel in [&home, &temp] {
        let left = fs::read_dir(sentinel)
            .expect("sentinel readable")
            .map(|entry| entry.expect("entry").path())
            .collect::<Vec<_>>();
        assert!(left.is_empty(), "{} received {left:?}", sentinel.display());
    }
}

#[test]
fn repository_boundary_child() {
    // Driven in a fresh process by the two tests above.
    match std::env::var(BOUNDARY_CHILD).as_deref() {
        Ok("proof") => sentinel_proof_cases(),
        Ok("outside-home") => outside_home_cases(),
        _ => {}
    }
}

fn outside_home_cases() {
    assert!(resolve_home_dir_from_environment().is_some());
    assert_eq!(resolve_home_dir(), None, "the real home must not resolve");
    assert!(
        resolve_source_codex_home_dir().is_err(),
        "no Codex home is seeded from the real home"
    );
    assert_eq!(
        resolve_termal_codex_discovery_root("default-workdir"),
        PathBuf::from("default-workdir")
            .join(".termal")
            .join("codex-home")
    );
    eprintln!("outside home cases passed");
}

fn sentinel_proof_cases() {
    let user_temp = manifest().join("target").join("tmp").join("termal");
    assert_eq!(test_user_temp(), user_temp);
    let tests_root = test_temp_dir();
    assert_eq!(tests_root, user_temp.join("termal").join("tests"));

    let fixture = TestTempRoot::create("repository-boundary-proof");
    assert!(
        test_path_inside_repository(fixture.path()),
        "{}",
        fixture.path().display()
    );
    // An Engram fixture child, with the environment the host gives it.
    let engram_home = fixture.path().join("engram-home");
    fs::create_dir(&engram_home).unwrap();
    let project_file = engram_home.join(".engram-project");
    fs::write(&project_file, "fixture-ready\n").unwrap();
    let mut command =
        engram_command(&super::engram_host_adapter::real_engram_control_fixture_path());
    for (name, value) in engram_test_child_temp_env(&engram_home) {
        command.env(name, value);
    }
    let output = command
        .arg("--project-file")
        .arg(&project_file)
        .arg("--home")
        .arg(&engram_home)
        .args(["readiness", "--json"])
        .output()
        .expect("fixture should run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A TermAl Codex home lands in the test tree.
    let scope = format!("repository-boundary-{}", Uuid::new_v4());
    let codex_home =
        prepare_termal_codex_home("unused-default-workdir", &scope).expect("Codex home");
    assert!(
        test_path_within(&codex_home, &tests_root),
        "{}",
        codex_home.display()
    );
    remove_test_directory(&codex_home);
    eprintln!("repository boundary cases passed");
}
