// A Codex test still running when its turn closed: carried until Codex reports
// its end, which comes when its process exits, tagged with the turn that
// started it (src/engram_reported_ends.rs).
//
// Owns: the end-to-end tests of that carry. The late end arrives through the
// shared Codex app-server path (`handle_shared_codex_app_server_message`) as
// the dropped `item/completed` of a finished turn, or as one the next turn's
// routing would take, and is credited at the next checkpoint on the claim; a command that ends within its turn is
// recorded once, as before; only the exact item key is taken; a command of
// the holder in the worktree before the late end refuses it; a check started
// in the worktree before the late end is overlapped by it, even once its
// evidence was refused, and one started after is not; its runtime's exit
// ends it; one whose end never comes stays carried across later turns,
// told as still running, never closed by a count of turns; a host restart
// before the end closes it with outcome Unknown; a command keyed by its line,
// and a Claude or ACP session's test, are closed Unknown as before.
// Does not own: the carried checks' record, fence and credit, whose tests are
// in engram_carried_checks.rs, or closing an unended check, in
// engram_turn_checks.rs and engram_unended_closures.rs.
// New file: nothing was moved into it.
use super::*;

/// The Codex item id the test's command runs under.
const ITEM: &str = "exec-late-item";
/// The Codex item id a second test, in the next turn, runs under.
const SECOND_ITEM: &str = "exec-second-item";
/// The thread the shared runtime knows the session by.
const THREAD: &str = "thread-late-end";
/// The second turn's grant.
const NEXT_GRANT: &str = "turn-check-grant-next";
/// What a passing size test prints.
const PASSING: &str = "running 3 tests\ntest result: ok. 3 passed";

/// A claimed Codex turn measured in its named worktree, with Engram answering
/// a second turn too.
fn two_turns(label: &str) -> (CheckedTurn, PathBuf) {
    let turn = CheckedTurn::start_with_opening(
        label,
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            grant_reply(NEXT_GRANT),
            begin_reply(NEXT_GRANT),
            checkpoint_reply(NEXT_GRANT),
        ],
        2,
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    (turn, worktree)
}

