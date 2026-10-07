// Owns the tests of what may overlap a check at its launch and why the holder
// is told it did: another session in a turn counts only where it may write,
// a session whose worktree was not yet resolved is resolved before it is
// counted, a read-only delegation child never counts, and the refusal names
// the session or command that made it. Does not own the carried gate's other
// refusals (src/tests/engram_carried_checks.rs, whose fixture and helpers
// this child module uses) or the in-turn check tests
// (src/tests/engram_turn_checks.rs). New module.
use super::*;

/// A read-only reviewer child of the holder, working in `worktree` and in a
/// turn, with a command still running there that only reads.
fn read_only_child_reading_in(turn: &CheckedTurn, worktree: &FsPath) -> String {
    let (_, child) = install_required_review_delegation(&turn.state, &turn.session_id);
    let cwd = fs::canonicalize(worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned();
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&child).expect("child session");
        inner.sessions[index].session.workdir = cwd.clone();
    }
    turn.state.note_engram_session_worktree_off_lock(&child);
    set_status(turn, &child, SessionStatus::Active);
    SessionRecorder::new(turn.state.clone(), child.clone())
        .command_started_in("child-read", "git diff", Some("git diff"), Some(&cwd))
        .expect("the child's command should record");
    child
}

fn set_status(turn: &CheckedTurn, session_id: &str, status: SessionStatus) {
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session_id).expect("session");
    inner.sessions[index].session.status = status;
}

fn session_name(turn: &CheckedTurn, session_id: &str) -> String {
    let inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(session_id).expect("session");
    inner.sessions[index].session.name.clone()
}

#[test]
fn a_lone_launch_beside_a_reading_read_only_child_is_carried() {
    // The reviewers of a change read the worktree its gate runs in; they
    // cannot write, so they overlap nothing, nor do the watcher's changes in
    // directories it ignores.
    let (turn, worktree) = named_turn("overlap-read-only-child");
    read_only_child_reading_in(&turn, &worktree);
    launch_gate(&turn, &worktree, false);
    assert_eq!(carried_fence(&turn), None);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    let change = |path: PathBuf| WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    };
    // A build's output under target/ never reaches the check: the watcher
    // drops it before it reports a change.
    assert!(is_ignored_workspace_file_event_path(
        &canonical.join("target").join("debug").join("termal.d")
    ));
    // The worktree's own scratch directory reaches it and fences nothing.
    turn.state
        .note_engram_workspace_file_changes(&[change(canonical.join(".tmp").join("reviewer.log"))]);
    assert_eq!(carried_fence(&turn), None);
}

#[test]
fn a_writable_session_in_a_turn_elsewhere_not_yet_resolved_leaves_a_lone_launch_carried() {
    // A writable session in a turn whose worktree TermAl has not resolved
    // (one whose session state was rebuilt, say) is resolved before it is
    // counted, so its turn in another worktree overlaps nothing here. The
    // read-only child reading in this worktree overlaps nothing either.
    let (turn, worktree) = named_turn("overlap-unresolved-elsewhere");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "overlap-unresolved-elsewhere-sibling");
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &elsewhere);
    set_status(&turn, &other, SessionStatus::Active);
    read_only_child_reading_in(&turn, &worktree);
    launch_gate(&turn, &worktree, false);
    assert_eq!(carried_fence(&turn), None);
}

#[test]
fn a_remote_proxy_in_a_turn_is_never_resolved_and_is_named_as_such() {
    // A proxy's workdir is the other host's path: TermAl does not resolve it
    // here, so the proxy keeps counting in every worktree, and the holder is
    // told it was an unresolved remote proxy.
    let (turn, worktree) = named_turn("overlap-remote-proxy");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "overlap-remote-proxy-sibling");
    let proxy = create_test_project_session(&turn.state, Agent::Codex, &project_id, &elsewhere);
    {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&proxy).expect("proxy session");
        inner.sessions[index].remote_id = Some("ssh-test".to_owned());
    }
    set_status(&turn, &proxy, SessionStatus::Active);
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    assert!(
        line.contains(&proxy)
            && line.contains("a remote proxy of remote ssh-test, has an UNRESOLVED workdir"),
        "{line}"
    );
    let resolved = {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&proxy).expect("proxy session");
        engram_session_worktree(&inner.sessions[index])
    };
    assert_eq!(resolved, None, "the proxy's workdir is never resolved");
}

