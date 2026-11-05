// Owns the tests of the committed-input switch (src/ordinary_commit_input.rs):
// that host and launcher defaults are independent, that off it leaves the content revision
// untouched, and that on it gives a clean root its HEAD commit and a root that
// is not clean a typed not-clean outcome and no basis. Does not own the
// content revision's own tests (src/tests/content_revision.rs) or those of
// where a basis is taken (src/tests/engram_source_roots.rs,
// src/tests/engram_turn_observations.rs). New module.
use super::*;

/// Removes the directories it owns when dropped, on success and on an
/// assertion's unwind alike; a failed removal is reported, never swallowed.
struct RemoveDirsOnDrop(Vec<PathBuf>);

impl Drop for RemoveDirsOnDrop {
    fn drop(&mut self) {
        remove_test_directories(&self.0);
    }
}

/// A repository with one committed file, `tracked.txt`, and a `.gitignore`
/// that ignores `target/`.
fn repository(cleanup: &mut RemoveDirsOnDrop) -> PathBuf {
    let root = test_temp_dir().join(format!("ordinary-commit-input-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    cleanup.0.push(root.clone());
    init_git_document_test_repo(&root);
    fs::write(root.join(".gitignore"), "target/\n").unwrap();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&root, &["add", ".gitignore", "tracked.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "base"]);
    root
}

fn head(root: &FsPath) -> String {
    run_git_test_command_output(root, &["rev-parse", "HEAD"])
}

fn workdir_place(root: &FsPath) -> EngramBasisPlace {
    EngramBasisPlace::Workdir(root.to_string_lossy().into_owned())
}

/// The dirty paths of a not-clean outcome; panics on a revision.
fn not_clean(root: &FsPath) -> Vec<SourceRevisionDirtyPath> {
    match source_revision(root, true).expect("the outcome should be taken") {
        SourceRevision::NotClean { dirty_paths } => dirty_paths,
        taken => panic!("a root that is not clean should have no revision: {taken:?}"),
    }
}

#[test]
fn the_launcher_selects_automatically_while_the_host_source_default_stays_off() {
    assert!(!ORDINARY_COMMIT_INPUT_DEFAULT, "ordinary host measurements retain content-v1");
    let launcher = fs::read_to_string(
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("test-launcher.mjs"),
    )
    .expect("the launcher should be readable");
    let declared = launcher
        .lines()
        .filter(|line| line.contains("const ORDINARY_COMMIT_INPUT_DEFAULT"))
        .collect::<Vec<_>>();
    assert_eq!(declared.len(), 1, "the launcher declares the switch once");
    assert_eq!(
        declared[0].trim_end(),
        "export const ORDINARY_COMMIT_INPUT_DEFAULT = true;",
        "launcher selection is independent of ordinary host measurements"
    );
}

#[test]
fn with_the_switch_off_the_revision_is_the_content_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    // Not clean, on purpose: off, that changes nothing.
    fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
    let (canonical, expected) = content_revision(&root).unwrap();
    assert!(expected.starts_with("content-v1:"), "{expected}");
    assert_eq!(
        source_revision(&root, false).unwrap(),
        SourceRevision::Taken {
            root: canonical,
            revision: expected.clone(),
        }
    );
    let place = workdir_place(&root);
    let basis = engram_place_source_basis_with(&place, false)
        .expect("a content revision gives a basis");
    assert_eq!(basis.source_revision, expected);
    if !ORDINARY_COMMIT_INPUT_DEFAULT {
        assert_eq!(engram_place_source_basis(&place), Some(basis));
    }
}

#[test]
fn with_the_switch_on_a_clean_root_has_its_head_commit() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let expected = format!("git-commit:{}", head(&root));
    assert_eq!(
        source_revision(&root, true).unwrap(),
        SourceRevision::Taken {
            root: fs::canonicalize(&root).unwrap(),
            revision: expected.clone(),
        }
    );
    let basis = engram_place_source_basis_with(&workdir_place(&root), true)
        .expect("a clean root has a basis");
    assert_eq!(basis.source_revision, expected);
    // A new commit moves it to the new HEAD.
    fs::write(root.join("tracked.txt"), "next\n").unwrap();
    run_git_test_command(&root, &["commit", "--quiet", "-am", "next"]);
    let next = format!("git-commit:{}", head(&root));
    assert_ne!(next, expected);
    assert_eq!(
        source_revision(&root, true).unwrap(),
        SourceRevision::Taken {
            root: fs::canonicalize(&root).unwrap(),
            revision: next,
        }
    );
}

#[test]
fn with_the_switch_on_an_untracked_file_is_not_clean() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/new.rs"), "fn new() {}\n").unwrap();
    assert_eq!(
        not_clean(&root),
        vec![SourceRevisionDirtyPath {
            status: "??".to_owned(),
            path: "src/new.rs".to_owned(),
            origin: None,
        }],
        "every untracked file is listed by its own path"
    );
    assert!(
        engram_place_source_basis_with(&workdir_place(&root), true).is_none(),
        "a root that is not clean has no basis, never a content revision"
    );
}