/// `worktree` as a runtime that reports a command's directory names it.
fn cwd(worktree: &FsPath) -> String {
    fs::canonicalize(worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned()
}

/// Starts the size test under the Codex item `key` in `worktree`, as Codex's
/// `item/started` does, and waits for its opening snapshot.
fn start_test(turn: &CheckedTurn, worktree: &FsPath, key: &str) {
    turn.recorder()
        .command_started_in(key, SIZE_TEST, Some(SIZE_TEST), Some(&cwd(worktree)))
        .expect("the start should record");
    turn.wait_for_snapshots();
}

/// The shared Codex runtime the session runs on, with the session known by
/// `THREAD`.
fn shared_runtime(turn: &CheckedTurn) -> SharedCodexRuntime {
    let runtime = turn.record(|record| match &record.runtime {
        SessionRuntime::Codex(handle) => handle
            .shared_session
            .as_ref()
            .expect("the session runs on the shared runtime")
            .runtime
            .clone(),
        _ => panic!("a Codex session"),
    });
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .entry(turn.session_id.clone())
        .or_default()
        .thread_id = Some(THREAD.to_owned());
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(THREAD.to_owned(), turn.session_id.clone());
    runtime
}

/// Codex reports, on `thread`, that the command of item `key` ended `exit`
/// in the turn `gone`, which is no longer running.
fn report_late_end(turn: &CheckedTurn, thread: &str, key: &str, exit: i64) {
    let runtime = shared_runtime(turn);
    report_late_end_on(turn, &runtime, thread, key, SIZE_TEST, exit);
}

/// `report_late_end` through `runtime`, which may no longer be the session's,
/// for the item running `command`.
fn report_late_end_on(
    turn: &CheckedTurn,
    runtime: &SharedCodexRuntime,
    thread: &str,
    key: &str,
    command: &str,
    exit: i64,
) {
    handle_shared_codex_app_server_message(
        &json!({
            "method": "item/completed",
            "params": {
                "threadId": thread,
                "turnId": "turn-gone",
                "item": {
                    "type": "commandExecution",
                    "id": key,
                    "command": command,
                    "status": if exit == 0 { "completed" } else { "failed" },
                    "exitCode": exit,
                    "aggregatedOutput": PASSING,
                },
            },
        }),
        &turn.state,
        &runtime.runtime_id,
        &Arc::new(Mutex::new(HashMap::new())),
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("the late end is handled");
}

/// Waits until the closing snapshot a carried test's late end started is
/// taken.
fn wait_for_carried_snapshots(turn: &CheckedTurn) {
    let captures = turn.record(|record| {
        record
            .engram
            .carried_checks
            .iter()
            .filter_map(|carried| carried.check.end.as_ref().map(|end| end.end_basis.clone()))
            .collect::<Vec<_>>()
    });
    for capture in captures {
        capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD);
    }
}

/// Runs the session's next turn to its close and returns its checkpoint.
fn next_turn_checkpoint(turn: &CheckedTurn) -> Value {
    dispatch_next_turn(turn);
    let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .last()
        .expect("the next turn is checkpointed")
        .request
}

/// The holder line pending now.
fn holder_line(turn: &CheckedTurn) -> String {
    turn.record(|record| record.engram.pending_source_root_line.clone())
        .unwrap_or_default()
}

/// The first turn's close carries the test it left running, records nothing
/// for it, and tells the holder.
fn assert_carried_at_close(turn: &CheckedTurn) {
    let first = turn.finish();
    assert!(first.get("verification_evidence").is_none(), "{first:#}");
    turn.record(|record| {
        assert_eq!(
            record.engram.carried_checks.len(),
            1,
            "the running test is carried"
        );
        assert!(record.engram.carried_checks[0].check.end.is_none());
        assert_eq!(record.engram.carried_checks[0].check.key, ITEM);
    });
    let line = holder_line(turn);
    assert!(
        line.contains("a Codex test still running when its turn closed")
            && !line.contains("closed with outcome Unknown")
            && !line.contains("retrying"),
        "{line}"
    );
}

/// A Codex test that outlives its turn (its exec session polled after the
/// tool's initial yield) ends after that turn closed; Codex reports the end
/// tagged with the closed turn, which the turn's events drop. The test is
/// carried at the close, takes that end, and is credited with its real
/// outcome at the next checkpoint on the claim.
#[test]
fn a_codex_test_that_outlives_its_turn_is_recorded_from_its_late_end() {
    let (turn, worktree) = two_turns("reported-end-credited");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);

    report_late_end(&turn, THREAD, ITEM, 0);
    turn.record(|record| {
        assert!(
            record.engram.carried_checks[0].check.end.is_some(),
            "the late end is taken"
        )
    });
    wait_for_carried_snapshots(&turn);
    let settling = next_turn_checkpoint(&turn);

    assert_eq!(settling["grant_id"], NEXT_GRANT, "{settling:#}");
    let evidence = settling["verification_evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("the carried test is recorded: {settling:#}"));
    assert_eq!(evidence.len(), 1, "{settling:#}");
    assert_eq!(evidence[0]["check_kind"], "test", "{settling:#}");
    assert!(
        observations(&settling)
            .iter()
            .any(|observation| observation["effect"] == "observe"
                && observation["outcome"] == "succeeded"),
        "its real outcome: {settling:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
}

/// A Codex test that ends within its turn (inside the exec tool's initial
/// yield) is recorded at that turn's checkpoint exactly once, and nothing is
/// carried.
#[test]
fn a_codex_test_ending_within_its_turn_is_recorded_once() {
    let (turn, worktree) = two_turns("reported-end-within-turn");
    start_test(&turn, &worktree, ITEM);
    turn.recorder()
        .command_completed_with_exit(
            ITEM,
            SIZE_TEST,
            PASSING,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the end should record");
    turn.wait_for_snapshots();
    let checkpoint = turn.finish();

    assert_eq!(
        checkpoint["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{checkpoint:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    // The same end reported again after the close is no second record.
    report_late_end(&turn, THREAD, ITEM, 0);
    let next = next_turn_checkpoint(&turn);
    assert!(next.get("verification_evidence").is_none(), "{next:#}");
}

/// A late end is taken only for the carried test whose Codex item it names,
/// on the session's own thread: one for another item, or on another thread,
/// is dropped as before and the carried test still waits for its own end.
#[test]
fn a_late_end_of_another_item_or_thread_is_not_taken() {
    let (turn, worktree) = two_turns("reported-end-exact-key");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);

    let still_waiting = |why: &str| {
        turn.record(|record| {
            assert_eq!(record.engram.carried_checks.len(), 1, "{why}");
            assert!(record.engram.carried_checks[0].check.end.is_none(), "{why}");
        })
    };
    report_late_end(&turn, THREAD, "exec-another-item", 0);
    still_waiting("no other end is taken as its own");
    // A thread the router also leads to this session, which is not the
    // session's own thread: its end for the same item is not this test's.
    const OTHER_THREAD: &str = "thread-of-another-session";
    shared_runtime(&turn)
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(OTHER_THREAD.to_owned(), turn.session_id.clone());
    report_late_end(&turn, OTHER_THREAD, ITEM, 0);
    still_waiting("an end on another thread is not taken");
    let next = next_turn_checkpoint(&turn);
    assert!(next.get("verification_evidence").is_none(), "{next:#}");
    assert_eq!(
        turn.record(|record| record.engram.carried_checks.len()),
        1,
        "still carried, waiting for its own end"
    );
}

/// A command the holder runs in the worktree after the close and before the
/// late end may have changed what the test read: the carried test is refused
/// with that cause, never credited.
#[test]
fn a_command_in_the_worktree_before_the_late_end_refuses_the_carried_test() {
    let (turn, worktree) = two_turns("reported-end-fenced");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);

    dispatch_next_turn(&turn);
    turn.recorder()
        .command_started_in(
            "exec-writer",
            "git checkout -- README.md",
            Some("git checkout -- README.md"),
            Some(&cwd(&worktree)),
        )
        .expect("the writer starts");
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);
    let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    let settling = turn
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .last()
        .expect("the next turn is checkpointed")
        .request;

    assert!(
        settling.get("verification_evidence").is_none(),
        "{settling:#}"
    );
    let line = holder_line(&turn);
    assert!(
        line.contains("the Codex test launched at")
            && line.contains("earned no credit")
            && line.contains("this session ran a `git` command there"),
        "{line}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
}

/// A host restart while a Codex test is carried closes it with outcome
/// Unknown, as a check whose end was never reported is: its end can no
/// longer reach the host.
#[test]
fn a_restart_before_the_late_end_closes_the_carried_test_unknown() {
    let (turn, worktree) = two_turns("reported-end-restart");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let persisted = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record))
            .expect("the record serializes")
    });
    let loaded: PersistedSessionRecord =
        serde_json::from_value(persisted).expect("the record deserializes");
    let record = loaded.into_record().expect("the record loads");

    assert!(record.engram.carried_checks.is_empty());
    let line = record
        .engram
        .pending_source_root_line
        .expect("the holder should be told");
    assert!(
        line.contains("was not recorded: no end was reported for it before the host restarted")
            && line.contains("closed with outcome Unknown")
            && !line.contains("background full gate"),
        "{line}"
    );
}

