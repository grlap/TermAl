// Owns the tests of the content revision (src/content_revision.rs): what
// moves it, what does not, and when it refuses. Does not own the review-freeze
// fingerprint's tests (src/tests/review_freeze.rs) or the tests of where a
// basis is taken (src/tests/engram_turn_observations.rs,
// src/tests/engram_turn_checks.rs). New module.
use super::*;

/// Removes the directories it owns when dropped, on success and on an
/// assertion's unwind alike; a failed removal is reported, never swallowed,
/// and does not stop the removal of the others.
struct RemoveDirsOnDrop(Vec<PathBuf>);

impl Drop for RemoveDirsOnDrop {
    fn drop(&mut self) {
        remove_test_directories(&self.0);
    }
}

/// A repository with one committed file, `tracked.txt`, and a `.gitignore`
/// that ignores `target/`. Line endings are left as written.
fn repository(cleanup: &mut RemoveDirsOnDrop) -> PathBuf {
    let root = test_temp_dir().join(format!("content-revision-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    cleanup.0.push(root.clone());
    init_git_document_test_repo(&root);
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&root, &["add", ".gitignore", "tracked.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "base"]);
    root
}

fn revision(root: &FsPath) -> String {
    content_revision(root)
        .expect("the content revision should be taken")
        .1
}

#[test]
fn a_content_revision_names_its_scheme_and_a_sha256() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let (canonical, value) = content_revision(&root).unwrap();
    assert_eq!(canonical, fs::canonicalize(&root).unwrap());
    let digest = value
        .strip_prefix("content-v1:")
        .expect("the revision names its scheme");
    assert!(is_lowercase_sha256(digest), "{value}");
    assert_eq!(
        revision(&root),
        value,
        "the same content gives the same revision"
    );
}

#[test]
fn staging_and_committing_do_not_move_the_content_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let base = revision(&root);

    fs::write(root.join("tracked.txt"), "changed\n").unwrap();
    let changed = revision(&root);
    assert_ne!(changed, base, "an edit moves it");
    run_git_test_command(&root, &["add", "tracked.txt"]);
    assert_eq!(revision(&root), changed, "staging does not");
    run_git_test_command(&root, &["commit", "--quiet", "-m", "changed"]);
    assert_eq!(revision(&root), changed, "committing does not");

    fs::write(root.join("new.txt"), "new\n").unwrap();
    let added = revision(&root);
    assert_ne!(added, changed, "a new untracked file moves it");
    run_git_test_command(&root, &["add", "new.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "new"]);
    assert_eq!(
        revision(&root),
        added,
        "tracking and committing it does not"
    );

    fs::remove_file(root.join("new.txt")).unwrap();
    let removed = revision(&root);
    assert_eq!(removed, changed, "a deleted tracked file has no entry");
    run_git_test_command(&root, &["rm", "--quiet", "new.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "remove"]);
    assert_eq!(
        revision(&root),
        removed,
        "committing the deletion does not move it"
    );

    run_git_test_command(&root, &["reset", "--quiet", "--hard", "HEAD~3"]);
    assert_eq!(
        revision(&root),
        base,
        "going back to the same content goes back to the same revision"
    );
}

#[test]
fn files_in_subdirectories_count_and_a_deleted_directory_leaves_no_entry() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let base = revision(&root);
    fs::create_dir_all(root.join("src/nested")).unwrap();
    fs::write(root.join("src/nested/tracked.rs"), "fn tracked() {}\n").unwrap();
    run_git_test_command(&root, &["add", "src/nested/tracked.rs"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "nested"]);
    let tracked = revision(&root);
    assert_ne!(tracked, base, "a nested tracked file counts");
    fs::write(root.join("src/untracked.rs"), "fn untracked() {}\n").unwrap();
    let untracked = revision(&root);
    assert_ne!(untracked, tracked, "a nested untracked file counts");
    fs::write(root.join("src/nested/tracked.rs"), "fn changed() {}\n").unwrap();
    assert_ne!(
        revision(&root),
        untracked,
        "an edit in a subdirectory moves it"
    );

    // The whole tree under src/ gone, a tracked file in it included: the
    // missing ancestor means absence, not a failed capture.
    fs::remove_dir_all(root.join("src")).unwrap();
    assert_eq!(revision(&root), base);
}

#[test]
fn a_force_added_ignored_file_counts_while_it_is_tracked() {
    // The index decides which ignored paths are listed.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/kept.txt"), "kept\n").unwrap();
    let ignored = revision(&root);
    run_git_test_command(&root, &["add", "--force", "target/kept.txt"]);
    let forced = revision(&root);
    assert_ne!(forced, ignored, "force-adding it lists it");
    run_git_test_command(&root, &["rm", "--quiet", "--cached", "target/kept.txt"]);
    assert_eq!(revision(&root), ignored, "untracking it unlists it again");
}

#[test]
fn ignored_files_do_not_move_the_content_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let base = revision(&root);
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/output.bin"), b"build output").unwrap();
    assert_eq!(revision(&root), base);
}

#[test]
fn a_peer_worktree_with_the_same_content_has_the_same_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let peer = test_temp_dir().join(format!("content-revision-peer-{}", Uuid::new_v4()));
    cleanup.0.push(peer.clone());
    run_git_test_command(
        &root,
        &["worktree", "add", "--detach", &peer.to_string_lossy()],
    );
    let (root_canonical, at_root) = content_revision(&root).unwrap();
    let (peer_canonical, at_peer) = content_revision(&peer).unwrap();
    assert_ne!(root_canonical, peer_canonical);
    assert_eq!(at_root, at_peer, "the worktree's location takes no part");
    fs::write(peer.join("tracked.txt"), "peer\n").unwrap();
    assert_ne!(revision(&peer), at_root);
}

#[test]
fn line_endings_of_a_text_file_do_not_move_the_content_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let lf = revision(&root);
    fs::write(root.join("tracked.txt"), "base\r\n").unwrap();
    assert_eq!(
        revision(&root),
        lf,
        "a CRLF copy of a text file is the same content"
    );
    fs::write(root.join("tracked.txt"), "base\r").unwrap();
    assert_ne!(revision(&root), lf, "a lone CR counts as it is");

    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(root.join("data.bin"), b"\0\r\n").unwrap();
    let binary = revision(&root);
    fs::write(root.join("data.bin"), b"\0\n").unwrap();
    assert_ne!(revision(&root), binary, "a binary file counts every byte");
}

#[test]
fn a_tracked_gitattributes_missing_from_the_worktree_leaves_no_content_revision() {
    // Git would take its attributes from the index, so staging the deletion
    // alone would change which files count byte for byte.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join(".gitattributes"), "*.dat -text\n").unwrap();
    fs::write(root.join("table.dat"), "a,b\r\n").unwrap();
    run_git_test_command(&root, &["add", ".gitattributes", "table.dat"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "attributes"]);
    fs::remove_file(root.join(".gitattributes")).unwrap();
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains(".gitattributes"), "{error}");
    // Once the deletion is staged, the index no longer supplies attributes.
    run_git_test_command(&root, &["rm", "--quiet", "--cached", ".gitattributes"]);
    assert!(content_revision(&root).is_ok());
}

