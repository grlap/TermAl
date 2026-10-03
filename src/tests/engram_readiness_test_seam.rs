// Tests of the readiness test seam (src/engram_readiness_test_seam.rs): the
// staged receipt is the one the control fixture prints, a staged home is
// answered without a process, and an expired launch fails readiness with the
// budget it enforced, so the tests that hold both hooks are not passing on a
// hook that does nothing. New file, not split from another.
use super::*;

fn fixture_ready_root(prefix: &str) -> (TestTempRoot, PathBuf) {
    let root = TestTempRoot::create(prefix);
    let marker = root.path().join(".engram-project");
    fs::write(&marker, "fixture-ready\n").expect("fixture declaration should be written");
    (root, marker)
}

fn admitted_store_key(receipt: &Value, marker: &FsPath, home: &FsPath) -> EngramAuthorityStoreKey {
    let receipt: EngramReadinessReceipt =
        serde_json::from_value(receipt.clone()).expect("readiness receipt should deserialize");
    validate_engram_readiness(&receipt, marker, home, true)
        .unwrap_or_else(|error| panic!("readiness receipt should be admitted: {}", error.message))
}

#[test]
fn staged_readiness_receipt_is_the_one_the_control_fixture_prints() {
    let (fixture_root, fixture_marker) = fixture_ready_root("readiness-seam-fixture");
    let binary = super::engram_host_adapter::real_engram_control_fixture_path();
    // The real fixture runs to completion without the product's budget: this
    // test compares receipts and does not time the launch.
    let output = engram_command(&binary)
        .arg("--project-file")
        .arg(&fixture_marker)
        .arg("--home")
        .arg(fixture_root.path())
        .args(["readiness", "--json"])
        .current_dir(fixture_root.path())
        .stdin(Stdio::null())
        .output()
        .expect("control fixture should run");
    assert!(
        output.status.success(),
        "control fixture readiness failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let real: Value =
        serde_json::from_slice(&output.stdout).expect("fixture receipt should be JSON");

    let (staged_root, staged_marker) = fixture_ready_root("readiness-seam-staged");
    let staged = staged_test_engram_readiness_receipt(&staged_marker, staged_root.path())
        .unwrap_or_else(|error| panic!("staged receipt should build: {}", error.message));
    assert!(staged.status.success());
    let staged: Value =
        serde_json::from_slice(&staged.stdout).expect("staged receipt should be JSON");

    // Every field but the store path is the same...
    let without_database = |receipt: &Value| {
        let mut receipt = receipt.clone();
        receipt
            .as_object_mut()
            .expect("receipt should be an object")
            .remove("database")
            .expect("receipt should name its database");
        receipt
    };
    assert_eq!(without_database(&staged), without_database(&real));
    // ...and each store path is its own home's store, as the production
    // validator admits it.
    for (receipt, marker, home) in [
        (&real, &fixture_marker, fixture_root.path()),
        (&staged, &staged_marker, staged_root.path()),
    ] {
        let expected = normalize_user_facing_path(
            &fs::canonicalize(work_database_path(home, "fixture-ready"))
                .expect("the receipt's store file should exist"),
        );
        assert_eq!(
            admitted_store_key(receipt, marker, home),
            EngramAuthorityStoreKey {
                database_path: expected,
                project_id: "fixture-ready".to_owned(),
            }
        );
    }
}

#[test]
fn a_staged_home_is_answered_without_a_process() {
    let (root, marker) = fixture_ready_root("readiness-seam-no-process");
    let binary = super::engram_host_adapter::real_engram_control_fixture_path();
    let _seam = stage_test_engram_readiness_without_launch(&[root.path()]);

    let receipt = run_engram_readiness(&binary, &marker, root.path(), root.path())
        .unwrap_or_else(|error| panic!("staged readiness should succeed: {}", error.message));

    validate_engram_readiness(&receipt, &marker, root.path(), true)
        .unwrap_or_else(|error| panic!("staged receipt should be admitted: {}", error.message));
    assert!(
        !root.path().join("diagnostic-commands").exists(),
        "the control fixture records every diagnostic it runs; it must not have run"
    );
}

/// The witness the grant and quarantine tests' staging answers. Their setup
/// Saves, made exactly as those tests make them but WITHOUT staged readiness,
/// fail on setup with the product's readiness budget once the fixture launch
/// outlasts it: the failure a stalled machine causes, reproduced here by the
/// expired-launch seam instead of by load. The three tests
/// (engram_mcp_rechecks_retired_grants_under_the_commit_lock_across_projects,
/// engram_mcp_rechecks_active_grant_ownership_under_the_commit_lock and the
/// quarantine-transition tests) register staged readiness, so they never take
/// this path.
#[test]
fn unstaged_grant_and_quarantine_setups_fail_on_the_readiness_budget_when_a_launch_expires() {
    for (label, grant) in [
        ("retired-grant-race", Some("grant-race")),
        ("active-grant-race", None),
        ("quarantine-transition", Some("grant-old")),
    ] {
        let state = test_app_state();
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("readiness-witness-{label}"));
        fs::create_dir_all(&root).expect("project root should exist");
        fs::write(root.join(".engram-project"), "fixture-ready\n")
            .expect("fixture declaration should be written");
        let _seam = expire_test_engram_diagnostic_launches(&[&root]);
        let project_id = create_test_project(&state, &root, label);
        let mut settings = super::engram_host_adapter::real_fixture_engram_settings(&root);
        settings.work_authority_grant = grant.map(str::to_owned);

        let error = match state.update_project_engram_settings(&project_id, settings) {
            Ok(_) => panic!("{label}: an expired setup launch must not enable Engram"),
            Err(error) => error,
        };

        assert!(
            error
                .message
                .contains("Engram readiness exceeded the 20 second enablement deadline"),
            "{label}: the setup must fail on the product's readiness budget: {}",
            error.message
        );
    }
}

#[test]
fn an_expired_launch_fails_readiness_with_the_budget_it_enforced() {
    let (root, marker) = fixture_ready_root("readiness-seam-expired");
    let _seam = expire_test_engram_diagnostic_launches(&[root.path()]);
    // A process that holds nothing in the temp root: a small system binary,
    // started outside the root, which rejects these arguments and may exit at
    // once (on Unix it is not held suspended). The hook expires its launch as
    // soon as it resumes, whether or not it has already exited. Not this test
    // binary: the first launch of a freshly linked executable is slow and
    // holds the shared launch lock.
    let binary = if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set on Windows"))
            .join("System32")
            .join("cmd.exe")
    } else {
        PathBuf::from("/bin/sh")
    };
    let outside_root = FsPath::new(env!("CARGO_MANIFEST_DIR"));

    let error = match run_engram_readiness(&binary, &marker, root.path(), outside_root) {
        Ok(_) => panic!("an expired launch must not produce a readiness receipt"),
        Err(error) => error,
    };

    assert!(
        error
            .message
            .contains("Engram readiness exceeded the 20 second enablement deadline"),
        "an expired launch must fail on the product's readiness budget: {}",
        error.message
    );
}
