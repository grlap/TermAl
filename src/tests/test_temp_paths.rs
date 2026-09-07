//! Test temporary-path containment and final-resource cleanup contracts.
//! Does not inspect or remove historical entries from the user's temp root.

use super::*;

#[test]
fn direct_cargo_paths_stay_under_the_product_directory() {
    let user_temp = std::env::temp_dir();
    assert_eq!(
        resolve_test_temp_directory(&user_temp, None).unwrap(),
        user_temp.join("termal").join("tests")
    );
}

#[test]
fn wrapper_paths_reuse_the_run_directory_without_nesting_another_product_root() {
    let user_temp = std::env::temp_dir();
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