#[test]
fn a_file_with_a_line_ending_contract_counts_byte_for_byte() {
    // A script rewritten from LF to CRLF after a passing check no longer
    // runs; with `eol=` or `-text` in .gitattributes the rewrite is a change.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(
        root.join(".gitattributes"),
        "*.sh text eol=lf\n*.dat -text\n*.raw binary\n",
    )
    .unwrap();
    fs::write(root.join("run.sh"), "#!/bin/sh\necho ok\n").unwrap();
    fs::write(root.join("table.dat"), "a,b\n").unwrap();
    fs::write(root.join("blob.raw"), "x\n").unwrap();
    run_git_test_command(&root, &["add", "."]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "contracts"]);
    let base = revision(&root);
    for (path, lf, crlf) in [
        ("run.sh", "#!/bin/sh\necho ok\n", "#!/bin/sh\r\necho ok\r\n"),
        ("table.dat", "a,b\n", "a,b\r\n"),
        ("blob.raw", "x\n", "x\r\n"),
    ] {
        fs::write(root.join(path), crlf).unwrap();
        assert_ne!(revision(&root), base, "{path}: a CRLF rewrite is a change");
        fs::write(root.join(path), lf).unwrap();
        assert_eq!(revision(&root), base, "{path}: restored");
    }
    // A file with no contract still reads CRLF as LF.
    fs::write(root.join("tracked.txt"), "base\r\n").unwrap();
    assert_eq!(revision(&root), base);
}