/// The Codex runtime a test was carried under exits before its end is
/// reported: no end can come any more, so the carry is closed with outcome
/// Unknown and earns no credit, and an end reported later for that runtime's
/// item is taken by nothing.
#[test]
fn a_runtime_exit_before_the_late_end_closes_the_carried_test_unknown() {
    let (turn, worktree) = two_turns("reported-end-runtime-exit");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let runtime = shared_runtime(&turn);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));

    turn.state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, Some("the runtime exited"))
        .expect("the exit is handled");
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "the carry is closed"
    );
    let line = holder_line(&turn);
    assert!(
        line.contains("was not recorded: no end was reported for it before its runtime exited")
            && line.contains("closed with outcome Unknown")
            && line.contains(&runtime_gone_caveat("exited")),
        "{line}"
    );

    report_late_end_on(&turn, &runtime, THREAD, ITEM, SIZE_TEST, 0);
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "the dead runtime's end is taken by nothing"
    );
    assert!(
        turn.transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .all(|request| request.request.get("verification_evidence").is_none()),
        "no credit"
    );
}

/// A full gate Codex runs in the foreground, still running when its turn
/// closes, is carried until its end is reported; a file change the watcher
/// saw in its worktree while it ran in its turn fences it once carried, as it
/// fences a launched gate. A fenced carry can never earn credit, so the same
/// close refuses it at once and tells its holder; its late passing end is
/// then taken by nothing and earns no credit.
#[test]
fn a_file_change_while_a_carried_full_gate_ran_in_its_turn_refuses_it() {
    let (turn, worktree) = two_turns("reported-end-watched-gate");
    turn.recorder()
        .command_started_in(ITEM, GATE, Some(GATE), Some(&cwd(&worktree)))
        .expect("the gate starts");
    turn.wait_for_snapshots();
    turn.state
        .note_engram_workspace_file_changes(&[WorkspaceFileChangeEvent {
            path: fs::canonicalize(&worktree)
                .unwrap()
                .join("README.md")
                .to_string_lossy()
                .into_owned(),
            kind: WorkspaceFileChangeKind::Modified,
            root_path: None,
            session_id: None,
            mtime_ms: None,
            size_bytes: None,
        }]);
    let first = turn.finish();
    assert!(first.get("verification_evidence").is_none(), "{first:#}");
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "the fenced carry is refused at the close"
    );
    let refused = holder_line(&turn);
    assert!(
        refused.contains("a Codex test still running when its turn closed")
            && refused.contains("earned no credit")
            && refused.contains("a file in its worktree changed")
            && refused.contains("(while it was running)"),
        "{refused}"
    );
    // Refusing its evidence did not end its process: it still runs, and its
    // holder is told so.
    assert!(refused.contains(STILL_RUNNING_AFTER_REFUSAL), "{refused}");
    assert!(
        turn.record(|record| record
            .engram
            .running_codex_tests
            .iter()
            .any(|running| running.key == ITEM)),
        "still running after its evidence was refused"
    );

    let runtime = shared_runtime(&turn);
    report_late_end_on(&turn, &runtime, THREAD, ITEM, GATE, 0);
    assert!(
        turn.record(|record| record.engram.running_codex_tests.is_empty()),
        "its reported end ends it"
    );
    wait_for_carried_snapshots(&turn);
    let settling = next_turn_checkpoint(&turn);
    assert!(
        settling.get("verification_evidence").is_none(),
        "{settling:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
}

