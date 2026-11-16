//! Where an isolated-worktree delegation's worktree lives and when it goes:
//! inside its own source repository under the Git-ignored `.worktrees/`,
//! never under the TermAl data dir, removed when the delegation ends with
//! nothing to lose, retained with a reason otherwise. Also that repository
//! walkers do not read those nested worktrees. Does not own worker base
//! resolution (delegation_worker.rs) or the dirty-state copy into a new
//! worktree (delegations.rs).

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
        let _placement = TestTempRootDirectoryScope::repository_local();
        let root = TestTempRoot::create(name);
        let repo = root.path().join("repo");
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

    /// Spawns into a named directory under the ignored `.worktrees/`. Cases
    /// run in this process never use the default path, which on a host that
    /// still resolved it through the home would write to the real one.
    fn spawn_inside(&self, name: &str) -> Result<DelegationResponse, ApiError> {
        self.spawn(Some(&self.repo.join(".worktrees").join(name)))
    }

    fn worktree_of(created: &DelegationResponse) -> PathBuf {
        let DelegationWritePolicy::IsolatedWorktree { worktree_path, .. } =
            &created.delegation.write_policy
        else {
            panic!("the delegation keeps its isolated write policy");
        };
        PathBuf::from(worktree_path.as_deref().expect("worktree path recorded"))
    }

    fn registered_worktrees(&self) -> String {
        run_git_test_command_output(&self.repo, &["worktree", "list", "--porcelain"])
    }

    fn parent_status(&self) -> String {
        run_git_test_command_output(
            &self.repo,
            &["status", "--porcelain", "--untracked-files=all"],
        )
    }

    /// Cancels the delegation, then runs one pass of the worktree sweep the
    /// host's background thread runs.
    fn cancel(&self, created: &DelegationResponse) {
        self.cancel_only(created);
        self.state.sweep_delegation_worktrees();
    }

    /// Ends the child's turn with a completed result, as its runtime would.
    fn complete(&self, created: &DelegationResponse) {
        let child = created.delegation.child_session_id.clone();
        let token = {
            let mut inner = self.state.inner.lock().expect("state lock");
            let index = inner.find_session_index(&child).expect("child");
            let token = inner.sessions[index]
                .runtime
                .runtime_token()
                .expect("child runtime");
            let message_id = inner.next_message_id();
            let record = inner.session_mut_by_index(index).expect("child record");
            push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "## Result\n\nStatus: completed\n\nSummary:\nLooked around.".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            );
            self.state.commit_locked(&mut inner).expect("commit");
            token
        };
        self.state
            .finish_turn_ok_if_runtime_matches(&child, &token)
            .expect("completion");
    }

    fn cancel_only(&self, created: &DelegationResponse) {
        self.state
            .cancel_delegation(&self.parent, &created.delegation.id)
            .expect("cancel");
    }

    /// Edits the delegation's child session record.
    fn child_record(&self, created: &DelegationResponse, edit: impl FnOnce(&mut SessionRecord)) {
        let mut inner = self.state.inner.lock().expect("state lock");
        let index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child");
        edit(&mut inner.sessions[index]);
    }

    fn result_notes(&self, created: &DelegationResponse) -> Vec<String> {
        self.state
            .get_delegation(&self.parent, &created.delegation.id)
            .expect("delegation")
            .delegation
            .result
            .map(|result| result.notes)
            .unwrap_or_default()
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
    eprintln!("default worktree cases passed");
}