#[test]
fn a_launch_beside_a_writable_session_in_a_turn_there_names_that_session() {
    // The control: the same session working in this worktree overlaps the
    // launch, and the holder is told which session it was and why it matched.
    let (turn, worktree) = named_turn("overlap-named-writer");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    set_status(&turn, &other, SessionStatus::Active);
    read_only_child_reading_in(&turn, &worktree);
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    let name = session_name(&turn, &other);
    assert!(
        line.contains(&other) && line.contains(&name) && line.contains("works in this worktree"),
        "{line}"
    );
}

#[test]
fn a_writer_is_told_by_the_place_it_matched() {
    // Each place a session in a turn may write in, as the holder is told it.
    let (turn, worktree) = named_turn("overlap-writer-places");
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let elsewhere = sibling_worktree(&turn, "overlap-writer-places-sibling");
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &elsewhere);
    let root = engram_path_key(&worktree);
    let reason = |turn: &CheckedTurn| {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        engram_writer_match(&inner, index, &root)
    };
    let edit = |turn: &CheckedTurn, change: &dyn Fn(&mut SessionRecord)| {
        let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&other).expect("other session");
        change(&mut inner.sessions[index]);
    };
    let told = |reason: Option<String>, why: &str| {
        let reason = reason.expect("the session matches");
        assert!(reason.contains(&other) && reason.contains(why), "{reason}");
    };

    // Not resolved yet: it counts in every worktree.
    told(reason(&turn), "has an UNRESOLVED worktree");
    // Resolved elsewhere, it does not match.
    turn.state.note_engram_session_worktree_off_lock(&other);
    assert_eq!(reason(&turn), None);
    // Its claim's named source root is this worktree.
    edit(&turn, &|record| {
        record.engram.active_grant_id = Some("other-grant".to_owned());
        record.engram.active_turn_source_root = Some(EngramTurnSourceRoot {
            root: worktree.to_string_lossy().into_owned(),
            common_dir_key: String::new(),
            short_ref: "other-work".to_owned(),
            claim_id: "other-claim".to_owned(),
            generation: 1,
            sealed_revision: None,
        });
    });
    told(
        reason(&turn),
        "works in it as its claim's named source root",
    );
    edit(&turn, &|record| record.engram.active_grant_id = None);
    // A command it runs here, or one TermAl could not place.
    edit(&turn, &|record| {
        record.engram.running_command_worktrees =
            vec![("here".to_owned(), vec![Some(root.clone())])];
    });
    told(reason(&turn), "runs a command in it");
    edit(&turn, &|record| {
        record.engram.running_command_worktrees = vec![("lost".to_owned(), vec![None])];
    });
    told(reason(&turn), "runs a command TermAl could not place");
}

#[test]
fn a_later_writer_a_termal_write_and_a_late_name_are_each_told() {
    let (turn, worktree) = named_turn("overlap-later-causes");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let cause = |turn: &CheckedTurn| {
        turn.record(|record| record.engram.active_turn_checks[0].overlap_cause.clone())
            .expect("the check names what overlapped it")
    };

    // Named a test only by a later start of its command.
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("late", "Run the tests", None, Some(&cwd))
        .expect("the title should record");
    recorder
        .command_started_in("late", SIZE_TEST, Some(SIZE_TEST), Some(&cwd))
        .expect("the line should record");
    // The check's opening snapshot reads the worktree on its own thread; it
    // must finish before the fixture removes its directory.
    turn.wait_for_snapshots();
    assert!(
        cause(&turn).contains("named a test only by a later start"),
        "{}",
        cause(&turn)
    );

    // A write TermAl made in its worktree.
    turn.record_mut(|record| {
        let check = &mut record.engram.active_turn_checks[0];
        check.overlapped = false;
        check.overlap_cause = None;
    });
    let written = fs::canonicalize(&worktree).unwrap().join("notes.md");
    turn.state.note_engram_host_write(&written);
    assert!(cause(&turn).contains("TermAl wrote"), "{}", cause(&turn));

    // Another session that may write, in a turn where the check runs.
    turn.record_mut(|record| {
        let check = &mut record.engram.active_turn_checks[0];
        check.overlapped = false;
        check.overlap_cause = None;
    });
    let project_id = turn.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id, &worktree);
    turn.state.note_engram_session_worktree_off_lock(&other);
    // Each act is told with the writer and why it matched the worktree.
    let placed = [Some(engram_path_key(&worktree))];
    for (act, did) in [
        (EngramWriterAct::Presence, "and it was in a turn"),
        (
            EngramWriterAct::Command {
                worktrees: &placed,
                ran: Some("touch notes.md"),
                unplaced: false,
            },
            "and it reported a command",
        ),
        (
            EngramWriterAct::UnreportedCommand,
            "and it ended a command whose start TermAl was not told",
        ),
        (EngramWriterAct::Edit, "and it reported an edit"),
    ] {
        turn.record_mut(|record| {
            let check = &mut record.engram.active_turn_checks[0];
            check.overlapped = false;
            check.overlap_cause = None;
        });
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            engram_mark_checks_overlapped_by(&mut inner, index, act);
        }
        let told = cause(&turn);
        assert!(
            told.contains(&other)
                && told.contains("works in this worktree")
                && told.contains(did)
                && !told.contains("touch notes.md"),
            "{told}"
        );
    }
}