/// A carried Codex test whose late end was reported stays fenced until that
/// end's closing snapshot is taken (and a declared check's artifact read), as
/// a check in its turn stays open to writes: a command the holder runs in the
/// worktree while the snapshot is still being taken may be in what it records,
/// so the test is refused, never credited.
#[test]
fn a_write_while_the_late_ends_snapshot_is_taken_refuses_the_carried_test() {
    let (turn, worktree) = two_turns("reported-end-capture-window");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);
    // The closing snapshot, still being taken.
    let pending = Arc::new(EngramBasisCapture::default());
    turn.record_mut(|record| {
        let end = record.engram.carried_checks[0]
            .check
            .end
            .as_mut()
            .expect("the late end is recorded");
        end.end_basis = pending.clone();
    });

    dispatch_next_turn(&turn);
    turn.recorder()
        .command_started_in(
            "exec-writer",
            "git checkout -- README.md",
            Some("git checkout -- README.md"),
            Some(&cwd(&worktree)),
        )
        .expect("the writer starts");
    turn.record(|record| {
        let fence = record.engram.carried_checks[0].fence.clone();
        assert!(
            fence
                .as_deref()
                .is_some_and(|fence| fence.contains("this session ran a `git` command there")),
            "{fence:?}"
        );
    });
    pending.finish(None);
    let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    let settling = turn
        .transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .last()
        .expect("the next turn is checkpointed")
        .request;
    assert!(
        settling.get("verification_evidence").is_none(),
        "{settling:#}"
    );
}