fn a_default_worktree_lives_in_the_source_repos_ignored_worktrees_directory() {
    let fixture = WorktreeRepo::new("worktree-default-location", true);
    let created = fixture.spawn(None).expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    let expected = fixture
        .repo
        .join(".worktrees")
        .join(format!("delegation-{}", created.delegation.id));
    assert!(
        same_path(&worktree, &expected),
        "{} is not {}",
        worktree.display(),
        expected.display()
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

#[test]
fn a_clean_worktree_is_removed_when_its_delegation_ends() {
    let fixture = WorktreeRepo::new("worktree-clean-removed", true);
    let created = fixture.spawn_inside("clean").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    assert!(worktree.is_dir());
    fixture.cancel(&created);
    assert!(
        !worktree.exists(),
        "a worktree with nothing to lose goes when its delegation ends"
    );
    // Only the source checkout itself is still registered.
    assert!(
        !fixture.registered_worktrees().contains(".worktrees"),
        "and Git no longer lists it: {}",
        fixture.registered_worktrees()
    );
}

#[test]
fn a_worktree_with_uncommitted_changes_is_retained_with_a_reason() {
    let fixture = WorktreeRepo::new("worktree-dirty-retained", true);
    let created = fixture.spawn_inside("dirty").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fs::write(worktree.join("README.md"), "child edit\n").expect("child edit");
    fixture.cancel(&created);
    assert!(worktree.join("README.md").is_file(), "the edit is kept");
    let notes = fixture.result_notes(&created);
    assert!(
        notes
            .iter()
            .any(|note| note.contains("retained") && note.contains("uncommitted")),
        "the delegation says why its worktree stays: {notes:?}"
    );
}

#[test]
fn a_worktree_commit_on_no_ref_is_retained_and_one_on_a_branch_is_removed() {
    let fixture = WorktreeRepo::new("worktree-commit-reachability", true);
    let unreachable = fixture.spawn_inside("detached").expect("first delegation");
    let unreachable_root = WorktreeRepo::worktree_of(&unreachable);
    fs::write(unreachable_root.join("README.md"), "detached work\n").expect("edit");
    run_git_test_command(&unreachable_root, &["commit", "-am", "detached work"]);
    fixture.cancel(&unreachable);
    assert!(
        unreachable_root.is_dir(),
        "a commit no ref holds would be lost"
    );
    assert!(
        fixture
            .result_notes(&unreachable)
            .iter()
            .any(|note| note.contains("retained") && note.contains("commit")),
        "{:?}",
        fixture.result_notes(&unreachable)
    );

    let kept = fixture.spawn_inside("branched").expect("second delegation");
    let kept_root = WorktreeRepo::worktree_of(&kept);
    fs::write(kept_root.join("README.md"), "branch work\n").expect("edit");
    run_git_test_command(&kept_root, &["commit", "-am", "branch work"]);
    run_git_test_command(&kept_root, &["branch", "keep-delegation-work"]);
    fixture.cancel(&kept);
    assert!(
        !kept_root.exists(),
        "a commit a branch holds is preserved without the worktree"
    );
}

#[test]
fn a_followup_on_a_delegation_whose_worktree_was_removed_is_refused() {
    let fixture = WorktreeRepo::new("worktree-followup-refused", true);
    let created = fixture
        .spawn_inside("followup")
        .expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    fixture.state.sweep_delegation_worktrees();
    assert!(
        !worktree.exists(),
        "the clean worktree went after completion"
    );
    let error = match fixture.state.followup_delegation(
        &fixture.parent,
        &created.delegation.id,
        "one more look".to_owned(),
    ) {
        Ok(_) => panic!("a follow-up without its worktree must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("worktree"), "{}", error.message);
    assert!(!worktree.exists(), "the worktree is not recreated");
}

#[test]
fn a_worktree_holding_a_nested_worktree_is_retained_with_everything_in_it() {
    let fixture = WorktreeRepo::new("worktree-nested-retained", true);
    let outer = fixture.spawn_inside("outer").expect("outer delegation");
    let outer_root = WorktreeRepo::worktree_of(&outer);
    // A nested delegation's worktree lives in the outer one's ignored
    // `.worktrees/`, so the outer status stays clean.
    run_git_test_command(
        &outer_root,
        &["worktree", "add", "--detach", ".worktrees/inner", "HEAD"],
    );
    let inner_root = outer_root.join(".worktrees").join("inner");
    fs::write(inner_root.join("README.md"), "nested work\n").expect("nested edit");
    fixture.cancel(&outer);
    assert_eq!(
        fs::read_to_string(inner_root.join("README.md")).expect("nested work kept"),
        "nested work\n"
    );
    assert!(
        outer_root.join(".git").is_file(),
        "the outer worktree stays"
    );
    assert!(
        fixture
            .result_notes(&outer)
            .iter()
            .any(|note| note.contains("retained") && note.contains("another worktree")),
        "{:?}",
        fixture.result_notes(&outer)
    );
}

#[test]
fn the_sweep_waits_for_the_child_runtime_shutdown_and_its_source_observation() {
    let fixture = WorktreeRepo::new("worktree-sweep-waits", true);
    let created = fixture.spawn_inside("waits").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    let requests = fixture.state.delegation_worktree_sweep_requests();
    fixture.cancel_only(&created);
    assert!(
        fixture.state.delegation_worktree_sweep_requests() > requests,
        "ending the delegation asks for a sweep pass"
    );
    let child = created.delegation.child_session_id.clone();
    // A runtime shutdown still in progress fences the worktree.
    fixture
        .state
        .inner
        .lock()
        .expect("state lock")
        .delegation_child_shutdowns
        .insert(child.clone());
    fixture.state.sweep_delegation_worktrees();
    assert!(
        worktree.is_dir(),
        "not reclaimed while the child shuts down"
    );
    fixture.state.release_delegation_child_shutdown(&child);
    // So does a closing source observation that still reads it.
    fixture.child_record(&created, |record| {
        record.engram.source_root_bind_owner = Some(Uuid::new_v4());
    });
    fixture.state.sweep_delegation_worktrees();
    assert!(worktree.is_dir(), "not reclaimed while it is observed");
    fixture.child_record(&created, |record| {
        record.engram.source_root_bind_owner = None;
    });
    // A later pass picks it up; nothing was queued for it.
    fixture.state.sweep_delegation_worktrees();
    assert!(!worktree.exists(), "reclaimed once nothing holds it");
}

#[test]
fn a_followup_is_refused_while_the_sweep_holds_the_worktree_and_admitted_after() {
    let fixture = WorktreeRepo::new("worktree-followup-race", true);
    let created = fixture.spawn_inside("held").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    // The child left work, so the sweep will keep this worktree.
    fs::write(worktree.join("README.md"), "child edit\n").expect("child edit");
    fixture.complete(&created);
    fixture
        .state
        .inner
        .lock()
        .expect("state lock")
        .reclaiming_delegation_worktrees
        .insert(created.delegation.id.clone());
    let error = match fixture.state.followup_delegation(
        &fixture.parent,
        &created.delegation.id,
        "one more look".to_owned(),
    ) {
        Ok(_) => panic!("a follow-up must wait while the sweep holds the worktree"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("worktree"), "{}", error.message);
    fixture
        .state
        .inner
        .lock()
        .expect("state lock")
        .reclaiming_delegation_worktrees
        .remove(&created.delegation.id);
    fixture.state.sweep_delegation_worktrees();
    assert!(worktree.is_dir(), "the child's work keeps the worktree");
    fixture
        .state
        .followup_delegation(
            &fixture.parent,
            &created.delegation.id,
            "one more look".to_owned(),
        )
        .expect("a retained worktree keeps follow-ups available");
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

#[test]
fn the_termal_data_dir_under_test_is_never_the_users_home() {
    let Some(home) = resolve_home_dir() else {
        return;
    };
    // The checkout itself may sit under the home (macOS runners), so the
    // check is the production location, not the home as a whole.
    let production = normalize_path_best_effort(&home.join(".termal"));
    let data = resolve_termal_data_dir("unused-default-workdir");
    assert!(
        !normalize_path_best_effort(&data).starts_with(&production),
        "a test resolved the TermAl data dir to the production {}",
        data.display()
    );
}
