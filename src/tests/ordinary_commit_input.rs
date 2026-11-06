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
fn assume_unchanged_edits_are_a_documented_status_limitation() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    run_git_test_command(&root, &["update-index", "--assume-unchanged", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "hidden edit\n").unwrap();
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));
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
fn with_the_switch_on_an_uninitialized_submodule_gives_no_revision() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    // A committed gitlink with no checkout: an unpopulated submodule is an
    // empty directory at its path (a missing path reads as deleted), and
    // plain status calls that clean, so the initialization guard keeps the
    // root from a revision.
    let cacheinfo = format!("160000,{},sub", head(&root));
    run_git_test_command(&root, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "gitlink"]);
    fs::create_dir_all(root.join("sub")).unwrap();
    assert_eq!(run_git_test_command_output(&root, &["status", "--porcelain"]), "");
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("submodule initialization/pin"), "{error}");
    assert_eq!(committed_source_clean_head(&root), None);
    assert!(engram_place_source_basis_with(&workdir_place(&root), true).is_none());
}

fn repository_with_child(cleanup: &mut RemoveDirsOnDrop) -> (PathBuf, PathBuf) {
    let dependency = repository(cleanup);
    let root = repository(cleanup);
    run_git_test_command(&root, &["-c", "core.autocrlf=false", "-c", "protocol.file.allow=always", "submodule", "add", "--quiet",
        dependency.to_str().unwrap(), "child with spaces"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "pin child"]);
    let child = root.join("child with spaces");
    run_git_test_command(&child, &["config", "core.autocrlf", "false"]);
    (root, child)
}

#[cfg(windows)]
#[test]
fn windows_long_child_paths_are_measured_without_persistent_config_changes() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    // Fixture-local false overrides any developer-machine setting. Only the
    // setup commands opt in; the production measurement must do so itself.
    for directory in [&root, &child] {
        run_git_test_command(directory, &["config", "core.longpaths", "false"]);
    }
    let mut relative = PathBuf::from("deep");
    while child.join(&relative).join("tracked.txt").as_os_str().len() <= 280 {
        relative.push("long-path-component");
    }
    fs::create_dir_all(child.join(&relative)).unwrap();
    let tracked = child.join(&relative).join("tracked.txt");
    fs::write(&tracked, "long-path baseline\n").unwrap();
    let tracked_relative = relative.join("tracked.txt");
    run_git_test_command(&child, &["-c", "core.longpaths=true", "add", "--", tracked_relative.to_str().unwrap()]);
    run_git_test_command(&child, &["-c", "core.longpaths=true", "-c", "user.name=Fixture",
        "-c", "user.email=test@example.invalid", "commit", "--quiet", "-m", "long child path"]);
    run_git_test_command(&root, &["-c", "core.longpaths=true", "add", "--", "child with spaces"]);
    run_git_test_command(&root, &["-c", "core.longpaths=true", "commit", "--quiet", "-m", "pin long child"]);
    let root_config = root.join(".git/config");
    let child_config = PathBuf::from(run_git_test_command_output(&child,
        &["rev-parse", "--absolute-git-dir"])).join("config");
    let before = [fs::read(&root_config).unwrap(), fs::read(&child_config).unwrap()];
    let original = head(&root);
    assert_eq!(source_revision(&root, true).unwrap(), SourceRevision::Taken {
        root: fs::canonicalize(&root).unwrap(), revision: format!("git-commit:{original}"),
    }, "a clean child beyond MAX_PATH must not become apparent dirt");
    assert_eq!(committed_source_clean_head(&root), Some(original.clone()));

    fs::write(&tracked, "actual child change\n").unwrap();
    assert_eq!(fs::read(&tracked).unwrap(), b"actual child change\n");
    eprintln!("original long-path diagnostic relative_chars={} absolute_chars={} bytes={} modified={:?}",
        tracked_relative.as_os_str().len(), tracked.as_os_str().len(), fs::metadata(&tracked).unwrap().len(),
        fs::metadata(&tracked).unwrap().modified());
    for (place, directory) in [("root", &root), ("child", &child)] {
        let git = ReviewFreezeGit::new(directory).unwrap();
        for setting in ["core.longpaths=false", "core.longpaths=true"] {
            let mut command = git.command(&["-c", setting, "-c", "status.showUntrackedFiles=all",
                "-c", "diff.ignoreSubmodules=none", "status", "--porcelain=v1",
                "--untracked-files=all", "--ignore-submodules=none", "-z"]);
            let output = run_bounded_read_command(&mut command, git.deadline, REVIEW_FREEZE_OUTPUT_LIMIT, false).unwrap();
            eprintln!("original long-path diagnostic place={place} {setting} code={:?} stdout={:?} stderr={:?}",
                output.status.code(), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        }
    }
    let dirty = not_clean(&root);
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].path, "child with spaces");
    assert_eq!(committed_source_clean_head(&root), None);
    fs::write(&tracked, "long-path baseline\n").unwrap();
    assert_eq!(committed_source_clean_head(&root), Some(original.clone()));

    let untracked = child.join(&relative).join("untracked.txt");
    fs::write(&untracked, "untracked child input\n").unwrap();
    assert_eq!(not_clean(&root)[0].path, "child with spaces");
    assert_eq!(committed_source_clean_head(&root), None);
    fs::remove_file(untracked).unwrap();
    assert_eq!(committed_source_clean_head(&root), Some(original));
    assert_eq!(fs::read(root_config).unwrap(), before[0]);
    assert_eq!(fs::read(child_config).unwrap(), before[1]);
}