/// A carried Codex test whose late end was already reported keeps it when its
/// runtime then exits: an end that came can still be settled, so the exit
/// closes nothing, and the next checkpoint on the claim records the test.
#[test]
fn a_runtime_exit_after_the_late_end_keeps_the_reported_result() {
    let (turn, worktree) = two_turns("reported-end-exit-after-end");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));

    turn.state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, None)
        .expect("the exit is handled");
    turn.record(|record| {
        assert_eq!(
            record.engram.carried_checks.len(),
            1,
            "the ended test is kept"
        );
        assert!(record.engram.carried_checks[0].check.end.is_some());
    });
    assert!(
        !holder_line(&turn).contains("before its runtime exited"),
        "{}",
        holder_line(&turn)
    );
    let settling = next_turn_checkpoint(&turn);

    assert_eq!(
        settling["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "the reported result is recorded: {settling:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
}

/// Stop while a Codex test is still running: Stop's own close carries the
/// test, and clearing the runtime then closes it with outcome Unknown at once,
/// since the shared Codex router drops every event of a runtime that is no
/// longer the session's, so its end can never reach the host. Its carried
/// slot is freed and nothing is credited.
#[test]
fn a_stop_closes_a_carried_test_whose_end_cannot_arrive() {
    let (turn, worktree) = two_turns("reported-end-stop");
    start_test(&turn, &worktree, ITEM);

    let _ = turn.state.stop_session(&turn.session_id);
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "the carry is closed and its slot freed"
    );
    let line = holder_line(&turn);
    assert!(
        line.contains("no end can reach the host since its runtime was stopped or replaced")
            && line.contains("closed with outcome Unknown")
            && line.contains(&runtime_gone_caveat("cleared")),
        "{line}"
    );
    assert!(
        turn.transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .all(|request| request.request.get("verification_evidence").is_none()),
        "no credit"
    );
}

/// A test carried under a runtime the session no longer runs on, its runtime
/// replaced without being cleared, is closed with outcome Unknown at the next
/// checkpoint: its end can never reach the host. A test carried under the
/// session's current runtime keeps waiting for its own end.
#[test]
fn a_test_carried_under_a_replaced_runtime_is_closed_at_the_next_checkpoint() {
    let (turn, worktree) = two_turns("reported-end-replaced-runtime");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    // The same test as carried by an earlier runtime of the session: the
    // fixture keeps one runtime, so the replaced one is named by its token.
    turn.record_mut(|record| {
        let mut earlier = record.engram.carried_checks[0].clone();
        earlier.check.key = "exec-earlier-item".to_owned();
        earlier.check.sequence += 100;
        earlier.runtime = Some(RuntimeToken::Codex("replaced-runtime".to_owned()));
        record.engram.carried_checks.insert(0, earlier);
    });

    let next = next_turn_checkpoint(&turn);
    assert!(next.get("verification_evidence").is_none(), "{next:#}");
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(record.engram.carried_checks[0].check.key, ITEM);
        assert!(record.engram.carried_checks[0].check.end.is_none());
    });
    let line = holder_line(&turn);
    assert_eq!(
        line.matches("since its runtime was stopped or replaced")
            .count(),
        1,
        "{line}"
    );
    assert!(
        line.contains(&runtime_gone_caveat("replaced")),
        "{line}"
    );
}

/// Codex can report a carried test's end, while the session's next turn
/// runs, in a frame with no turn id or tagged with that running turn, which
/// the shared path routes to the running turn: the carried test still takes
/// it as its end, by its exact item id, and is recorded at the next
/// checkpoint on the claim.
#[test]
fn a_late_end_routed_to_the_running_turn_settles_the_carried_test() {
    for (label, tagged) in [
        ("reported-end-turnless-in-next-turn", None),
        ("reported-end-tagged-next-turn", Some("turn-next")),
    ] {
        let (turn, worktree) = two_turns(label);
        start_test(&turn, &worktree, ITEM);
        assert_carried_at_close(&turn);
        dispatch_next_turn(&turn);
        let runtime = shared_runtime(&turn);
        {
            let mut sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let shared = sessions
                .get_mut(&turn.session_id)
                .expect("the session is known to the shared runtime");
            shared.turn_id = Some("turn-next".to_owned());
            shared.turn_started = true;
            shared.completed_turn_id = None;
        }
        let mut params = json!({
            "threadId": THREAD,
            "item": {
                "type": "commandExecution",
                "id": ITEM,
                "command": SIZE_TEST,
                "status": "completed",
                "exitCode": 0,
                "aggregatedOutput": PASSING,
            },
        });
        if let Some(tagged) = tagged {
            params["turnId"] = json!(tagged);
        }
        handle_shared_codex_app_server_message(
            &json!({ "method": "item/completed", "params": params }),
            &turn.state,
            &runtime.runtime_id,
            &Arc::new(Mutex::new(HashMap::new())),
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
        )
        .expect("the end is handled");
        turn.record(|record| {
            assert!(
                record.engram.carried_checks[0].check.end.is_some(),
                "{label}: the end is taken by the carried test"
            )
        });
        wait_for_carried_snapshots(&turn);
        let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
        turn.state
            .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
            .expect("the next turn should complete");
        let settling = turn
            .transport
            .requests()
            .into_iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .last()
            .expect("the next turn is checkpointed")
            .request;
        assert_eq!(
            settling["verification_evidence"].as_array().map(Vec::len),
            Some(1),
            "{label}: the carried test is recorded: {settling:#}"
        );
        assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    }
}

