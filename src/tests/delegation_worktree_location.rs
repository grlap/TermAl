//! Where an isolated-worktree delegation's worktree lives: inside its own
//! source repository under the Git-ignored `.worktrees/`, never under the
//! TermAl data dir, refused outside the repository, in an unignored location,
//! or (on Windows) past Git's administrative path limit. Also that repository
//! walkers do not read those nested worktrees. Does not own worker base
//! resolution (delegation_worker.rs), the dirty-state copy into a new
//! worktree (delegations.rs), or removing a worktree after its delegation ends.

use super::delegation_support::test_app_state_with_delegation_codex_runtime;
use super::*;

/// A source repository whose `.worktrees/` is ignored (or not), with a parent
/// Codex session in it.
struct WorktreeRepo {
    state: AppState,
    _input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    parent: String,
    repo: PathBuf,
    _root: TestTempRoot,
}

impl WorktreeRepo {
    fn new(name: &str, ignore_worktrees: bool) -> Self {
        Self::with_repo_path(name, ignore_worktrees, |root| root.join("repo"))
    }

    fn with_repo_path(
        name: &str,
        ignore_worktrees: bool,
        repo_path: impl FnOnce(&FsPath) -> PathBuf,
    ) -> Self {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let root = TestTempRoot::create(name);
        let repo = repo_path(root.path());
        fs::create_dir_all(&repo).expect("repo directory");
        run_git_test_command(&repo, &["init"]);
        run_git_test_command(&repo, &["config", "user.name", "Worktree fixture"]);
        run_git_test_command(&repo, &["config", "user.email", "worktree@example.invalid"]);
        run_git_test_command(&repo, &["config", "core.autocrlf", "false"]);
        run_git_test_command(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("README.md"), "base\n").expect("base content");
        run_git_test_command(&repo, &["add", "README.md"]);
        if ignore_worktrees {
            fs::write(repo.join(".gitignore"), ".worktrees/\n").expect("ignore file");
            run_git_test_command(&repo, &["add", ".gitignore"]);
        }
        run_git_test_command(&repo, &["commit", "-m", "base"]);
        let (state, input_rx) = test_app_state_with_delegation_codex_runtime(name);
        let parent = {
            let mut inner = state.inner.lock().expect("state lock");
            let record = inner.create_session(
                Agent::Codex,
                Some("Worktree parent".to_owned()),
                repo.to_string_lossy().into_owned(),
                None,
                None,
            );
            let parent = record.session.id.clone();
            state.commit_locked(&mut inner).expect("parent persisted");
            parent
        };
        Self {
            state,
            _input_rx: input_rx,
            parent,
            repo,
            _root: root,
        }
    }

    fn spawn(&self, worktree_path: Option<&FsPath>) -> Result<DelegationResponse, ApiError> {
        self.state.create_read_only_delegation(
            &self.parent,
            CreateDelegationRequest {
                prompt: "Look around.".to_owned(),
                title: Some("Worktree location".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                    base: None,
                    owned_paths: Vec::new(),
                    worktree_path: worktree_path.map(|path| path.to_string_lossy().into_owned()),
                }),
            },
        )
    }

    fn worktree_of(created: &DelegationResponse) -> PathBuf {
        let DelegationWritePolicy::IsolatedWorktree { worktree_path, .. } =
            &created.delegation.write_policy
        else {
            panic!("the delegation keeps its isolated write policy");
        };
        PathBuf::from(worktree_path.as_deref().expect("worktree path recorded"))
    }

    #[cfg(windows)]
    fn registered_worktrees(&self) -> String {
        run_git_test_command_output(&self.repo, &["worktree", "list", "--porcelain"])
    }

    fn parent_status(&self) -> String {
        run_git_test_command_output(
            &self.repo,
            &["status", "--porcelain", "--untracked-files=all"],
        )
    }
}

fn same_path(left: &FsPath, right: &FsPath) -> bool {
    normalize_path_best_effort(left) == normalize_path_best_effort(right)
}