#[cfg(unix)]
#[test]
fn an_executable_file_counts_byte_for_byte() {
    use std::os::unix::fs::PermissionsExt;
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let path = root.join("tool");
    fs::write(&path, "#!/bin/sh\necho ok\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let lf = revision(&root);
    fs::write(&path, "#!/bin/sh\r\necho ok\r\n").unwrap();
    assert_ne!(revision(&root), lf, "its interpreter line would break");
}

#[test]
fn eol_listing_attributes_decide_which_files_keep_their_bytes() {
    assert!(content_revision_keeps_bytes(
        b"i/lf    w/lf    attr/text eol=lf      "
    ));
    assert!(content_revision_keeps_bytes(
        b"i/lf    w/crlf  attr/-text            "
    ));
    assert!(content_revision_keeps_bytes(
        b"i/      w/lf    attr/text eol=crlf    "
    ));
    assert!(!content_revision_keeps_bytes(
        b"i/lf    w/crlf  attr/                 "
    ));
    assert!(!content_revision_keeps_bytes(
        b"i/lf    w/crlf  attr/text=auto        "
    ));
    assert!(!content_revision_keeps_bytes(
        b"i/lf    w/crlf  attr/text             "
    ));
}

/// Git's object id of `path` as it would store it: as it is, or converted
/// the way `core.autocrlf` converts a file on the way in. Git runs pinned, as
/// the capture's own runner does: no GIT_* variable, no global or system
/// configuration and no global or system attributes, so a developer's setup
/// (a global `* -text`, say) cannot change Git's side of the comparison.
fn git_blob_id(repository: &FsPath, path: &str, autocrlf: bool) -> String {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut command = git_command();
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(name);
        }
    }
    command
        .current_dir(repository)
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .args(["-c", &format!("core.attributesFile={null}")]);
    if autocrlf {
        command.args([
            "-c",
            "core.autocrlf=true",
            "-c",
            "core.safecrlf=false",
            "hash-object",
            path,
        ]);
    } else {
        command.args(["hash-object", "--no-filters", path]);
    }
    let output = command.output().expect("git hash-object should run");
    assert!(
        output.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn line_ending_normalisation_converts_exactly_what_git_autocrlf_converts() {
    // Asks Git itself rather than restating its rule: for each fixture the
    // blob Git stores under core.autocrlf=true must be the blob of what the
    // revision hashes. The scratch repository has no .gitattributes and the
    // files are not in its index, so Git's index rule cannot apply.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let text = |count: usize| vec![b'a'; count];
    let with = |mut head: Vec<u8>, tail: &[u8]| {
        head.extend_from_slice(tail);
        head
    };
    let fixtures: Vec<(&str, Vec<u8>, bool)> = vec![
        // (name, raw bytes, whether Git converts it)
        ("crlf-text", b"a\r\nb\r\n".to_vec(), true),
        ("nul-first", b"\0a\r\n".to_vec(), false),
        ("nul-past-8000", with(text(9000), b"\0\r\n"), false),
        ("cr-cr-lf", b"a\r\r\nb\r\n".to_vec(), false),
        ("lone-cr", b"a\rb".to_vec(), false),
        // 128 printable against 1 non-printable is text, 127 is binary.
        ("ratio-text", with(with(text(128), b"\x01"), b"\r\n"), true),
        (
            "ratio-binary",
            with(with(text(127), b"\x01"), b"\r\n"),
            false,
        ),
        // A final ^Z is not counted as non-printable.
        ("final-ctrl-z", b"a\r\n\x1a".to_vec(), true),
        ("inner-ctrl-z", b"a\x1a\r\n".to_vec(), false),
        ("lf-only", b"a\nb\n".to_vec(), false),
        ("empty", Vec::new(), false),
    ];
    for (name, raw, converts) in fixtures {
        let raw_path = format!("fixture-{name}.raw");
        let ours_path = format!("fixture-{name}.ours");
        fs::write(root.join(&raw_path), &raw).unwrap();
        let ours = content_revision_line_endings(raw.clone());
        assert_eq!(
            ours != raw,
            converts,
            "{name}: the fixture's own expectation"
        );
        fs::write(root.join(&ours_path), &ours).unwrap();
        assert_eq!(
            git_blob_id(&root, &raw_path, true),
            git_blob_id(&root, &ours_path, false),
            "{name}: the revision reads the file as Git's autocrlf stores it"
        );
    }
}

#[test]
fn the_review_freeze_fingerprint_moves_where_the_content_revision_does_not() {
    // The control: staging and committing move the schema-1 fingerprint the
    // basis used to be, and leave the content revision where it was.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("tracked.txt"), "changed\n").unwrap();
    let git = ReviewFreezeGit::new(&root).unwrap();
    let (freeze, content) = (capture_review_freeze(&git).unwrap(), revision(&root));
    run_git_test_command(&root, &["add", "tracked.txt"]);
    let staged_freeze = capture_review_freeze(&ReviewFreezeGit::new(&root).unwrap()).unwrap();
    assert_ne!(staged_freeze, freeze, "staging moves the freeze");
    run_git_test_command(&root, &["commit", "--quiet", "-m", "changed"]);
    let committed_freeze = capture_review_freeze(&ReviewFreezeGit::new(&root).unwrap()).unwrap();
    assert_ne!(
        committed_freeze, staged_freeze,
        "committing moves the freeze"
    );
    assert_eq!(
        revision(&root),
        content,
        "neither moves the content revision"
    );
}

#[test]
fn a_content_revision_frames_paths_and_contents_unambiguously() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("a"), "bc").unwrap();
    let split_one_way = revision(&root);
    fs::remove_file(root.join("a")).unwrap();
    fs::write(root.join("ab"), "c").unwrap();
    assert_ne!(revision(&root), split_one_way);
}

