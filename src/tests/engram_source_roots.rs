// Owns the tests of a work's source root (src/engram_source_roots.rs): what may
// be named, the basis taken on exactly the named path, the list's cap, the
// rules that end an entry, the closing basis of a turn whose root was sealed,
// and the root an evaluation is taken on. Does not own the tests of turns
// measured in a named root (src/tests/engram_turn_observations.rs) or of check
// credit (src/tests/engram_turn_checks.rs). New module.
use super::*;

/// Removes the directories it owns when dropped, on success and on an
/// assertion's unwind alike.
struct RemoveDirsOnDrop(Vec<PathBuf>);

impl Drop for RemoveDirsOnDrop {
    fn drop(&mut self) {
        for dir in self.0.iter().rev() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

/// A project folder holding a repository with one commit, and a linked
/// worktree of it under `.worktrees/wt`.
struct Project {
    root: PathBuf,
    worktree: PathBuf,
}

fn project(cleanup: &mut RemoveDirsOnDrop) -> Project {
    let root = test_temp_dir().join(format!("source-root-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    cleanup.0.push(root.clone());
    init_git_document_test_repo(&root);
    fs::write(root.join(".gitignore"), ".worktrees/\ntarget/\n").unwrap();
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    run_git_test_command(&root, &["add", ".gitignore", "tracked.txt"]);
    run_git_test_command(&root, &["commit", "--quiet", "-m", "base"]);
    let worktree = root.join(".worktrees").join("wt");
    run_git_test_command(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            worktree.to_str().unwrap(),
        ],
    );
    Project { root, worktree }
}

fn root_key(path: &FsPath) -> String {
    fs::canonicalize(path).unwrap().to_string_lossy().into_owned()
}

fn validate(path: &FsPath, project: &Project) -> std::result::Result<(String, String), String> {
    validate_engram_source_root(
        &path.to_string_lossy(),
        &project.root.to_string_lossy(),
        &project.root.to_string_lossy(),
    )
}

fn store() -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey {
        database_path: PathBuf::from("C:/store/engram.db"),
        project_id: "github.com/example/project".to_owned(),
    }
}

fn entry(work: &str, claim: &str, session: &str, generation: u64) -> EngramWorkSourceRoot {
    EngramWorkSourceRoot {
        store: store(),
        work_id: format!("work-{work}"),
        short_ref: format!("w-{work}"),
        claim_id: claim.to_owned(),
        claim_fence: 1,
        root: format!("C:/p/.worktrees/{work}"),
        common_dir_key: "c:/p/.git".to_owned(),
        named_by_session: session.to_owned(),
        named_at: "2026-09-27T00:00:00.000Z".to_owned(),
        generation,
    }
}

fn held(claims: &[&str], omitted: u64) -> EngramHeldClaims {
    EngramHeldClaims {
        items: claims
            .iter()
            .map(|claim| EngramHeldClaim {
                claim_id: (*claim).to_owned(),
                ..Default::default()
            })
            .collect(),
        omitted,
    }
}

#[test]
fn a_linked_worktree_inside_the_project_can_be_named() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) =
        validate(&project.worktree, &project).expect("a registered worktree may be named");
    assert_eq!(root, root_key(&project.worktree));
    assert_eq!(
        common_dir_key,
        engram_exact_path_key(&fs::canonicalize(project.root.join(".git")).unwrap()),
        "the key names the repository's common directory"
    );
    // A relative path resolves from the session's workdir.
    let (relative, _) = validate(FsPath::new(".worktrees/wt"), &project)
        .expect("a relative path names the same worktree");
    assert_eq!(relative, root);
    // The main checkout is a registered worktree of its own repository.
    assert!(validate(&project.root, &project).is_ok());
}

#[test]
fn a_directory_inside_a_worktree_is_not_a_worktree_root() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let inside = project.worktree.join("sub");
    fs::create_dir_all(&inside).unwrap();
    let error = validate(&inside, &project).expect_err("a subdirectory is refused");
    assert!(error.contains("is not the root of a worktree"), "{error}");
}

#[test]
fn a_repository_of_its_own_inside_the_project_is_refused() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let other = project.root.join(".worktrees").join("other");
    fs::create_dir_all(&other).unwrap();
    init_git_document_test_repo(&other);
    fs::write(other.join("file.txt"), "other\n").unwrap();
    run_git_test_command(&other, &["add", "file.txt"]);
    run_git_test_command(&other, &["commit", "--quiet", "-m", "other"]);
    let error = validate(&other, &project).expect_err("another repository is refused");
    assert!(error.contains("another repository"), "{error}");
}