/// The default-path cases run in a fresh test process whose HOME and
/// USERPROFILE name a sentinel directory this test owns. A default that still
/// resolved through the home would land in the sentinel, never the real
/// ~/.termal, and the sentinel must stay empty.
#[test]
fn default_worktrees_never_touch_the_home_directory() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let sandbox = TestTempRoot::create("worktree-default-home");
    let sentinel = sandbox.path().join("home");
    fs::create_dir_all(&sentinel).expect("sentinel home");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "tests::delegation_worktree_location::default_worktree_child",
            "--nocapture",
        ])
        .env("TERMAL_TEST_DEFAULT_WORKTREE_CHILD", "1")
        .env("HOME", &sentinel)
        .env("USERPROFILE", &sentinel);
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("default worktree child");
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains("default worktree cases passed"),
        "the child ran its cases: {stderr}"
    );
    let left = fs::read_dir(&sentinel)
        .expect("sentinel readable")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    assert!(left.is_empty(), "the home received {left:?}");
}

#[test]
fn default_worktree_child() {
    // Driven in a fresh process by default_worktrees_never_touch_the_home_directory.
    if std::env::var_os("TERMAL_TEST_DEFAULT_WORKTREE_CHILD").is_none() {
        return;
    }
    a_default_worktree_lives_in_the_source_repos_ignored_worktrees_directory();
    a_default_worktree_is_refused_when_the_worktrees_directory_is_not_ignored();
    #[cfg(windows)]
    {
        a_default_worktree_fits_git_in_a_deeply_nested_repository();
        a_worktree_beyond_gits_windows_path_limit_is_refused_with_the_reason();
    }
    eprintln!("default worktree cases passed");
}

fn a_default_worktree_lives_in_the_source_repos_ignored_worktrees_directory() {
    let fixture = WorktreeRepo::new("worktree-default-location", true);
    let created = fixture.spawn(None).expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    let worktrees = fixture.repo.join(".worktrees");
    assert!(
        same_path(worktree.parent().expect("worktree parent"), &worktrees),
        "{} is not in {}",
        worktree.display(),
        worktrees.display()
    );
    // Git names the worktree's metadata directory after this folder as well,
    // so a long name would cost twice against Git's Windows path limit.
    let folder = worktree.file_name().expect("folder").to_string_lossy();
    assert!(
        folder.len() <= 14,
        "the folder name {folder} is not compact"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("README.md")).expect("checked-out file"),
        "base\n"
    );
    assert_eq!(
        fixture.parent_status(),
        "",
        "the nested worktree leaves the parent's status clean"
    );
}