#[test]
fn a_withheld_check_line_names_what_overlapped_it() {
    let withheld = |overlap_cause: Option<&str>| EngramWithheldCheck {
        kind: EngramVerificationKind::Test,
        program: "cargo".to_owned(),
        fingerprint: "f".repeat(64),
        reason: EngramWithheldReason::Overlapped,
        cause: None,
        overlap_cause: overlap_cause.map(str::to_owned),
    };
    let line = engram_withheld_check_line(&withheld(Some("its own command k started")));
    assert!(
        line.contains("reached its worktree while it ran (its own command k started)."),
        "{line}"
    );
    // A check nothing named keeps the reason alone.
    let line = engram_withheld_check_line(&withheld(None));
    assert!(
        line.contains("reached its worktree while it ran. Run it again"),
        "{line}"
    );
}

#[test]
fn a_launch_beside_a_command_of_its_own_still_running_names_that_command() {
    // The holder's own command still running when the gate launched may
    // write under it; the refusal names that command by a digest of its
    // key. A runtime that gives no id keys a command by its whole line,
    // which can carry a secret, so the key itself is never told.
    let (turn, worktree) = named_turn("overlap-own-command");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let keyed_by_line = "API_TOKEN=sentinel-secret-7731 npm run watch";
    turn.recorder()
        .command_started_in(
            keyed_by_line,
            keyed_by_line,
            Some(keyed_by_line),
            Some(&cwd),
        )
        .expect("the earlier command should record");
    launch_command(&turn, &worktree, GATE, EngramCommandExit::NotFinished);
    assert_dropped_at_launch(&turn, "another command or another writable session");
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder should be told");
    let digest = &sha256_hex(keyed_by_line.as_bytes())[..12];
    assert!(
        line.contains(&format!("its own command (key {digest}) was still running"))
            && !line.contains("sentinel-secret-7731"),
        "{line}"
    );
}

#[test]
fn an_own_command_starting_under_an_open_check_is_named_by_digest_only() {
    // A command of the holder that starts while its check is open is told by
    // the digest of its key too.
    let (turn, worktree) = named_turn("overlap-own-command-later");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("check", SIZE_TEST, Some(SIZE_TEST), Some(&cwd))
        .expect("the check should record");
    // As above: the opening snapshot finishes before the fixture is removed.
    turn.wait_for_snapshots();
    let keyed_by_line = "curl -H 'Authorization: Bearer sentinel-secret-9902' x";
    recorder
        .command_started_in(
            keyed_by_line,
            keyed_by_line,
            Some(keyed_by_line),
            Some(&cwd),
        )
        .expect("the later command should record");
    let cause = turn
        .record(|record| record.engram.active_turn_checks[0].overlap_cause.clone())
        .expect("the check names what overlapped it");
    let digest = &sha256_hex(keyed_by_line.as_bytes())[..12];
    assert!(
        cause == format!("its own command (key {digest}) started")
            && !cause.contains("sentinel-secret-9902"),
        "{cause}"
    );
}