/// Runs, in the session's next turn, a second size test in `worktree` under the
/// Codex item `SECOND_ITEM` that passes within that turn, and returns the
/// turn's checkpoint.
fn second_test_checkpoint(turn: &CheckedTurn, worktree: &FsPath) -> Value {
    passing_test_checkpoint(turn, worktree, SECOND_ITEM)
}

/// Runs, in the session's next turn, a size test in `worktree` under the
/// Codex item `key` that passes within that turn, and returns the turn's
/// checkpoint.
fn passing_test_checkpoint(turn: &CheckedTurn, worktree: &FsPath, key: &str) -> Value {
    dispatch_next_turn(turn);
    start_test(turn, worktree, key);
    turn.recorder()
        .command_completed_with_exit(
            key,
            SIZE_TEST,
            PASSING,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("the second test's end should record");
    turn.wait_for_snapshots();
    let runtime_token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session_id, &runtime_token)
        .expect("the next turn should complete");
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .last()
        .expect("the next turn is checkpointed")
        .request
}

/// A check that starts in the worktree while a carried Codex test of the
/// session still runs there runs beside it, as beside a command still running
/// in its turn: it is overlapped, with the carried test named as still
/// running, and earns no credit, even when it ends before the carried test.
#[test]
fn a_check_started_while_a_carried_test_still_runs_is_overlapped_by_it() {
    let (turn, worktree) = two_turns("reported-end-overlaps-next-check");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);

    let checkpoint = second_test_checkpoint(&turn, &worktree);

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "the second test is not credited: {checkpoint:#}"
    );
    let line = holder_line(&turn);
    let carried = &sha256_hex(ITEM.as_bytes())[..12];
    assert!(
        line.contains(&format!(
            "reached its worktree while it ran (its own command (key {carried}) was still \
             running)"
        )),
        "{line}"
    );
}

/// A carried Codex test whose end was reported runs no more: a check that
/// starts in its worktree after that is not overlapped by it, and both are
/// recorded at the next checkpoint on the claim.
#[test]
fn a_check_started_after_the_carried_tests_end_is_not_overlapped_by_it() {
    let (turn, worktree) = two_turns("reported-end-ended-before-next-check");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);

    let checkpoint = second_test_checkpoint(&turn, &worktree);

    assert_eq!(
        checkpoint["verification_evidence"].as_array().map(Vec::len),
        Some(2),
        "the carried test and the second test are both recorded: {checkpoint:#}"
    );
    let line = holder_line(&turn);
    assert!(
        !line.contains("was still running") && !line.contains("earned no credit"),
        "{line}"
    );
}

/// A claimed Codex turn measured in its named worktree, with Engram answering
/// one later turn under each grant of `later`.
fn later_turns(label: &str, later: &[&str]) -> (CheckedTurn, PathBuf) {
    let mut replies = vec![checkpoint_reply(CHECK_GRANT)];
    for grant in later {
        replies.extend([grant_reply(grant), begin_reply(grant), checkpoint_reply(grant)]);
    }
    let turn =
        CheckedTurn::start_with_opening(label, true, None, replies, 1 + later.len(), true);
    let worktree = turn.root.join(".worktrees").join("wt");
    (turn, worktree)
}

