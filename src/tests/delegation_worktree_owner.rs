//! A follow-up runs only in the worktree its delegation created: TermAl
//! records the owner when it makes the worktree, and refuses a follow-up once
//! the worktree has been removed by hand or its path holds anything else.
//! Does not own where the worktree lives (delegation_worktree_location.rs) or
//! worktree creation (delegations.rs). Carved out of the tests of unlanded
//! work on removing ended worktrees automatically, keeping only the owner
//! record and the follow-up refusal.

use super::delegation_worker::WorkerFixture;
use super::delegation_worktree_location::{WorktreeRepo, same_path};
use super::*;

/// Whether Git still lists a worktree at this path.
fn git_lists_worktree(repo: &FsPath, worktree: &FsPath) -> bool {
    let listed = run_git_test_command_output(repo, &["worktree", "list", "--porcelain"]);
    listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| same_path(FsPath::new(path), worktree))
}

/// The worktree's Git administrative directory, exactly as Git prints it:
/// only the line ending is removed, so a name ending in other whitespace
/// keeps it and never names a sibling's directory.
fn admin_dir(worktree: &FsPath) -> PathBuf {
    let output = git_command()
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .expect("git rev-parse");
    assert!(
        output.status.success(),
        "git rev-parse --absolute-git-dir failed"
    );
    let text = String::from_utf8(output.stdout).expect("UTF-8 administrative path");
    let line = text
        .strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .unwrap_or(&text);
    PathBuf::from(line)
}

impl WorktreeRepo {
    /// Spawns into a named directory under the ignored `.worktrees/`. Cases
    /// run in this process never use the default path, which on a host that
    /// still resolved it through the home would write to the real one.
    fn spawn_inside(&self, name: &str) -> Result<DelegationResponse, ApiError> {
        self.spawn(Some(&self.repo.join(".worktrees").join(name)))
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

    fn follow_up(&self, created: &DelegationResponse) -> Result<(), ApiError> {
        self.state
            .followup_delegation(
                &self.parent,
                &created.delegation.id,
                "one more look".to_owned(),
            )
            .map(|_| ())
    }

    /// Removes a worktree by hand, as a person does once its work is done.
    fn remove_by_hand(&self, worktree: &FsPath) {
        let path = worktree.to_string_lossy().into_owned();
        run_git_test_command(&self.repo, &["worktree", "remove", path.as_str()]);
    }
}

fn assert_refused(error: ApiError) {
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("is gone, is no longer the one it created"),
        "{}",
        error.message
    );
}

/// The owner record Git keeps for this worktree, read through Git's own
/// answer for its administrative directory.
fn recorded_owner(worktree: &FsPath) -> String {
    fs::read_to_string(admin_dir(worktree).join("termal-delegation")).expect("owner record")
}

#[test]
fn a_worktree_records_the_delegation_that_created_it() {
    let fixture = WorktreeRepo::new("worktree-owner-record", true);
    let created = fixture.spawn_inside("owned").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    assert_eq!(recorded_owner(&worktree), created.delegation.id);
    assert_eq!(
        delegation_worktree_owner(&worktree),
        DelegationWorktreeOwner::Recorded(created.delegation.id.clone())
    );
}

#[test]
fn sibling_worktrees_whose_names_differ_by_trailing_whitespace_keep_their_own_owners() {
    let fixture = WorktreeRepo::new("worktree-owner-nbsp-sibling", true);
    let plain = fixture.spawn_inside("slot").expect("first delegation");
    // The same name with a trailing no-break space; the final separator keeps
    // the request from trimming it.
    let spaced = fixture
        .spawn(Some(FsPath::new(&format!(
            "{}{}",
            fixture.repo.join(".worktrees").join("slot\u{a0}").display(),
            std::path::MAIN_SEPARATOR
        ))))
        .expect("second delegation");
    let (plain_worktree, spaced_worktree) = (
        WorktreeRepo::worktree_of(&plain),
        WorktreeRepo::worktree_of(&spaced),
    );
    assert!(!same_path(&plain_worktree, &spaced_worktree));
    // Git gives each its own administrative directory; read back exactly.
    assert_ne!(admin_dir(&plain_worktree), admin_dir(&spaced_worktree));
    assert_eq!(recorded_owner(&plain_worktree), plain.delegation.id);
    assert_eq!(recorded_owner(&spaced_worktree), spaced.delegation.id);
    for created in [&plain, &spaced] {
        fixture.complete(created);
        fixture
            .follow_up(created)
            .expect("each worktree keeps its own owner and follow-ups");
    }
}