#[test]
fn a_copied_git_file_is_not_a_registered_worktree() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let copy = project.root.join(".worktrees").join("copy");
    fs::create_dir_all(&copy).unwrap();
    fs::copy(project.worktree.join(".git"), copy.join(".git")).unwrap();
    fs::write(copy.join("tracked.txt"), "base\n").unwrap();
    let error = validate(&copy, &project).expect_err("a copied .git file is refused");
    assert!(
        error.contains("not a worktree registered") || error.contains("is not the root of a worktree"),
        "{error}"
    );
}

#[test]
fn a_worktree_outside_the_project_folder_is_refused() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let outside = test_temp_dir().join(format!("source-root-outside-{}", Uuid::new_v4()));
    cleanup.0.insert(0, outside.clone());
    run_git_test_command(
        &project.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "outside",
            outside.to_str().unwrap(),
        ],
    );
    let error = validate(&outside, &project).expect_err("a worktree outside is refused");
    assert!(error.contains("outside the project folder"), "{error}");
}

#[test]
fn a_named_root_is_measured_on_exactly_its_own_path() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    let basis = engram_named_root_source_basis(&root, &common_dir_key)
        .expect("a present root has a basis");
    assert_eq!(basis.workspace_id, root);
    let (_, main_revision) = content_revision(&project.root).unwrap();
    fs::write(project.worktree.join("tracked.txt"), "changed in the worktree\n").unwrap();
    let moved = engram_named_root_source_basis(&root, &common_dir_key).unwrap();
    assert_ne!(moved.source_revision, basis.source_revision, "an edit there moves it");
    assert_ne!(moved.source_revision, main_revision);
    assert!(
        engram_named_root_source_basis(&root, "c:/another/repository/.git").is_none(),
        "a root that now belongs to another repository has no basis"
    );
}

#[test]
fn a_named_root_without_its_git_entry_has_no_basis_and_never_main() {
    // F1: the workdir's basis walks up to the nearest `.git`, which for a
    // worktree inside the main checkout is main itself.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    fs::remove_file(project.worktree.join(".git")).unwrap();
    assert!(engram_named_root_absent(&root));
    assert!(
        engram_named_root_source_basis(&root, &common_dir_key).is_none(),
        "a root that lost its .git is not measured"
    );
    let walked = engram_execution_source_basis(&project.worktree)
        .expect("the ancestor walk finds main, which is why a named root never takes it");
    assert_eq!(walked.workspace_id, root_key(&project.root));
}

#[test]
fn a_named_root_that_was_removed_has_no_basis() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    fs::remove_dir_all(&project.worktree).unwrap();
    assert!(engram_named_root_absent(&root));
    assert!(engram_named_root_source_basis(&root, &common_dir_key).is_none());
    assert!(
        engram_place_source_basis(&EngramBasisPlace::Named {
            root,
            common_dir_key
        })
        .is_none()
    );
}

/// Links `link` to the directory `target`: a symbolic link on Unix, a
/// junction on Windows, which needs no privilege.
fn link_directory(link: &FsPath, target: &FsPath) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("the symbolic link should be created");
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink should run");
        assert!(status.success(), "the junction should be created");
    }
}

#[test]
fn a_named_root_replaced_by_a_link_to_another_tree_has_no_basis() {
    // The named worktree is replaced by a link to the main checkout of the
    // same repository: the repository check passes, but the path no longer
    // resolves to itself, so nothing is measured there.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    assert!(engram_named_root_source_basis(&root, &common_dir_key).is_some());
    fs::remove_dir_all(&project.worktree).unwrap();
    link_directory(&project.worktree, &project.root);

    assert!(
        engram_named_root_source_basis(&root, &common_dir_key).is_none(),
        "the link's target is not the named root"
    );
    assert!(
        !engram_named_root_absent(&root),
        "and the path is there, so no sealed revision stands in either"
    );
}

#[test]
fn a_naming_capture_leaves_the_commit_its_reserve() {
    // A capture stops short of the budget's end, so a slow tree reports the
    // root unmeasured instead of spending the budget and refusing the name.
    assert_eq!(
        engram_source_root_capture_budget(ENGRAM_SOURCE_ROOT_NAMING_BUDGET),
        REVIEW_FREEZE_TIMEOUT.min(ENGRAM_SOURCE_ROOT_NAMING_BUDGET - ENGRAM_SOURCE_ROOT_COMMIT_RESERVE)
    );
    assert_eq!(
        engram_source_root_capture_budget(ENGRAM_SOURCE_ROOT_COMMIT_RESERVE + Duration::from_secs(3)),
        Duration::from_secs(3).min(REVIEW_FREEZE_TIMEOUT)
    );
    assert_eq!(
        engram_source_root_capture_budget(ENGRAM_SOURCE_ROOT_COMMIT_RESERVE),
        Duration::ZERO,
        "nothing is left to capture in once only the reserve remains"
    );
}