#[test]
fn a_directory_replaced_by_a_file_counts_its_former_paths_as_absent() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::create_dir_all(root.join("module")).unwrap();
    fs::write(root.join("module/inner.rs"), "fn inner() {}\n").unwrap();
    run_git_test_command(&root, &["add", "module/inner.rs"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "module"]);
    fs::remove_dir_all(root.join("module")).unwrap();
    fs::write(root.join("module"), "now a file\n").unwrap();
    let replaced = revision(&root);

    // The same content reached without a tracked path under the file.
    run_git_test_command(&root, &["rm", "--quiet", "--cached", "module/inner.rs"]);
    assert_eq!(revision(&root), replaced);
}

#[test]
fn a_file_replaced_by_a_directory_counts_as_absent_and_its_files_count() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::remove_file(root.join("tracked.txt")).unwrap();
    fs::create_dir_all(root.join("tracked.txt")).unwrap();
    fs::write(root.join("tracked.txt/inside.txt"), "inside\n").unwrap();
    let replaced = content_revision(&root)
        .expect("a file turned into a directory does not fail the capture")
        .1;
    // The same content reached with the old file untracked.
    run_git_test_command(&root, &["rm", "--quiet", "--cached", "tracked.txt"]);
    assert_eq!(revision(&root), replaced);
}

#[test]
fn a_submodule_leaves_no_content_revision() {
    // What is inside a submodule is not read, so a change there would leave
    // the revision equal: the capture fails closed instead.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let head = run_git_test_command_output(&root, &["rev-parse", "HEAD"]);
    run_git_test_command(
        &root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},vendored", head.trim()),
        ],
    );
    fs::create_dir_all(root.join("vendored")).unwrap();
    let error = content_revision(&root).unwrap_err().to_string();
    assert!(error.contains("submodules"), "{error}");
    // Deinitialised, the gitlink still fails the capture.
    fs::remove_dir_all(root.join("vendored")).unwrap();
    assert!(content_revision(&root).is_err());
}

#[test]
fn a_capture_started_past_its_deadline_returns_no_content_revision() {
    // Covers a capture whose budget is spent before it starts. The check
    // after the loop, for a read that crosses the deadline, needs a slow read
    // this test cannot inject.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let mut git = ReviewFreezeGit::new(&root).unwrap();
    git.deadline = std::time::Instant::now();
    assert!(capture_content_revision(&git).is_err());
}