#[test]
fn with_the_switch_on_a_modified_tracked_file_is_not_clean() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::write(root.join("tracked.txt"), "changed\n").unwrap();
    assert_eq!(
        not_clean(&root),
        vec![SourceRevisionDirtyPath {
            status: " M".to_owned(),
            path: "tracked.txt".to_owned(),
            origin: None,
        }]
    );
    run_git_test_command(&root, &["add", "tracked.txt"]);
    assert_eq!(
        not_clean(&root)[0].status,
        "M ",
        "a staged change is not committed either"
    );
    assert!(engram_place_source_basis_with(&workdir_place(&root), true).is_none());
}

#[test]
fn with_the_switch_on_a_named_root_that_is_not_clean_has_no_basis() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let workdir = root.to_string_lossy().into_owned();
    let (named, common_dir_key) = validate_engram_source_root(&workdir, &workdir, &workdir)
        .expect("the main checkout may be named");
    let place = EngramBasisPlace::Named {
        root: named,
        common_dir_key,
    };
    let clean = engram_place_source_basis_with(&place, true).expect("a clean root has a basis");
    assert_eq!(clean.source_revision, format!("git-commit:{}", head(&root)));
    fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
    assert!(engram_place_source_basis_with(&place, true).is_none());
}

#[test]
fn with_the_switch_on_ignored_files_leave_the_root_clean() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/output.bin"), b"build output").unwrap();
    assert_eq!(
        source_revision(&root, true).unwrap(),
        SourceRevision::Taken {
            root: fs::canonicalize(&root).unwrap(),
            revision: format!("git-commit:{}", head(&root)),
        }
    );
}

#[test]
fn with_the_switch_on_a_file_git_is_told_to_skip_gives_no_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["update-index", "--assume-unchanged", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "hidden edit\n").unwrap();
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("assume-unchanged"), "{error}");
}

#[test]
fn status_entries_keep_a_rename_origin() {
    let parsed = source_revision_dirty_paths(b"R  new.txt\0old.txt\0?? u.txt\0").unwrap();
    assert_eq!(
        parsed,
        vec![
            SourceRevisionDirtyPath {
                status: "R ".to_owned(),
                path: "new.txt".to_owned(),
                origin: Some("old.txt".to_owned()),
            },
            SourceRevisionDirtyPath {
                status: "??".to_owned(),
                path: "u.txt".to_owned(),
                origin: None,
            },
        ]
    );
    assert!(source_revision_dirty_paths(b"R  new.txt\0").is_err());
    assert!(source_revision_dirty_paths(b"bad\0").is_err());
}

#[test]
fn with_the_switch_on_a_skip_worktree_file_gives_no_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["update-index", "--skip-worktree", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "hidden edit\n").unwrap();
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("skip-worktree"), "{error}");
    assert_eq!(committed_source_clean_head(&root), None);
}

#[test]
fn with_the_switch_on_a_submodule_gitlink_gives_no_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    // A committed gitlink with no checkout: an unpopulated submodule is an
    // empty directory at its path (a missing path reads as deleted), and
    // plain status calls that clean, so only the gitlink refusal keeps the
    // root from a revision.
    let cacheinfo = format!("160000,{},sub", head(&root));
    run_git_test_command(&root, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "gitlink"]);
    fs::create_dir_all(root.join("sub")).unwrap();
    assert_eq!(run_git_test_command_output(&root, &["status", "--porcelain"]), "");
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("gitlinks are unsupported"), "{error}");
    assert!(error.ends_with(": sub"), "{error}");
    assert_eq!(committed_source_clean_head(&root), None);
    assert!(engram_place_source_basis_with(&workdir_place(&root), true).is_none());
}

#[test]
fn with_the_switch_on_gitignore_and_info_exclude_leave_an_untracked_file_clean() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    // Ignored by the repository's own .gitignore: clean.
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/output.bin"), b"build output").unwrap();
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));

    // Listed only in .git/info/exclude: clean, as plain status says.
    let info = root.join(".git").join("info");
    fs::create_dir_all(&info).unwrap();
    fs::write(info.join("exclude"), "excluded.txt\n").unwrap();
    fs::write(root.join("excluded.txt"), "hidden by info/exclude\n").unwrap();
    assert_eq!(
        source_revision(&root, true).unwrap(),
        SourceRevision::Taken {
            root: fs::canonicalize(&root).unwrap(),
            revision: format!("git-commit:{}", head(&root)),
        }
    );
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));
    fs::remove_file(root.join("excluded.txt")).unwrap();

    // Listed only in an excludes file the repository configures: dirty, since
    // core.excludesFile is disabled on both sides.
    let excludes = root.join("target").join("excludes");
    fs::write(&excludes, "configured.txt\n").unwrap();
    run_git_test_command(
        &root,
        &["config", "core.excludesFile", excludes.to_str().unwrap()],
    );
    fs::write(root.join("configured.txt"), "hidden by excludesFile\n").unwrap();
    assert_eq!(
        not_clean(&root),
        vec![SourceRevisionDirtyPath {
            status: "??".to_owned(),
            path: "configured.txt".to_owned(),
            origin: None,
        }]
    );
    assert_eq!(committed_source_clean_head(&root), None);
}