#[test]
fn a_full_list_refuses_a_new_name_and_evicts_nothing() {
    let mut entries = (0..ENGRAM_WORK_SOURCE_ROOT_LIMIT)
        .map(|index| entry(&index.to_string(), "claim", "session-1", 1))
        .collect::<Vec<_>>();
    let error = engram_set_work_source_root(
        &mut entries,
        &store(),
        "work-new",
        Some(entry("new", "claim", "session-1", 1)),
    )
    .expect_err("a new name on a full list is refused");
    assert!(error.contains("evicts none"), "{error}");
    assert!(error.contains("w-0 (named by session-1"), "the refusal names the entries: {error}");
    assert!(
        error.contains(&format!("in store {})", store().project_id)),
        "and each entry's store, since an entry of a store no longer used cannot be cleared: \
         {error}"
    );
    assert!(error.contains("stays until the session that named it"), "{error}");
    assert_eq!(entries.len(), ENGRAM_WORK_SOURCE_ROOT_LIMIT, "nothing was evicted");
    // Renaming a work already on the list replaces it in place.
    engram_set_work_source_root(
        &mut entries,
        &store(),
        "work-3",
        Some(entry("3", "claim", "session-1", 2)),
    )
    .expect("a rename is not a new entry");
    assert_eq!(
        engram_work_source_root_for_work(&entries, &store(), "work-3").map(|entry| entry.generation),
        Some(2)
    );
    // A clear removes it.
    engram_set_work_source_root(&mut entries, &store(), "work-3", None).unwrap();
    assert!(engram_work_source_root_for_work(&entries, &store(), "work-3").is_none());
}

#[test]
fn an_entry_applies_only_to_the_claim_it_was_named_for() {
    let entries = vec![entry("a", "claim-1", "session-1", 1)];
    assert!(engram_work_source_root_for_claim(&entries, &store(), "work-a", "claim-1").is_some());
    assert!(
        engram_work_source_root_for_claim(&entries, &store(), "work-a", "claim-2").is_none(),
        "a new claim on the same work does not inherit the tree"
    );
    let other_store = EngramAuthorityStoreKey {
        project_id: "github.com/example/other".to_owned(),
        ..store()
    };
    assert!(engram_work_source_root_for_claim(&entries, &other_store, "work-a", "claim-1").is_none());
}

#[test]
fn only_a_complete_held_list_ends_the_entries_of_released_claims() {
    let mut entries = vec![
        entry("kept", "claim-kept", "session-1", 1),
        entry("released", "claim-released", "session-1", 1),
        entry("other", "claim-other", "session-2", 1),
    ];
    let known = entries.clone();
    assert!(
        !engram_end_released_work_source_roots(
            &mut entries,
            &known,
            "session-1",
            &store(),
            &held(&["claim-kept"], 1)
        ),
        "a list that left claims out ends nothing"
    );
    assert_eq!(entries.len(), 3);
    // Named after the read began: its claim may not be on the list yet.
    entries.push(entry("late", "claim-late", "session-1", 2));
    assert!(engram_end_released_work_source_roots(
        &mut entries,
        &known,
        "session-1",
        &store(),
        &held(&["claim-kept"], 0)
    ));
    let names = entries.iter().map(|entry| entry.short_ref.as_str()).collect::<Vec<_>>();
    assert_eq!(
        names,
        ["w-kept", "w-other", "w-late"],
        "another session's entry is not ended by this session's read, nor one named after it"
    );
}

#[test]
fn an_entry_ends_with_the_session_that_named_it() {
    let mut entries = vec![
        entry("a", "claim-a", "session-gone", 1),
        entry("b", "claim-b", "session-live", 1),
    ];
    assert!(engram_end_orphaned_work_source_roots(&mut entries, |session| {
        session == "session-live"
    }));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].named_by_session, "session-live");
}