/// Refusing a carried Codex test's evidence does not end its process: a
/// check B started in its worktree fences it and its evidence is refused at
/// that turn's checkpoint, yet a check C a turn later is still overlapped
/// with it named as still running. Once its end is reported, a check D
/// started after it is not overlapped by it and is credited.
#[test]
fn a_refused_carried_test_still_overlaps_later_checks_until_its_end() {
    let (turn, worktree) = later_turns(
        "reported-end-refused-still-running",
        &[
            "turn-check-grant-later-b",
            "turn-check-grant-later-c",
            "turn-check-grant-later-d",
        ],
    );
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let running = &sha256_hex(ITEM.as_bytes())[..12];
    let overlapped_by_it = format!(
        "reached its worktree while it ran (its own command (key {running}) was still running)"
    );

    let checkpoint = passing_test_checkpoint(&turn, &worktree, "exec-b");
    assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}");
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "B's command fenced it, so its evidence is refused"
    );
    let refused = holder_line(&turn);
    assert!(
        refused.contains("earned no credit")
            && refused.contains(STILL_RUNNING_AFTER_REFUSAL),
        "the refusal says it still runs: {refused}"
    );

    let checkpoint = passing_test_checkpoint(&turn, &worktree, "exec-c");
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "C runs beside it and earns no credit: {checkpoint:#}"
    );
    let line = holder_line(&turn);
    assert!(line.contains(&overlapped_by_it), "{line}");

    report_late_end(&turn, THREAD, ITEM, 0);
    assert!(
        turn.record(|record| record.engram.running_codex_tests.is_empty()),
        "its reported end ends it, carried or not"
    );
    let checkpoint = passing_test_checkpoint(&turn, &worktree, "exec-d");
    assert_eq!(
        checkpoint["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "D, started after its end, is credited: {checkpoint:#}"
    );
}

/// What a line closing a carried Codex test still waiting for its end says
/// when its runtime was `gone` (cleared, replaced or exited): the host
/// stopped counting it as running, which does not prove its command ended.
fn runtime_gone_caveat(gone: &str) -> String {
    format!(
        "its runtime was {gone}: this does not prove the command ended; a later check in \
         this worktree may read what it still writes"
    )
}

/// What a refused Codex test's line says while its end has not come.
const STILL_RUNNING_AFTER_REFUSAL: &str = "It is still running, and later checks in its \
     worktree are overlapped until it ends; ending its process, or Stopping the session \
     (whose runtime exit closes it), ends the wait.";

/// Six hours after its launch a carried Codex test's evidence expires, but
/// its process may still run: a check started in its worktree after that is
/// still overlapped with it named.
#[test]
fn an_expired_carried_test_still_overlaps_later_checks() {
    let (turn, worktree) = later_turns(
        "reported-end-expired-still-running",
        &["turn-check-grant-expiry", "turn-check-grant-after-expiry"],
    );
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    turn.record_mut(|record| {
        record.engram.carried_checks[0].check.started_at =
            (chrono::Utc::now() - chrono::Duration::hours(7)).to_rfc3339();
    });

    let expiring = next_turn_checkpoint(&turn);
    assert!(expiring.get("verification_evidence").is_none(), "{expiring:#}");
    assert!(
        turn.record(|record| record.engram.carried_checks.is_empty()),
        "its evidence expired"
    );
    let expired = holder_line(&turn);
    assert!(
        expired.contains("within six hours of its launch")
            && expired.contains(STILL_RUNNING_AFTER_REFUSAL),
        "{expired}"
    );

    let checkpoint = passing_test_checkpoint(&turn, &worktree, "exec-after-expiry");
    assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}");
    let running = &sha256_hex(ITEM.as_bytes())[..12];
    let line = holder_line(&turn);
    assert!(
        line.contains(&format!("its own command (key {running}) was still running")),
        "{line}"
    );
}

/// A carried Codex test whose end never comes, its runtime alive, is never
/// closed by a count of turns: Codex reports no other fact that its process
/// is gone, and it may still write what later checks read. It stays carried
/// across later turns, its holder is told it is still running, what that
/// holds up and how to end the wait, and a check started in its worktree
/// turns later is overlapped with it named as still running.
#[test]
fn a_carried_test_whose_end_never_comes_stays_carried_across_later_turns() {
    const LATER: [&str; 3] = [
        "turn-check-grant-later-1",
        "turn-check-grant-later-2",
        "turn-check-grant-later-3",
    ];
    let mut replies = vec![checkpoint_reply(CHECK_GRANT)];
    for grant in LATER {
        replies.extend([grant_reply(grant), begin_reply(grant), checkpoint_reply(grant)]);
    }
    let turn = CheckedTurn::start_with_opening(
        "reported-end-never-comes",
        true,
        None,
        replies,
        1 + LATER.len(),
        true,
    );
    let worktree = turn.root.join(".worktrees").join("wt");
    start_test(&turn, &worktree, ITEM);
    let carried = holder_line_after_carry(&turn);
    assert!(
        carried.contains("Until then it is still running")
            && carried.contains("later checks in its worktree are overlapped until it ends")
            && carried.contains(
                "ending its process, or Stopping the session (whose runtime exit closes it), \
                 ends the wait"
            ),
        "{carried}"
    );

    let still_carried = |why: &str| {
        turn.record(|record| {
            assert_eq!(record.engram.carried_checks.len(), 1, "{why}");
            assert_eq!(record.engram.carried_checks[0].check.key, ITEM, "{why}");
            assert!(record.engram.carried_checks[0].check.end.is_none(), "{why}");
        })
    };
    for later in 1..=2 {
        let checkpoint = next_turn_checkpoint(&turn);
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "turn {later}: {checkpoint:#}"
        );
        still_carried(&format!("still carried after later turn {later}"));
    }

    let checkpoint = second_test_checkpoint(&turn, &worktree);
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "a check beside it earns no credit: {checkpoint:#}"
    );
    let line = holder_line(&turn);
    let named = &sha256_hex(ITEM.as_bytes())[..12];
    assert!(
        line.contains(&format!("its own command (key {named}) was still running")),
        "{line}"
    );
}