#[test]
fn an_owner_record_is_never_written_over_another() {
    let fixture = WorktreeRepo::new("worktree-owner-no-overwrite", true);
    let created = fixture
        .spawn_inside("recorded")
        .expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    let error = record_delegation_worktree_owner(&worktree, "another-delegation")
        .expect_err("an existing owner record is not overwritten");
    assert!(
        error.message.contains("already records an owner"),
        "{}",
        error.message
    );
    assert_eq!(recorded_owner(&worktree), created.delegation.id);
}

#[test]
fn a_worker_base_worktree_records_the_delegation_that_created_it() {
    let fixture = WorkerFixture::new();
    let source = fixture.source.to_string_lossy().into_owned();
    let target = fixture.worktree.to_string_lossy().into_owned();
    prepare_isolated_delegation_worktree(
        &source,
        Some(target.as_str()),
        "delegation-worker-owner",
        Some("worker-base"),
    )
    .expect("worker worktree");
    assert_eq!(recorded_owner(&fixture.worktree), "delegation-worker-owner");
}

#[test]
fn a_followup_is_refused_when_the_owner_record_is_not_text() {
    let fixture = WorktreeRepo::new("worktree-followup-owner-bytes", true);
    let created = fixture.spawn_inside("bytes").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    // A record no retry can read is not "busy": the follow-up is told to
    // start a new delegation, never to retry.
    fs::write(
        admin_dir(&worktree).join("termal-delegation"),
        [0xff, 0xfe, 0xfd],
    )
    .expect("unreadable record");
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("a record that is not text names no owner"),
    );
}

#[test]
fn a_followup_runs_in_the_worktree_its_delegation_created() {
    let fixture = WorktreeRepo::new("worktree-followup-own", true);
    let created = fixture.spawn_inside("own").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fs::write(worktree.join("README.md"), "child edit\n").expect("child edit");
    fixture.complete(&created);
    fixture
        .follow_up(&created)
        .expect("the worktree it created keeps follow-ups available");
}

#[test]
fn a_followup_runs_when_the_repository_is_on_a_branch_with_unicode_whitespace() {
    let fixture = WorktreeRepo::new("worktree-followup-nbsp-branch", true);
    // Git accepts a no-break space in a branch name; the repository's HEAD
    // names that branch, and the check does not judge what a HEAD names.
    run_git_test_command(
        &fixture.repo,
        &["checkout", "-q", "-b", "release\u{a0}candidate"],
    );
    let created = fixture.spawn_inside("nbsp").expect("isolated delegation");
    fixture.complete(&created);
    fixture
        .follow_up(&created)
        .expect("a branch name Git accepts keeps follow-ups available");
}

#[test]
fn a_followup_on_a_delegation_whose_worktree_was_removed_is_refused() {
    let fixture = WorktreeRepo::new("worktree-followup-removed", true);
    let created = fixture
        .spawn_inside("removed")
        .expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    fixture.remove_by_hand(&worktree);
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("no worktree to run in"),
    );
    assert!(!worktree.exists(), "the worktree is not recreated");
}

#[test]
fn a_followup_is_refused_in_a_worktree_a_person_made_at_the_path() {
    let fixture = WorktreeRepo::new("worktree-followup-person", true);
    let created = fixture.spawn_inside("manual").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    fixture.remove_by_hand(&worktree);
    // Someone makes a worktree of their own at the freed path.
    let path = worktree.to_string_lossy().into_owned();
    run_git_test_command(
        &fixture.repo,
        &["worktree", "add", "--detach", path.as_str(), "HEAD"],
    );
    fs::write(worktree.join("README.md"), "their work\n").expect("their work");
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("not the worktree it created"),
    );
    assert_eq!(
        fs::read_to_string(worktree.join("README.md")).expect("their work"),
        "their work\n",
        "their worktree is left alone"
    );
}

#[test]
fn a_followup_is_refused_in_a_worktree_another_delegation_made_at_the_path() {
    let fixture = WorktreeRepo::new("worktree-followup-reused", true);
    let first = fixture.spawn_inside("shared").expect("first delegation");
    let path = WorktreeRepo::worktree_of(&first);
    fixture.complete(&first);
    fixture.remove_by_hand(&path);
    let second = fixture
        .spawn_inside("shared")
        .expect("a later delegation may use the freed path");
    assert!(same_path(&WorktreeRepo::worktree_of(&second), &path));
    assert_refused(
        fixture
            .follow_up(&first)
            .expect_err("a follow-up must not run in another delegation's worktree"),
    );
}