#[test]
fn a_sealed_revision_stands_in_only_when_the_root_is_gone() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    let place = EngramBasisPlace::Named {
        root: root.clone(),
        common_dir_key,
    };
    let sealed = (root.clone(), "content-v1:sealed".to_owned());
    let captured = EngramExecutionSourceBasis {
        workspace_id: root.clone(),
        source_revision: "content-v1:captured".to_owned(),
    };
    assert_eq!(
        engram_turn_end_basis(&place, Some(&sealed), Some(captured.clone())),
        Some(captured),
        "a live capture wins"
    );
    assert_eq!(
        engram_turn_end_basis(&place, Some(&sealed), None),
        None,
        "a root that exists but was not measured (the budget, the limit) has no basis"
    );
    fs::remove_dir_all(&project.worktree).unwrap();
    assert_eq!(
        engram_turn_end_basis(&place, Some(&sealed), None),
        Some(EngramExecutionSourceBasis {
            workspace_id: root,
            source_revision: "content-v1:sealed".to_owned(),
        }),
        "a root that is gone is measured by its seal"
    );
    assert_eq!(
        engram_turn_end_basis(
            &EngramBasisPlace::Workdir(project.root.to_string_lossy().into_owned()),
            Some(&sealed),
            None
        ),
        None,
        "a seal never stands in for the workdir"
    );
}

#[test]
fn an_evaluation_uses_the_root_of_its_requested_claim() {
    let entries = vec![entry("a", "claim-1", "session-1", 3)];
    let claim = AcceptanceEvaluationSourceClaim {
        work_id: "work-a".to_owned(),
        claim_id: "claim-1".to_owned(),
    };
    let root = engram_evaluation_source_root(&entries, &store(), Some(&claim), "w-a", None)
        .expect("matched on the short ref when the receipt has no work id");
    assert_eq!(root.generation, 3);
    assert!(root.still_named(&entries, &store()));
    assert!(
        engram_evaluation_source_root(&entries, &store(), Some(&claim), "w-other", None).is_none(),
        "the evaluation of another work is not measured there"
    );
    assert!(
        engram_evaluation_source_root(&entries, &store(), Some(&claim), "w-a", Some("work-other"))
            .is_none(),
        "a work id, when the receipt carries one, decides"
    );
    let other_claim = AcceptanceEvaluationSourceClaim {
        claim_id: "claim-2".to_owned(),
        ..claim.clone()
    };
    assert!(
        engram_evaluation_source_root(&entries, &store(), Some(&other_claim), "w-a", None)
            .is_none(),
        "a different requested claim cannot inherit this named root"
    );
    assert!(engram_evaluation_source_root(&entries, &store(), None, "w-a", None).is_none());
    let renamed = vec![entry("a", "claim-1", "session-1", 4)];
    assert!(
        !root.still_named(&renamed, &store()),
        "a new generation refuses the old evaluation's submission"
    );
}

#[test]
fn the_display_path_drops_the_verbatim_prefix() {
    assert_eq!(
        engram_source_root_display(r"\\?\C:\github\Personal\Engram"),
        r"C:\github\Personal\Engram"
    );
    assert_eq!(engram_source_root_display(r"\\?\UNC\server\share\x"), r"\\server\share\x");
    assert_eq!(engram_source_root_display("/home/me/project"), "/home/me/project");
}

#[test]
fn a_test_is_credited_to_a_named_root_only_when_it_ran_there() {
    // The command is read from the session's workdir (the main checkout),
    // where its runtime runs it; it counts only when it ran in the root.
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    let check = engram_check_command("cargo test").expect("a recognised test");
    let credit_root = Some((FsPath::new(&root), common_dir_key.as_str()));
    let target = engram_check_worktree_in(&check, &project.root, Some(".worktrees/wt"), credit_root)
        .expect("a test run in the named root is credited to it");
    assert_eq!(target.root, PathBuf::from(&root));
    assert_eq!(target.common_dir_key.as_deref(), Some(common_dir_key.as_str()));
    assert!(
        matches!(target.basis_place(), EngramBasisPlace::Named { .. }),
        "its snapshots are taken on exactly the named path"
    );
    assert!(
        engram_check_worktree_in(&check, &project.root, None, credit_root).is_none(),
        "a test run in the main checkout is not credited while a root is named"
    );
    let unnamed = engram_check_worktree(&check, &project.root, None)
        .expect("without a name the session's own worktree is credited");
    assert_eq!(unnamed.common_dir_key, None);
}