/// A runtime of the session replaced without being cleared: its tests run no
/// more for the session, and the clear of a stranded runtime drops exactly
/// those, keeping the current runtime's still-running test.
#[test]
fn a_replaced_runtime_clears_only_its_own_running_tests() {
    let (turn, worktree) = two_turns("reported-end-replaced-running");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let root_key = turn.record(|record| record.engram.running_codex_tests[0].root_key.clone());
    turn.record_mut(|record| {
        record
            .engram
            .running_codex_tests
            .insert(0, EngramRunningCodexTest {
                key: "exec-earlier-runtime".to_owned(),
                runtime: RuntimeToken::Codex("replaced-runtime".to_owned()),
                root_key: root_key.clone(),
            });
    });
    let current = turn.record(|record| record.runtime.runtime_token());
    turn.record(|record| {
        assert_eq!(
            engram_carried_running_key(&record.engram, current.as_ref(), &root_key, "exec-new"),
            Some(ITEM.to_owned()),
            "only the current runtime's test overlaps a new check"
        );
    });

    turn.record_mut(|record| {
        engram_take_stranded_reported_ends(record);
    });
    turn.record(|record| {
        let keys = record
            .engram
            .running_codex_tests
            .iter()
            .map(|running| running.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(keys, [ITEM], "the replaced runtime's test is gone, the current one stays");
        assert_eq!(record.engram.carried_checks.len(), 1, "its carry stays too");
    });
}

/// The Codex runtime a still-running test ran under exits: its process went
/// with it, so it no longer counts as running, carried or not.
#[test]
fn a_runtime_exit_ends_a_still_running_tests_overlap() {
    let (turn, worktree) = two_turns("reported-end-exit-ends-running");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    turn.record(|record| {
        assert_eq!(record.engram.running_codex_tests.len(), 1);
        assert_eq!(record.engram.running_codex_tests[0].key, ITEM);
    });
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));

    turn.state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, Some("the runtime exited"))
        .expect("the exit is handled");
    assert!(
        turn.record(|record| record.engram.running_codex_tests.is_empty()),
        "its runtime's exit ends it"
    );
}

/// The first turn's close carries the test it left running, as
/// `assert_carried_at_close` checks, and returns the holder's line.
fn holder_line_after_carry(turn: &CheckedTurn) -> String {
    assert_carried_at_close(turn);
    holder_line(turn)
}

/// What is not carried at a turn's close is closed with outcome Unknown as
/// before: a Codex command keyed by its own line, which names no one run, and
/// the test of a Claude or an ACP session, whose runtimes report no end after
/// their turn.
#[test]
fn only_a_codex_test_keyed_by_its_item_is_carried() {
    let cases: [(&str, Agent, &str); 3] = [
        ("reported-end-line-key", Agent::Codex, SIZE_TEST),
        ("reported-end-claude", Agent::Claude, ITEM),
        ("reported-end-acp", Agent::Cursor, ITEM),
    ];
    for (label, agent, key) in cases {
        let (turn, worktree) = two_turns(label);
        start_test(&turn, &worktree, key);
        assert_eq!(
            turn.record(|record| record.engram.active_turn_checks.len()),
            1,
            "{label}: the check started"
        );
        // The check started as any runtime's would; whose session it is
        // decides only what its turn's close does with it.
        turn.record_mut(|record| record.session.agent = agent);
        let checkpoint = turn.finish();
        assert!(checkpoint.get("verification_evidence").is_none(), "{label}");
        assert!(
            turn.record(|record| record.engram.carried_checks.is_empty()),
            "{label}: not carried"
        );
        let line = holder_line(&turn);
        assert!(
            line.contains("no end was reported for it by the turn's close")
                && line.contains("closed with outcome Unknown"),
            "{label}: {line}"
        );
    }
}