fn a_default_worktree_is_refused_when_the_worktrees_directory_is_not_ignored() {
    let fixture = WorktreeRepo::new("worktree-not-ignored", false);
    let error = match fixture.spawn(None) {
        Ok(_) => panic!("an unignored .worktrees/ must refuse the spawn"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains(".worktrees"), "{}", error.message);
    assert!(!fixture.repo.join(".worktrees").exists());
    assert_eq!(fixture.parent_status(), "");
}

/// Builds the fixture's repository at a path `length` characters long, as a
/// repository in a deeply nested checkout would be.
#[cfg(windows)]
fn repo_at_depth(length: usize) -> impl FnOnce(&FsPath) -> PathBuf {
    move |root| {
        let prefix = root.to_string_lossy().len() + 1;
        assert!(
            prefix < length,
            "the fixture root {} is already longer than {length} characters",
            root.display()
        );
        root.join("r".repeat(length - prefix))
    }
}

#[cfg(windows)]
fn a_default_worktree_fits_git_in_a_deeply_nested_repository() {
    // A 189-character repository path puts the worktree's `.git` file, the
    // longer of the paths Git checks, at exactly Git's 220. The earlier
    // `delegation-<id>` folder name failed beyond a repository path of about
    // 145 characters.
    let fixture = WorktreeRepo::with_repo_path("worktree-deep", true, repo_at_depth(189));
    let created = fixture.spawn(None).expect("isolated delegation at depth");
    let worktree = WorktreeRepo::worktree_of(&created);
    assert_eq!(
        fs::read_to_string(worktree.join("README.md")).expect("checked-out file"),
        "base\n"
    );
    assert_eq!(fixture.parent_status(), "");
}

#[cfg(windows)]
fn a_worktree_beyond_gits_windows_path_limit_is_refused_with_the_reason() {
    // One character deeper puts the worktree's `.git` file at 221.
    let fixture = WorktreeRepo::with_repo_path("worktree-too-deep", true, repo_at_depth(190));
    let error = match fixture.spawn(None) {
        Ok(_) => panic!("a worktree past Git's path limit must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("too deep for Git on Windows"),
        "{}",
        error.message
    );
    assert!(!fixture.repo.join(".worktrees").exists());
    assert_eq!(
        fixture.registered_worktrees().matches("worktree ").count(),
        1
    );
    assert_eq!(fixture.parent_status(), "");
}

#[test]
fn an_explicit_worktree_path_outside_the_source_repo_is_refused() {
    let fixture = WorktreeRepo::new("worktree-explicit-outside", true);
    let outside = fixture._root.path().join("outside");
    let error = match fixture.spawn(Some(&outside)) {
        Ok(_) => panic!("a worktree outside the source repository must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains(".worktrees"),
        "the refusal names the allowed location: {}",
        error.message
    );
    assert!(!outside.exists());
}

#[test]
fn an_explicit_worktree_path_inside_the_ignored_directory_is_accepted() {
    let fixture = WorktreeRepo::new("worktree-explicit-inside", true);
    let inside = fixture.repo.join(".worktrees").join("chosen");
    let created = fixture.spawn(Some(&inside)).expect("isolated delegation");
    assert!(same_path(&WorktreeRepo::worktree_of(&created), &inside));
    assert!(inside.join("README.md").is_file());
}

/// Spawns with an explicit path that must be refused, and checks that the
/// refusal is a bounded 400 that created nothing at that path.
fn refuse_explicit(fixture: &WorktreeRepo, path: &FsPath) -> String {
    let error = match fixture.spawn(Some(path)) {
        Ok(_) => panic!("{} must be refused", path.display()),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST, "{}", error.message);
    assert!(!path.exists(), "{} was created", path.display());
    assert_eq!(fixture.parent_status(), "");
    error.message
}

#[test]
fn explicit_paths_inside_the_repository_but_not_ignored_are_refused() {
    let fixture = WorktreeRepo::new("worktree-explicit-refusals", true);
    // Inside .git, in a directory Git does not ignore, and the repository
    // root itself.
    refuse_explicit(&fixture, &fixture.repo.join(".git").join("child"));
    fs::create_dir_all(fixture.repo.join("src")).expect("unignored directory");
    let message = refuse_explicit(&fixture, &fixture.repo.join("src").join("child"));
    assert!(message.contains("not ignored"), "{message}");
    let root_error = match fixture.spawn(Some(&fixture.repo)) {
        Ok(_) => panic!("the repository root itself must be refused"),
        Err(error) => error,
    };
    assert_eq!(root_error.status, StatusCode::BAD_REQUEST);
}

#[cfg(unix)]
#[test]
fn a_literal_backslash_does_not_pass_as_the_ignored_directory() {
    // On Unix `.worktrees\child` is one file name beside `.worktrees`, which
    // Git does not ignore.
    let fixture = WorktreeRepo::new("worktree-backslash", true);
    let message = refuse_explicit(&fixture, &fixture.repo.join(".worktrees\\child"));
    assert!(message.contains("not ignored"), "{message}");
}

#[cfg(unix)]
#[test]
fn an_ignored_directory_linked_outside_the_repository_is_refused() {
    let fixture = WorktreeRepo::new("worktree-linked-out", true);
    let outside = fixture._root.path().join("outside");
    fs::create_dir_all(&outside).expect("outside directory");
    std::os::unix::fs::symlink(&outside, fixture.repo.join(".worktrees")).expect("link");
    let error = match fixture.spawn(Some(&fixture.repo.join(".worktrees").join("child"))) {
        Ok(_) => panic!("a worktree resolving outside the repository must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(!outside.join("child").exists());
}

#[cfg(windows)]
#[test]
fn a_taken_git_metadata_name_counts_against_the_windows_limit() {
    // With short ignored directories the metadata path is the longer one:
    // `<repo>/.git/worktrees/x` is 220 characters for a 203-character
    // repository. A second worktree also named `x` gets `x1`, at 221.
    let fixture = WorktreeRepo::with_repo_path("worktree-taken-name", true, repo_at_depth(203));
    fs::write(
        fixture.repo.join(".git").join("info").join("exclude"),
        "w/\nv/\n",
    )
    .expect("exclude rules");
    let first = fixture.repo.join("w").join("x");
    fixture
        .spawn(Some(&first))
        .expect("metadata at exactly 220");
    assert!(first.join("README.md").is_file());
    let message = refuse_explicit(&fixture, &fixture.repo.join("v").join("x"));
    assert!(message.contains("too deep for Git on Windows"), "{message}");
    assert_eq!(
        fixture.registered_worktrees().matches("worktree ").count(),
        2
    );
}

#[test]
fn a_read_only_review_of_the_repository_leaves_its_isolated_worktree_writable() {
    let fixture = WorktreeRepo::new("worktree-review-scope", true);
    create_test_project(&fixture.state, &fixture.repo, "Worktree review scope");
    let isolated = fixture
        .spawn(Some(&fixture.repo.join(".worktrees").join("child")))
        .expect("isolated delegation");
    fixture
        .state
        .create_read_only_delegation(
            &fixture.parent,
            CreateDelegationRequest {
                prompt: "Review the repository.".to_owned(),
                title: Some("Repository review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("read-only delegation");
    // The isolated child's worktree is a Git tree of its own: neither its
    // writes nor its approvals belong to the reviewed repository.
    let child = isolated.delegation.child_session_id.clone();
    fixture
        .state
        .ensure_read_only_delegation_allows_write_action(Some(&child), None, None, "child write")
        .expect("the isolated child can write while the repository is reviewed");
    fixture
        .state
        .ensure_read_only_delegation_allows_approval(&child)
        .expect("the isolated child's approvals stay open");
    // A Git file action on an existing file in the child passes the same
    // guards the file route uses, with the file's own path.
    let worktree = WorktreeRepo::worktree_of(&isolated);
    ensure_git_repo_root_write_allowed(&fixture.state, None, None, &worktree, "git file actions")
        .expect("the child's repository root is writable");
    ensure_git_path_write_allowed(
        &fixture.state,
        None,
        None,
        &worktree,
        FsPath::new("README.md"),
        "git file actions",
    )
    .expect("an existing file in the child is writable");
    // A submodule-style nested repository, whose `.git` file names a Git
    // directory with no common dir, is part of the reviewed tree. Git needs
    // the parent of a separate Git directory to exist.
    fs::create_dir_all(fixture.repo.join(".git").join("modules")).expect("modules directory");
    run_git_test_command(
        &fixture.repo,
        &[
            "init",
            "--quiet",
            "--separate-git-dir",
            ".git/modules/vendored",
            "vendored",
        ],
    );
    let vendored = fixture.repo.join("vendored").to_string_lossy().into_owned();
    let error = fixture
        .state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(vendored.as_str()),
            "submodule write",
        )
        .expect_err("writes in a nested repository stay blocked");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    // The reviewed repository's own tree stays read-only.
    let inside = fixture.repo.join("src");
    fs::create_dir_all(&inside).expect("repository directory");
    let inside = inside.to_string_lossy().into_owned();
    let error = fixture
        .state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(inside.as_str()),
            "repository write",
        )
        .expect_err("writes in the reviewed repository stay blocked");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
}

#[test]
fn instruction_search_does_not_read_delegation_worktrees() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("instruction-search-worktrees");
    fs::write(root.join("CLAUDE.md"), "Prefer small steps.\n").expect("root doc");
    let nested = root.join(".worktrees").join("delegation-copy");
    fs::create_dir_all(&nested).expect("nested worktree");
    fs::write(nested.join("CLAUDE.md"), "Prefer small steps.\n").expect("copied doc");
    let response = search_instruction_phrase(root.path(), "small steps").expect("search");
    assert_eq!(
        response
            .matches
            .iter()
            .map(|matched| matched.path.clone())
            .collect::<Vec<_>>(),
        [normalize_path_best_effort(&root.join("CLAUDE.md"))
            .to_string_lossy()
            .into_owned()],
        "a delegation's copy of the instructions is not the workdir's own"
    );
}