#[test]
fn an_evaluation_whose_work_was_renamed_is_refused_at_its_first_submission() {
    let state = test_app_state();
    let source_root = AcceptanceEvaluationSourceRoot::from_entry(&entry("a", "claim-1", "session-1", 1));
    // Whether or not a revision was taken at the request: a capture that
    // failed there leaves no fingerprint, and the root check still holds.
    for source_fingerprint in [Some("content-v1:requested".to_owned()), None] {
        for current in [vec![entry("a", "claim-1", "session-1", 2)], Vec::new()] {
            state
                .inner
                .lock()
                .expect("state mutex poisoned")
                .engram_work_source_roots = current.clone();
            let target = DelegationAcceptanceEvaluation {
                work_ref: "w-a".to_owned(),
                mode: AcceptanceEvaluationMode::IndependentSession,
                acceptance_basis: 1,
                evidence_basis: 1,
                criteria_count: 1,
                bindings: Vec::new(),
                attempt_key: "delegation-a".to_owned(),
                store: Some(store()),
                source_fingerprint: source_fingerprint.clone(),
                source_root: Some(source_root.clone()),
                source_claim: None,
                submission: AcceptanceEvaluationSubmission::None,
            };
            let error = state
                .acceptance_evaluation_declared_target("session-evaluator", &target)
                .expect_err("a renamed or cleared root refuses the submission");
            assert_eq!(error.status, StatusCode::CONFLICT, "{source_fingerprint:?} {current:?}");
            assert!(error.message.contains("source root changed"), "{}", error.message);
        }
    }
}

#[test]
fn an_entry_survives_a_restart_and_one_whose_session_did_not_ends() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(
            Agent::Codex,
            Some("Namer".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        )
        .session
        .id
        .clone();
    inner.engram_work_source_roots = vec![
        entry("kept", "claim-kept", &session_id, 1),
        entry("orphan", "claim-orphan", "a-session-that-is-gone", 2),
        entry("quarantined", "claim-quarantined", "session-quarantined", 1),
    ];
    // A state written before the counter was kept.
    inner.engram_source_root_generation = 0;

    let encoded =
        serde_json::to_vec(&PersistedState::from_inner(&inner)).expect("the state should encode");
    let mut persisted =
        serde_json::from_slice::<PersistedState>(&encoded).expect("the state should decode");
    // A session whose row failed validation is kept aside, not gone: it may
    // load again once its row is repaired. The load marks it so, as the
    // SQLite reader does.
    persisted
        .quarantined_persisted_session_ids
        .insert("session-quarantined".to_owned());
    let restored = persisted.into_inner().expect("the state should restore");

    assert_eq!(
        restored.engram_work_source_roots,
        vec![
            entry("kept", "claim-kept", &session_id, 1),
            entry("quarantined", "claim-quarantined", "session-quarantined", 1),
        ],
        "the entries of a live and of a quarantined session are kept whole; the orphan is ended"
    );
    assert_eq!(
        restored.engram_source_root_generation, 2,
        "the next name is numbered above every stored one, the orphan's too"
    );
}

#[test]
fn a_root_on_a_network_share_is_refused() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    for path in ["//server/share/wt", r"\\server\share\wt"] {
        let error = validate(FsPath::new(path), &project).expect_err("a share is refused");
        assert!(error.contains("network share"), "{path}: {error}");
    }
}

#[test]
fn a_held_list_with_a_row_that_names_no_claim_ends_nothing() {
    let mut entries = vec![entry("live", "claim-live", "session-1", 1)];
    let known = entries.clone();
    let mut no_ids = held(&["claim-other"], 0);
    no_ids.items.push(EngramHeldClaim::default());
    assert!(
        !engram_end_released_work_source_roots(&mut entries, &known, "session-1", &store(), &no_ids),
        "the unnamed row may be the live claim"
    );
    assert_eq!(entries.len(), 1);
}