#[cfg(windows)]
#[test]
fn windows_long_path_status_diagnostic_distinguishes_config_from_stat_cache() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    for directory in [&root, &child] {
        run_git_test_command(directory, &["config", "core.longpaths", "false"]);
    }
    let relative = PathBuf::from("a".repeat(96)).join("b".repeat(96)).join("c".repeat(96)).join("tracked.txt");
    assert!(relative.as_os_str().len() > 260);
    let tracked = child.join(&relative);
    fs::create_dir_all(tracked.parent().unwrap()).unwrap();
    fs::write(&tracked, "base\n").unwrap();
    run_git_test_command(&child, &["-c", "core.longpaths=true", "add", "--", relative.to_str().unwrap()]);
    run_git_test_command(&child, &["-c", "core.longpaths=true", "-c", "user.name=Fixture",
        "-c", "user.email=test@example.invalid", "commit", "--quiet", "-m", "long relative path"]);
    run_git_test_command(&root, &["-c", "core.longpaths=true", "add", "--", "child with spaces"]);
    run_git_test_command(&root, &["-c", "core.longpaths=true", "commit", "--quiet", "-m", "pin long relative path"]);
    let mut measurements = Vec::new();
    for (state, contents) in [("clean", "base\n"), ("dirty", "a visibly different-size child change\n")] {
        fs::write(&tracked, contents).unwrap();
        assert_eq!(fs::read(&tracked).unwrap(), contents.as_bytes());
        eprintln!("long-path diagnostic state={state} relative_chars={} absolute_chars={} bytes={} modified={:?}",
            relative.as_os_str().len(), tracked.as_os_str().len(), contents.len(), fs::metadata(&tracked).unwrap().modified());
        for (place, directory) in [("root", &root), ("child", &child)] {
            let git = ReviewFreezeGit::new(directory).unwrap();
            for setting in ["core.longpaths=false", "core.longpaths=true"] {
                let mut command = git.command(&["-c", setting, "-c", "status.showUntrackedFiles=all",
                    "-c", "diff.ignoreSubmodules=none", "status", "--porcelain=v1",
                    "--untracked-files=all", "--ignore-submodules=none", "-z"]);
                let output = run_bounded_read_command(&mut command, git.deadline, REVIEW_FREEZE_OUTPUT_LIMIT, false).unwrap();
                eprintln!("long-path diagnostic state={state} place={place} {setting} code={:?} stdout={:?} stderr={:?}",
                    output.status.code(), String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                if setting.ends_with("=true") {
                    assert!(output.status.success(), "enabled Git must read the fixture");
                    assert_eq!(output.stdout.is_empty(), state == "clean", "enabled {place} status must distinguish actual dirt");
                    assert!(output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
                } else {
                    // Reproduce Git's exit-zero partial read. The raw control
                    // deliberately overrides the production long-path option;
                    // the existing checked reader must refuse the same read.
                    assert!(output.status.success());
                    assert!(output.stdout.is_empty());
                    assert!(String::from_utf8_lossy(&output.stderr).contains("Filename too long"));
                    let error = git.run(&["-c", setting, "status", "--porcelain=v1",
                        "--untracked-files=all", "--ignore-submodules=none", "-z"], false)
                        .unwrap_err().to_string();
                    assert!(error.contains("long-path read failed"), "{error}");
                    assert!(error.contains("shorten the worktree or child path"), "{error}");
                    assert!(!error.contains('\n'), "the refusal is safe for a log line");
                }
            }
        }
        let measurement = source_revision(&root, true);
        eprintln!("long-path diagnostic production state={state} outcome={measurement:?}");
        measurements.push(measurement);
    }
    assert!(matches!(measurements[0].as_ref().unwrap(), SourceRevision::Taken { .. }),
        "clean long child must measure clean: {:?}", measurements[0]);
    assert!(matches!(measurements[1].as_ref().unwrap(), SourceRevision::NotClean { .. }),
        "different-size child dirt must be observed: {:?}", measurements[1]);
}

#[cfg(windows)]
#[test]
fn windows_crlf_warnings_are_suppressed_without_ignoring_other_git_diagnostics() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = repository(&mut cleanup);
    let expected = head(&root);
    let original_fingerprint = capture_review_freeze(&ReviewFreezeGit::new(&root).unwrap()).unwrap();
    run_git_test_command(&root, &["config", "core.autocrlf", "true"]);
    run_git_test_command(&root, &["config", "core.safecrlf", "warn"]);
    let config_before = fs::read(root.join(".git/config")).unwrap();
    fs::write(root.join("tracked.txt"), "a changed LF-only file\n").unwrap();
    let git = ReviewFreezeGit::new(&root).unwrap();
    // This is the real benign Windows warning, deliberately re-enabled only
    // for the control. Residual stderr is never accepted by text matching.
    let args = ["-c", "core.safecrlf=warn", "diff", "--no-ext-diff", "--no-textconv"];
    let mut control = git.command(&args);
    let output = run_bounded_read_command(&mut control, git.deadline, REVIEW_FREEZE_OUTPUT_LIMIT, false).unwrap();
    assert!(output.status.success());
    assert!(!output.stderr.is_empty(), "the control must actually emit the conversion warning");
    let error = git.run(&args, false).unwrap_err().to_string();
    assert!(error.contains("Git emitted stderr; source measurement refused"), "{error}");
    assert!(!git.run(&["diff", "--no-ext-diff", "--no-textconv"], false).unwrap().is_empty());
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    assert_eq!(committed_source_clean_head(&root), Some(expected));
    assert_eq!(capture_review_freeze(&ReviewFreezeGit::new(&root).unwrap()).unwrap(), original_fingerprint);
    assert_eq!(fs::read(root.join(".git/config")).unwrap(), config_before);
}

