//! Test temporary-path containment and final-resource cleanup contracts.
//! Does not inspect or remove historical entries from the user's temp root.

use super::*;

fn age_test_directory(path: &FsPath) {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
        };
        options
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    options
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
        .unwrap();
}

#[test]
fn direct_helper_first_use_sweeps_stale_unmarked_fixtures_once() {
    let sandbox = TestTempRoot::create("termal-direct-sweep");
    let root = sandbox.path().join("termal").join("tests");
    fs::create_dir_all(&root).unwrap();
    let stale = root.join("legacy-fixture");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("evidence"), b"stale fixture").unwrap();
    age_test_directory(&stale);
    fs::write(root.join("stale-file"), b"stale fixture file").unwrap();
    age_test_directory(&root.join("stale-file"));
    fs::write(root.join("recent-file"), b"keep").unwrap();
    for name in ["recent-fixture", "run-with-marker", "invalid-marker"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    fs::write(
        root.join("run-with-marker/.termal-test-run"),
        b"{\"version\":1,\"pid\":2147483647}",
    )
    .unwrap();
    fs::write(root.join("invalid-marker/.termal-test-run"), b"null").unwrap();
    age_test_directory(&root.join("run-with-marker"));
    age_test_directory(&root.join("invalid-marker"));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::test_temp_paths::direct_helper_sweep_child",
            "--nocapture",
        ])
        .env("TERMAL_TEST_DIRECT_SWEEP", "1")
        .env("TERMAL_TEST_USER_TEMP", sandbox.path())
        .env_remove("TERMAL_TEST_RUN_ROOT");
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("direct-helper stale sweep");
    assert!(status.success(), "{stderr}");
    assert!(root.join("recent-file").exists());
    for name in ["recent-fixture", "run-with-marker", "invalid-marker"] {
        assert!(root.join(name).exists(), "retained {name}");
    }
}

#[test]
fn direct_helper_sweep_child() {
    // Driven in a fresh process by direct_helper_first_use_sweeps_stale_unmarked_fixtures_once.
    if std::env::var_os("TERMAL_TEST_DIRECT_SWEEP").is_none() {
        return;
    }
    let root = test_temp_dir();
    assert!(
        !root.join("stale-file").exists(),
        "first use must sweep stale plain files"
    );
    assert!(
        !root.join("legacy-fixture").exists(),
        "first direct-helper use must sweep stale fixtures"
    );
    let after_start = root.join("aged-after-start");
    fs::create_dir(&after_start).unwrap();
    age_test_directory(&after_start);
    assert_eq!(test_temp_dir(), root);
    assert!(
        after_start.exists(),
        "sweep runs once, never during later fixture creation"
    );
}

#[cfg(windows)]
#[test]
fn direct_helper_sweep_failure_preserves_original_diagnostic_without_retry() {
    let sandbox = TestTempRoot::create("termal-sweep-failure");
    let root = sandbox.path().join("termal").join("tests");
    fs::create_dir_all(&root).unwrap();
    let stale = root.join("locked-stale-file");
    fs::write(&stale, b"retained cleanup evidence").unwrap();
    age_test_directory(&stale);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::test_temp_paths::direct_helper_sweep_failure_child",
            "--nocapture",
        ])
        .env("TERMAL_TEST_SWEEP_FAILURE", "1")
        .env("TERMAL_TEST_USER_TEMP", sandbox.path())
        .env_remove("TERMAL_TEST_RUN_ROOT");
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("direct-helper cached sweep failure");
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains("cached sweep failure verified twice"),
        "{stderr}"
    );
    assert!(stale.exists(), "a failed startup sweep must not be retried");
}

#[cfg(windows)]
#[test]
fn direct_helper_sweep_failure_child() {
    // Re-executed by the parent regression so the process-wide sweep starts fresh.
    if std::env::var_os("TERMAL_TEST_SWEEP_FAILURE").is_none() {
        return;
    }
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    fn sweep_panic() -> String {
        let payload = std::panic::catch_unwind(test_temp_dir)
            .expect_err("startup sweep failure must fail every caller");
        if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            payload
                .downcast_ref::<&str>()
                .expect("text panic")
                .to_string()
        }
    }

    let root = PathBuf::from(std::env::var_os("TERMAL_TEST_USER_TEMP").unwrap())
        .join("termal")
        .join("tests");
    let stale = root.join("locked-stale-file");
    // Permit metadata reads, but deny deletion until this handle is dropped.
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&stale)
        .unwrap();
    let first = sweep_panic();
    drop(lock);
    assert!(first.contains(&root.display().to_string()), "{first}");
    assert!(first.contains(&stale.display().to_string()), "{first}");
    assert!(first.contains("os=Some(32)"), "{first}");
    let second = sweep_panic();
    assert!(!second.contains("poison"), "{second}");
    assert_eq!(second, first, "every caller must retain the original error");
    assert!(
        stale.exists(),
        "releasing the lock must not trigger a retry"
    );
    eprintln!("cached sweep failure verified twice");
}

#[test]
fn unmarked_sweep_is_bounded_and_does_not_descend_into_recent_roots() {
    let sandbox = TestTempRoot::create("termal-sweep-bound");
    for index in 0..65 {
        let path = sandbox.path().join(format!("fixture-{index}"));
        if index % 2 == 0 {
            fs::write(&path, b"stale file").unwrap();
        } else {
            fs::create_dir(&path).unwrap();
        }
        age_test_directory(&path);
    }
    let nested = sandbox.path().join("recent/nested-old");
    fs::create_dir_all(&nested).unwrap();
    age_test_directory(&nested);
    let removed =
        sweep_unmarked_test_fixtures(sandbox.path(), std::time::SystemTime::now()).unwrap();
    assert_eq!(removed.len(), 64);
    assert_eq!(fs::read_dir(sandbox.path()).unwrap().count(), 2);
    assert!(nested.exists());
}