#[test]
fn a_followup_is_refused_in_a_half_removed_worktree() {
    let fixture = WorktreeRepo::new("worktree-followup-broken-link", true);
    let created = fixture.spawn_inside("broken").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    // A removal that stopped part-way: the directory and its owner record are
    // still there, but Git no longer links the worktree.
    fs::remove_file(admin_dir(&worktree).join("HEAD")).expect("break the link");
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("a worktree Git no longer links is not run in"),
    );
}

#[test]
fn a_followup_is_refused_when_the_worktree_lost_its_common_directory_link() {
    let fixture = WorktreeRepo::new("worktree-followup-no-commondir", true);
    let created = fixture
        .spawn_inside("nocommon")
        .expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    // HEAD and the owner record survive, but Git can no longer reach the
    // repository's objects and refs from this worktree.
    fs::remove_file(admin_dir(&worktree).join("commondir")).expect("break the common link");
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("a worktree without its common directory is not run in"),
    );
}

#[test]
fn a_followup_is_refused_at_a_path_its_worktree_was_moved_away_from() {
    let fixture = WorktreeRepo::new("worktree-followup-moved", true);
    let created = fixture.spawn_inside("before").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    let marker = fs::read_to_string(worktree.join(".git")).expect("worktree marker");
    let moved = fixture.repo.join(".worktrees").join("after");
    let (from, to) = (
        worktree.to_string_lossy().into_owned(),
        moved.to_string_lossy().into_owned(),
    );
    run_git_test_command(
        &fixture.repo,
        &["worktree", "move", from.as_str(), to.as_str()],
    );
    // A stale copy of the old `.git` file at the old path names the same
    // administrative directory, HEAD and owner, but Git links that directory
    // to the new path.
    fs::create_dir_all(&worktree).expect("old path again");
    fs::write(worktree.join(".git"), marker).expect("stale marker");
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("a worktree Git links elsewhere is not run in"),
    );
    assert!(
        moved.join("README.md").exists(),
        "the moved worktree is left alone"
    );
}

#[test]
fn a_failed_owner_record_fails_the_spawn_and_removes_its_worktree() {
    let fixture = WorktreeRepo::new("worktree-owner-record-fails", true);
    let target = fixture.repo.join(".worktrees").join("unrecorded");
    TEST_FAIL_NEXT_DELEGATION_WORKTREE_OWNER_RECORD.with(|fail| fail.set(true));
    let error = fixture
        .spawn_inside("unrecorded")
        .err()
        .expect("a worktree whose owner cannot be recorded is not handed out");
    assert!(
        error.message.contains("failed to record the owner"),
        "{}",
        error.message
    );
    assert!(!target.exists(), "the worktree is removed");
    assert!(
        !git_lists_worktree(&fixture.repo, &target),
        "Git no longer lists it"
    );
}

#[test]
fn a_failed_owner_record_on_a_worker_base_removes_its_worktree() {
    let fixture = WorkerFixture::new();
    let source = fixture.source.to_string_lossy().into_owned();
    let target = fixture.worktree.to_string_lossy().into_owned();
    TEST_FAIL_NEXT_DELEGATION_WORKTREE_OWNER_RECORD.with(|fail| fail.set(true));
    let error = prepare_isolated_delegation_worktree(
        &source,
        Some(target.as_str()),
        "delegation-owner-record-fails",
        Some("worker-base"),
    )
    .err()
    .expect("a worker worktree whose owner cannot be recorded is not handed out");
    assert!(
        error.message.contains("failed to record the owner"),
        "{}",
        error.message
    );
    assert!(!fixture.worktree.exists(), "the worktree is removed");
    assert!(
        !git_lists_worktree(&fixture.source, &fixture.worktree),
        "Git no longer lists it"
    );
}

#[test]
fn a_followup_is_refused_when_the_worktree_goes_after_the_first_check() {
    let fixture = WorktreeRepo::new("worktree-followup-second-check", true);
    let created = fixture.spawn_inside("racing").expect("isolated delegation");
    let worktree = WorktreeRepo::worktree_of(&created);
    fixture.complete(&created);
    // Removed after the check before restoration, before the lock that
    // admits the follow-up: only the second check can refuse it.
    let (repo, path) = (fixture.repo.clone(), worktree.clone());
    TEST_REVIEW_BEFORE_FOLLOWUP_RESERVATION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let path = path.to_string_lossy().into_owned();
            run_git_test_command(&repo, &["worktree", "remove", path.as_str()]);
        }));
    });
    assert_refused(
        fixture
            .follow_up(&created)
            .expect_err("the second check, just before the admitting lock, refuses it"),
    );
    assert!(!worktree.exists(), "the worktree is not recreated");
}
