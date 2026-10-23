// Owns the regressions for an entry of the observed Git metadata trees
// (objects/ and refs/ of the own and common Git directory) that a concurrent
// Git writer removes while the review-identity observer walks them, and for
// an observer IO failure naming the path it could not read. Does not own the
// worktree discovery races (the parent module's discovery tests) or the
// observer process contract (src/review_identity_api.rs). New module, a child
// of src/tests/review_identity.rs, whose fixtures it uses.
use super::*;

/// Clears the metadata walk hook when the test ends.
struct WalkHook;
impl Drop for WalkHook {
    fn drop(&mut self) {
        IDENTITY_METADATA_WALK_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

/// Removes `target` (a file or a directory tree) the `nth` time the metadata
/// walk reaches `point`, as a concurrent Git writer finishing its temporary
/// entry would. A queued directory is reached first as a listed entry, then
/// before its link check, then before it is listed.
fn remove_at_walk_point(point: PathBuf, nth: usize, target: PathBuf) -> WalkHook {
    let mut reached = 0;
    IDENTITY_METADATA_WALK_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |at: &FsPath| {
            if at != point.as_path() {
                return;
            }
            reached += 1;
            if reached == nth {
                if target.is_dir() {
                    fs::remove_dir_all(&target).unwrap();
                } else {
                    fs::remove_file(&target).unwrap();
                }
            }
        }))
    });
    WalkHook
}

fn objects(root: &FsPath) -> PathBuf {
    fs::canonicalize(root.join(".git").join("objects")).unwrap()
}

/// A temporary object a concurrent Git writer removes after the walk listed
/// it is no failure: it cannot be a link or an escape any more. The capture
/// completes with the identity it has without that entry.
#[test]
fn a_git_object_removed_after_it_was_listed_does_not_fail_the_identity() {
    let root = local_root("review-identity-vanishing-object");
    init_git_document_test_repo(&root);
    let temporary = objects(&root).join("tmp_obj_vanishing");
    fs::write(&temporary, b"partial object").unwrap();
    let hook = remove_at_walk_point(temporary.clone(), 1, temporary.clone());
    let value = identity_result(&root, &capture_request())
        .expect("an entry removed mid-walk is no failure");
    drop(hook);
    assert_eq!(value["complete"], true);
    assert!(!temporary.exists(), "the walk reached the entry and it was removed");
    let unchanged = identity_result(&root, &capture_request()).unwrap();
    assert_eq!(value["fingerprint"], unchanged["fingerprint"], "the same identity");
}

/// A quarantine directory removed after it was queued is no failure either,
/// whether it is gone before its link check or only before it is listed.
#[test]
fn a_git_metadata_directory_removed_before_it_was_listed_does_not_fail_the_identity() {
    for (nth, step) in [(2, "before its link check"), (3, "before it is listed")] {
        let root = local_root("review-identity-vanishing-directory");
        init_git_document_test_repo(&root);
        let quarantine = objects(&root).join("incoming-vanishing");
        fs::create_dir(&quarantine).unwrap();
        fs::write(quarantine.join("pack"), b"partial pack").unwrap();
        let hook = remove_at_walk_point(quarantine.clone(), nth, quarantine.clone());
        let value = identity_result(&root, &capture_request())
            .unwrap_or_else(|error| panic!("removed {step}: {error:#}"));
        drop(hook);
        assert_eq!(value["complete"], true, "removed {step}");
        assert!(!quarantine.exists(), "the walk reached the directory {step}");
    }
}

/// The walked tree itself is no temporary: if objects/ vanishes, neither the
/// queued directories inside it nor the rest of its own listing quietly
/// vanish with it. The walk fails, naming the path it could not read.
#[test]
fn a_vanished_metadata_tree_fails_the_identity_naming_its_path() {
    for (name, nth, step) in [
        ("incoming-queued", 2, "before a queued directory's link check"),
        ("tmp_obj_listed", 1, "while its own listing is read"),
    ] {
        let root = local_root("review-identity-vanishing-tree");
        init_git_document_test_repo(&root);
        let objects = objects(&root);
        let entry = objects.join(name);
        if name.starts_with("incoming") {
            fs::create_dir(&entry).unwrap();
        } else {
            fs::write(&entry, b"partial object").unwrap();
        }
        let hook = remove_at_walk_point(entry, nth, objects.clone());
        let error = identity_result(&root, &capture_request()).unwrap_err();
        drop(hook);
        let message = format!("{error:#}");
        assert_eq!(identity_error_code(&error), "input_unavailable", "{step}: {message}");
        assert!(
            message.contains(&objects.display().to_string()),
            "{step}: the failure names the vanished tree: {message}"
        );
    }
}

/// Any other IO failure names the path it could not read, never a bare
/// "os error 2": a review root that does not exist, read directly and
/// through a parent's scoped capture admission.
#[test]
fn an_observer_io_failure_names_the_path_it_could_not_read() {
    let root = local_root("review-identity-missing-root");
    let missing = root.join("not-created");
    let error = identity_result(&missing, &capture_request()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(&missing.display().to_string()),
        "the failure names the missing path: {message}"
    );
    assert_eq!(identity_error_code(&error), "input_unavailable");

    let present = identity_utf8_path(&fs::canonicalize(&root).unwrap()).unwrap();
    let scope = ReviewIdentityScope {
        cwd: present.clone(),
        project: present,
        named: Vec::new(),
    };
    let error = scope.admit(&missing).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains(&missing.display().to_string()),
        "scoped admission names the missing path: {message}"
    );
    assert_eq!(identity_error_code(&error), "input_unavailable");
}