#[cfg(unix)]
#[test]
fn unmarked_sweep_rejects_directory_links_including_the_sweep_root() {
    let sandbox = TestTempRoot::create("termal-sweep-links");
    let root = sandbox.path().join("tests");
    let outside = sandbox.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("evidence"), b"keep").unwrap();
    age_test_directory(&outside);
    let link = root.join("linked-fixture");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(
        sweep_unmarked_test_fixtures(&root, std::time::SystemTime::now())
            .unwrap()
            .is_empty()
    );
    assert!(sweep_unmarked_test_fixtures(&link, std::time::SystemTime::now()).is_err());
    assert!(outside.join("evidence").exists());
}

#[test]
fn checked_directory_cleanup_reports_path_and_os_error_without_deleting_a_file() {
    let sandbox = TestTempRoot::create("termal-checked-removal");
    let path = sandbox.path().join("not-directory");
    fs::write(&path, b"evidence").unwrap();
    let failure = std::panic::catch_unwind(|| remove_test_directory(&path)).unwrap_err();
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains(&path.display().to_string()));
    assert!(message.contains("os="));
    assert!(path.exists());
    remove_test_directory(sandbox.path().join("already-absent"));
}

#[test]
fn checked_directory_cleanup_does_not_double_panic_during_assertion_unwind() {
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            remove_test_directory(&self.0);
        }
    }
    let sandbox = TestTempRoot::create("termal-checked-unwind");
    let path = sandbox.path().join("not-directory");
    fs::write(&path, b"evidence").unwrap();
    let failure = std::panic::catch_unwind(|| {
        let _cleanup = Cleanup(path.clone());
        panic!("original assertion");
    })
    .unwrap_err();
    assert_eq!(failure.downcast_ref::<&str>(), Some(&"original assertion"));
    assert!(path.exists());
}

#[test]
fn direct_cargo_paths_stay_under_the_product_directory() {
    let user_temp = std::env::current_dir().unwrap().join("synthetic-user-temp");
    assert_eq!(
        resolve_test_temp_directory(&user_temp, None).unwrap(),
        user_temp.join("termal").join("tests")
    );
}

#[test]
fn wrapper_paths_reuse_the_run_directory_without_nesting_another_product_root() {
    let user_temp = std::env::current_dir().unwrap().join("synthetic-user-temp");
    let run = user_temp.join("termal").join("tests").join("run-contract");
    assert_eq!(
        resolve_test_temp_directory(&user_temp, Some(&run)).unwrap(),
        run
    );
    for invalid in [
        user_temp.clone(),
        user_temp.join("run-outside"),
        run.join("nested"),
        user_temp.join("termal").join("tests").join("arbitrary"),
        run.join("..").join("run-other"),
    ] {
        assert!(resolve_test_temp_directory(&user_temp, Some(&invalid)).is_err());
    }
    assert!(resolve_test_temp_directory(FsPath::new("relative"), None).is_err());
}

#[test]
fn poisoned_cleanup_observers_do_not_panic_or_lose_the_removal_receipt() {
    let root = TestTempRoot::create("termal-poisoned-cleanup");
    let path = root.path().to_owned();
    let cleaned = root.observe_cleanup();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _observers = root.cleanup_observers.lock().expect("observer mutex");
        panic!("injected observer poison");
    }));
    assert!(poisoned.is_err());
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(root)));
    assert!(
        dropped.is_ok(),
        "cleanup must survive an already reported poison"
    );
    phase_sync::receive(&cleaned, "poisoned root cleanup receipt").unwrap();
    assert!(!path.exists());
}

#[test]
fn unexpected_release_panic_during_unwind_still_drops_the_fixture_state() {
    let state = test_app_state();
    let path = state.test_temp_root_path().unwrap().to_owned();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _fixture = TestAppStateCleanup::new(state, "panicking release", || {
            panic!("injected unexpected release panic");
        });
        panic!("original fixture assertion");
    }));
    let payload = unwound.expect_err("original assertion must still fail");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"original fixture assertion")
    );
    assert!(!path.exists());
}

#[test]
fn guarded_temp_roots_are_contained_and_removed_before_the_cleanup_receipt() {
    let root = TestTempRoot::create("termal-temp-contract");
    let path = root.path().to_owned();
    assert_eq!(path.parent(), Some(test_temp_dir().as_path()));
    fs::write(path.join("evidence"), b"fixture").unwrap();
    let cleaned = root.observe_cleanup();
    drop(root);
    phase_sync::receive(&cleaned, "test root final cleanup").unwrap();
    assert!(!path.exists());
}

#[test]
fn cleanup_errors_are_reported_to_the_fixture_owner_with_the_path() {
    let parent = TestTempRoot::create("termal-cleanup-error");
    let path = parent.path().join("not-a-directory");
    fs::write(&path, b"fixture").unwrap();
    let root = TestTempRoot::own(path.clone());
    let cleaned = root.observe_cleanup();
    drop(root);
    let error = phase_sync::receive(&cleaned, "failed test root cleanup").unwrap_err();
    assert!(error.contains(&path.display().to_string()));
    assert!(error.contains("os="));
    assert!(
        path.exists(),
        "failure must not be hidden by a fallback deletion"
    );
}