#[test]
fn sha256_roots_are_outside_the_registered_commit_wire_domain() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let root = test_temp_dir().join(format!("ordinary-sha256-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    cleanup.0.push(root.clone());
    run_git_test_command(&root, &["init", "--quiet", "--template=", "--object-format=sha256"]);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&root, &["add", "--", "tracked.txt"]);
    let commit = |message| run_git_test_command(&root, &["-c", "user.name=Fixture",
        "-c", "user.email=test@example.invalid", "commit", "--quiet", "-m", message]);
    commit("base");
    let original = head(&root);
    assert_eq!(original.len(), 64);
    assert_eq!(committed_source_clean_head(&root), None);
    let cacheinfo = format!("160000,{original},dep");
    run_git_test_command(&root, &["update-index", "--add", "--cacheinfo", &cacheinfo]);
    commit("gitlink");
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("unsupported object format"), "{error}");
}

#[test]
fn pinned_submodule_has_the_existing_revision_and_root_status_reports_child_dirt() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    let original = head(&root);
    assert_eq!(source_revision(&root, true).unwrap(), SourceRevision::Taken {
        root: fs::canonicalize(&root).unwrap(), revision: format!("git-commit:{original}"),
    });
    assert_eq!(committed_source_clean_head(&root), Some(original.clone()));
    assert!(engram_place_source_basis_with(&workdir_place(&root), true).is_some());
    run_git_test_command(&root, &["config", "submodule.child with spaces.ignore", "all"]);
    for name in ["tracked.txt", "untracked.txt"] {
        fs::write(child.join(name), "child change\n").unwrap();
        let dirty = not_clean(&root);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].path, "child with spaces");
        assert_eq!(committed_source_clean_head(&root), None);
        if name == "tracked.txt" { run_git_test_command(&child, &["checkout", "--", name]); }
        else { fs::remove_file(child.join(name)).unwrap(); }
    }
    assert_eq!(committed_source_clean_head(&root), Some(original.clone()));
    fs::write(root.join("tracked.txt"), "root change\n").unwrap();
    assert_eq!(committed_source_clean_head(&root), None);
    run_git_test_command(&root, &["commit", "--quiet", "-am", "new root"]);
    assert_ne!(head(&root), original);
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));
}