#[test]
fn a_validation_that_outlasts_its_budget_names_nothing_and_keeps_its_place() {
    // A counter of its own, so no other test's checks share the cap.
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let (ended_tx, ended) = std::sync::mpsc::channel::<()>();
    let outcome = engram_run_source_root_validation_within(
        &live,
        1,
        Duration::from_millis(20),
        "slow".to_owned(),
        move || {
            let _ = blocked.recv();
            let _ = ended_tx.send(());
            Ok(("root".to_owned(), "key".to_owned()))
        },
    );
    assert!(
        matches!(outcome, Err(EngramSourceRootValidation::OutOfTime(ref message)) if message.contains("nothing was named")),
        "{outcome:?}"
    );
    // The stalled check still holds the one place: the next is refused
    // without starting a thread.
    let refused = engram_run_source_root_validation_within(
        &live,
        1,
        Duration::from_secs(60),
        "next".to_owned(),
        || panic!("no check starts while the place is held"),
    );
    assert!(
        matches!(refused, Err(EngramSourceRootValidation::OutOfTime(ref message)) if message.contains("still running")),
        "{refused:?}"
    );
    drop(release);
    ended
        .recv_timeout(Duration::from_secs(60))
        .expect("the stalled check ends once released");
    // Its place is given back as its thread ends.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while live.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        assert!(std::time::Instant::now() < deadline, "the place is given back");
        std::thread::yield_now();
    }
    assert_eq!(
        engram_run_source_root_validation_within(
            &live,
            1,
            Duration::from_secs(60),
            "bad".to_owned(),
            || Err("not a worktree".to_owned()),
        ),
        Err(EngramSourceRootValidation::Refused("not a worktree".to_owned()))
    );
}

#[test]
fn a_drive_relative_path_is_refused_before_it_is_joined() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    if cfg!(windows) {
        let error = validate(FsPath::new("C:wt"), &project).expect_err("drive-relative");
        assert!(error.contains("relative to a drive's current folder"), "{error}");
    } else {
        // No drive prefixes: such a name is an ordinary relative path.
        let error = validate(FsPath::new("C:wt"), &project).expect_err("not a worktree");
        assert!(!error.contains("drive's current folder"), "{error}");
    }
}

#[test]
fn only_a_lookup_that_says_missing_makes_a_root_absent() {
    use std::io::ErrorKind;
    assert!(engram_lookup_says_missing(ErrorKind::NotFound));
    assert!(engram_lookup_says_missing(ErrorKind::NotADirectory));
    for kind in [ErrorKind::PermissionDenied, ErrorKind::Other, ErrorKind::TimedOut] {
        assert!(
            !engram_lookup_says_missing(kind),
            "{kind:?} leaves a root that may still exist unmeasured"
        );
    }
}

#[test]
fn a_bounded_closing_capture_measures_a_gone_root_by_its_seal() {
    let mut cleanup = RemoveDirsOnDrop(Vec::new());
    let project = project(&mut cleanup);
    let (root, common_dir_key) = validate(&project.worktree, &project).unwrap();
    let place = EngramBasisPlace::Named {
        root: root.clone(),
        common_dir_key,
    };
    let sealed = (root.clone(), "content-v1:sealed".to_owned());
    let state = test_app_state();
    let live = state
        .engram_turn_end_basis_within(&place, Some(&sealed), Duration::from_secs(60))
        .expect("a live root is captured");
    assert_ne!(live.source_revision, "content-v1:sealed", "a live capture wins");

    fs::remove_dir_all(&project.worktree).unwrap();
    assert_eq!(
        state.engram_turn_end_basis_within(&place, Some(&sealed), Duration::from_secs(60)),
        Some(EngramExecutionSourceBasis {
            workspace_id: root,
            source_revision: "content-v1:sealed".to_owned(),
        }),
        "the capture and the absence check ran inside the bound"
    );
}

#[test]
fn pending_source_root_lines_accumulate_without_repeats_up_to_their_cap() {
    let mut engram = EngramSessionState::default();
    engram.set_pending_source_root_line("bind".to_owned());
    engram.set_pending_source_root_line("withheld".to_owned());
    engram.set_pending_source_root_line("bind".to_owned());
    let pending = |engram: &EngramSessionState| {
        engram
            .pending_source_root_line
            .as_deref()
            .map(|pending| pending.lines().map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    assert_eq!(
        pending(&engram),
        ["withheld", "bind"],
        "a later line does not hide an earlier one; a repeat moves to the newest place"
    );
    for line in ["a", "b", "c"] {
        engram.set_pending_source_root_line(line.to_owned());
    }
    assert_eq!(
        pending(&engram),
        ["bind", "a", "b", "c"],
        "only the newest are kept"
    );

    // A path with a line break in it (Linux and macOS allow one) stays one
    // line, so it is deduplicated and acknowledged whole.
    let mut engram = EngramSessionState::default();
    engram.set_pending_source_root_line("named /repo/odd\nname".to_owned());
    engram.set_pending_source_root_line("named /repo/odd\r\nname".to_owned());
    engram.set_pending_source_root_line("named /repo/odd\nname".to_owned());
    assert_eq!(
        pending(&engram),
        ["named /repo/odd  name", "named /repo/odd name"],
        "each line stays whole, and a repeat is still one line"
    );
}