#[cfg(unix)]
#[test]
fn a_tracked_name_with_a_colon_or_backslash_counts_on_unix() {
    // Legal names on Unix: only Windows refuses them.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("a:b.txt"), "colon\n").unwrap();
    fs::write(root.join("c\\d.txt"), "backslash\n").unwrap();
    run_git_test_command(&root, &["add", "a:b.txt", "c\\d.txt"]);
    let with = revision(&root);
    fs::write(root.join("a:b.txt"), "changed\n").unwrap();
    assert_ne!(revision(&root), with);
}

#[test]
fn an_untracked_nested_repository_leaves_no_content_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    init_git_document_test_repo(&nested);
    fs::write(nested.join("file.txt"), "nested\n").unwrap();
    run_git_test_command(&nested, &["add", "file.txt"]);
    run_git_test_command(&nested, &["commit", "--quiet", "-m", "nested"]);
    assert!(content_revision(&root).is_err());
}

#[test]
fn a_content_revision_must_be_taken_at_the_worktree_root() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::create_dir_all(root.join("sub")).unwrap();
    assert!(content_revision(&root.join("sub")).is_err());
}

#[cfg(unix)]
#[test]
fn a_symlink_counts_as_its_link_text_and_the_executable_bit_counts() {
    use std::os::unix::fs::PermissionsExt;
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    std::os::unix::fs::symlink("tracked.txt", root.join("link")).unwrap();
    let linked = revision(&root);
    fs::remove_file(root.join("link")).unwrap();
    std::os::unix::fs::symlink("elsewhere.txt", root.join("link")).unwrap();
    assert_ne!(revision(&root), linked, "the link text is the content");

    let before = revision(&root);
    let path = root.join("tracked.txt");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(&path, permissions).unwrap();
    assert_ne!(
        revision(&root),
        before,
        "the executable bit is part of the entry"
    );
}

#[test]
fn a_bounded_capture_counts_live_threads_and_refuses_past_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let live = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let budget = Duration::from_millis(20);
    // Two captures that stall until released overrun their budget: nothing
    // is returned, and their threads stay counted while they live.
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (release, stalled) = std::sync::mpsc::channel::<()>();
        releases.push(release);
        let started = started.clone();
        let revision = bounded_content_revision_capture(&live, 2, budget, move || {
            started.fetch_add(1, Ordering::SeqCst);
            let _ = stalled.recv();
            Some("late".to_owned())
        });
        assert_eq!(revision, None, "an overrun capture declares nothing");
    }
    assert_eq!(live.load(Ordering::SeqCst), 2);
    // At the limit nothing new starts. The refused capture has its own
    // counter: the stalled ones may reach theirs at any moment.
    let refused_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refused = bounded_content_revision_capture(&live, 2, budget, {
        let refused_ran = refused_ran.clone();
        move || {
            refused_ran.store(true, Ordering::SeqCst);
            Some("never".to_owned())
        }
    });
    assert_eq!(refused, None);
    assert!(!refused_ran.load(Ordering::SeqCst), "no thread started");
    // Released, the threads end and are uncounted.
    drop(releases);
    wait_for_no_live_captures(&live);
    // A capture within its budget returns its revision and leaves no count.
    let taken = bounded_content_revision_capture(&live, 2, TEST_PHASE_DEADLOCK_GUARD, || {
        Some("content-v1:taken".to_owned())
    });
    assert_eq!(taken.as_deref(), Some("content-v1:taken"));
    wait_for_no_live_captures(&live);
}

#[test]
fn a_bounded_capture_that_panics_is_uncounted() {
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let revision: Option<String> =
        bounded_content_revision_capture(&live, 1, TEST_PHASE_DEADLOCK_GUARD, || {
            panic!("a capture that panics")
        });
    assert_eq!(revision, None);
    wait_for_no_live_captures(&live);
    // The one slot is free again.
    let taken = bounded_content_revision_capture(&live, 1, TEST_PHASE_DEADLOCK_GUARD, || {
        Some("content-v1:after".to_owned())
    });
    assert_eq!(taken.as_deref(), Some("content-v1:after"));
}

/// Waits until every capture thread counted in `live` has ended; its guard
/// lowers the count as the thread unwinds, just after it answers.
fn wait_for_no_live_captures(live: &Arc<std::sync::atomic::AtomicUsize>) {
    let deadline = std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD;
    while live.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "capture threads never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