#[test]
fn child_filters_refuse_before_recursive_root_status() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    for driver in ["clean", "smudge", "process"] {
        let key = format!("filter.fixture.{driver}");
        run_git_test_command(&child, &["config", &key, "must-not-execute"]);
        let error = source_revision(&root, true).unwrap_err().to_string();
        assert!(error.contains("configured Git filter"), "{error}");
        run_git_test_command(&child, &["config", "--unset", &key]);
    }
}

#[test]
fn initialized_nested_pinned_children_are_admitted() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    let donor = repository(&mut cleanup);
    run_git_test_command(&child, &["-c", "core.autocrlf=false", "-c", "protocol.file.allow=always",
        "submodule", "add", "--quiet", donor.to_str().unwrap(), "nested"]);
    run_git_test_command(&child.join("nested"), &["config", "core.autocrlf", "false"]);
    run_git_test_command(&child, &["-c", "user.name=Fixture", "-c", "user.email=test@example.invalid", "commit", "--quiet", "-am", "nested pin"]);
    run_git_test_command(&root, &["add", "--", "child with spaces"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "nested parent pin"]);
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));
    run_git_test_command(&child, &["config", "diff.ignoreSubmodules", "all"]);
    fs::write(child.join("nested/tracked.txt"), "nested dirt\n").unwrap();
    assert_eq!(committed_source_clean_head(&root), None);
}

#[test]
fn a_child_local_status_setting_cannot_hide_untracked_input() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    run_git_test_command(&child, &["config", "status.showUntrackedFiles", "no"]);
    fs::write(child.join("untracked.txt"), "child input\n").unwrap();
    assert_eq!(run_git_test_command_output(&root, &["status", "--porcelain=v1",
        "--untracked-files=all", "--ignore-submodules=none"]), "");
    let dirty = not_clean(&root);
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].path, "child with spaces");
    assert_eq!(committed_source_clean_head(&root), None);
}

#[test]
fn off_pin_and_deinitialized_submodules_are_named_refusals() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    run_git_test_command(&child, &["-c", "user.name=Fixture", "-c", "user.email=test@example.invalid",
        "commit", "--quiet", "--allow-empty", "-m", "off pin"]);
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("not initialized at its pin"), "{error}");
    run_git_test_command(&root, &["submodule", "deinit", "--force", "--", "child with spaces"]);
    let error = source_revision(&root, true).unwrap_err().to_string();
    assert!(error.contains("not initialized at its pin"), "{error}");
}

#[test]
fn a_clean_pinned_child_without_a_gitmodules_mapping_is_refused_by_name() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let (root, child) = repository_with_child(&mut cleanup);
    let pin = head(&child);
    assert_eq!(committed_source_clean_head(&root), Some(head(&root)));
    fs::write(root.join(".gitmodules"), "").unwrap();
    run_git_test_command(&root, &["add", "--", ".gitmodules"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "remove child mapping"]);
    assert_eq!(head(&child), pin);
    assert_eq!(run_git_test_command_output(&root, &["status", "--porcelain=v1", "--ignore-submodules=none"]), "");
    let error = format!("{:#}", source_revision(&root, true).unwrap_err());
    assert!(error.contains(".gitmodules mapping check failed") && error.contains("child with spaces"), "{error}");
    assert_eq!(committed_source_clean_head(&root), None);
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
