// A Codex test still running when its turn closed: carried until Codex reports
// its end, which comes when its process exits, tagged with the turn that
// started it (src/engram_reported_ends.rs).
//
// Owns: the end-to-end tests of that carry. The late end arrives through the
// shared Codex app-server path (`handle_shared_codex_app_server_message`) as
// the dropped `item/completed` of a finished turn, or as one the next turn's
// routing would take. Its result stays with the original local owner and is
// never injected into a successor trace or granted native credit. A command
// that ends within its turn is observed once; only the exact item key is taken; a command of
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

mod claim_deferral {
    use super::*;
    include!("engram_reported_claim_deferral.rs");
}

#[path = "engram_reported_declared_waits.rs"]
mod declared_waits;

/// The Codex item id the test's command runs under.
const ITEM: &str = "exec-late-item";
/// The Codex item id a second test, in the next turn, runs under.
const SECOND_ITEM: &str = "exec-second-item";
/// The thread the shared runtime knows the session by.
const THREAD: &str = "thread-late-end";
/// What a passing size test prints.
const PASSING: &str = "running 3 tests\ntest result: ok. 3 passed";

/// An ordinary Codex turn associated with claimed work and a named worktree.
fn two_turns(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::start_named(label);
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
fn start_test(turn: &EOCheckedTurn, worktree: &FsPath, key: &str) {
    turn.recorder()
        .command_started_in(key, SIZE_TEST, Some(SIZE_TEST), Some(&cwd(worktree)))
        .expect("the start should record");
    turn.wait_for_snapshots();
}

/// The shared Codex runtime the session runs on, with the session known by
/// `THREAD`.
fn shared_runtime(turn: &EOCheckedTurn) -> SharedCodexRuntime {
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
fn report_late_end(turn: &EOCheckedTurn, thread: &str, key: &str, exit: i64) {
    let runtime = shared_runtime(turn);
    report_late_end_on(turn, &runtime, thread, key, SIZE_TEST, exit);
}

/// `report_late_end` through `runtime`, which may no longer be the session's,
/// for the item running `command`.
fn report_late_end_on(
    turn: &EOCheckedTurn,
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
fn wait_for_carried_snapshots(turn: &EOCheckedTurn) {
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

fn dispatch_next_turn(turn: &EOCheckedTurn) {
    turn.begin("Continue ordinary work.");
}

/// Runs the next ordinary turn and returns its actual observation envelope.
fn next_turn_observation(turn: &EOCheckedTurn) -> Value {
    dispatch_next_turn(turn);
    turn.finish()
}

fn observed_checks(request: &Value) -> &[Value] {
    assert_eq!(request["operation"], "execution_observe", "{request:#}");
    assert_eq!(request["occurrence"]["kind"], "unadmitted_turn", "{request:#}");
    assert!(request.get("verification_evidence").is_none(), "{request:#}");
    request["occurrence"]["observed_checks"].as_array().unwrap()
}

fn assert_no_automatic_credit(turn: &EOCheckedTurn) {
    for request in turn.transport.requests() {
        let operation = request.request["operation"].as_str().unwrap_or_default();
        assert!(!matches!(operation, "session_bind" | "session_status" | "turn_grant_read"
            | "turn_evaluate" | "turn_begin" | "turn_checkpoint"
            | "verification_record" | "verification_bind"), "{}", request.request);
    }
}

/// The holder line pending now.
fn holder_line(turn: &EOCheckedTurn) -> String {
    turn.record(|record| record.engram.pending_source_root_line.clone())
        .unwrap_or_default()
}

/// The overlap facts the host recorded on the latest check of `turn` started
/// under the runtime key `key`: whether it was overlapped, and by what.
fn overlap_of(turn: &EOCheckedTurn, key: &str) -> (bool, Option<String>) {
    turn.record(|record| {
        let check = record
            .engram
            .active_turn_checks
            .iter()
            .rev()
            .find(|check| check.key == key)
            .unwrap_or_else(|| panic!("the check under {key} is kept after its turn"));
        (check.overlapped, check.overlap_cause.clone())
    })
}

/// The overlap cause a check records when this session's command under the
/// runtime key `running` was still running in its worktree as it started.
fn still_running_cause(running: &str) -> String {
    format!(
        "another command of this session (key {}) was still running",
        &sha256_hex(running.as_bytes())[..12]
    )
}

/// Asserts that the latest check under `key` was overlapped by this
/// session's still-running command under `running`.
fn assert_overlapped_by(turn: &EOCheckedTurn, key: &str, running: &str) {
    let (overlapped, cause) = overlap_of(turn, key);
    assert!(
        overlapped && cause.as_deref() == Some(still_running_cause(running).as_str()),
        "{key} is overlapped by {running}: {overlapped} {cause:?}"
    );
}

/// The first turn's close carries the test it left running, records nothing
/// for it, and tells the holder.
fn assert_carried_at_close(turn: &EOCheckedTurn) {
    let first = turn.finish();
    let checks = observed_checks(&first);
    assert_eq!(checks.len(), 1, "{first:#}");
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("finished_at").is_none());
    assert_no_automatic_credit(turn);
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

#[test]
fn a_legacy_carried_terminal_requires_its_exact_item_thread_and_terminal_state() {
    let (turn, worktree) = two_turns("reported-end-legacy-identity");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let terminal = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": THREAD,
            "msg": { "item": {
                "type": "CommandExecution", "id": ITEM, "command": SIZE_TEST,
                "status": "completed", "exitCode": 0, "aggregatedOutput": PASSING
            }}
        }
    });
    for (pointer, replacement) in [
        ("/params/conversationId", json!("another-thread")),
        ("/params/conversationId", Value::Null),
        ("/params/msg/item/id", json!("another-item")),
        ("/params/msg/item/id", Value::Null),
        ("/params/msg/item/command", Value::Null),
        ("/params/msg/item/type", json!("AgentMessage")),
        ("/params/msg/item/status", json!("inProgress")),
    ] {
        let mut rejected = terminal.clone();
        *rejected.pointer_mut(pointer).unwrap() = replacement;
        assert!(!take_shared_codex_late_command_end(
            &turn.state,
            &turn.session_id,
            Some(THREAD),
            &rejected,
            rejected.pointer("/params/msg/item").unwrap(),
        ), "{rejected:#}");
        turn.record(|record| {
            assert!(record.engram.carried_checks[0].check.end.is_none());
            assert!(!record.engram.running_codex_tests.is_empty());
        });
    }
}

/// A Codex test that outlives its turn (its exec session polled after the
/// tool's initial yield) ends after that turn closed; Codex reports the end
/// tagged with the closed turn, which the turn's events drop. The test is
/// carried at the close and takes that end under its original owner.
#[test]
fn a_legacy_terminal_across_turns_closes_the_original_check_before_a_later_edit() {
    let (turn, worktree) = two_turns("reported-end-legacy-next-turn-edit");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    dispatch_next_turn(&turn);
    let runtime = shared_runtime(&turn);
    {
        let mut sessions = runtime.sessions.lock().unwrap();
        let shared = sessions.get_mut(&turn.session_id).unwrap();
        shared.turn_id = Some("turn-next".to_owned());
        shared.turn_started = true;
        shared.completed_turn_id = None;
    }
    handle_shared_codex_app_server_message(
        &json!({
            "method": "codex/event/item_completed",
            "params": {
                "conversationId": THREAD,
                "msg": {
                    "turn_id": "turn-gone",
                    "item": {
                        "type": "CommandExecution",
                        "id": ITEM,
                        "command": SIZE_TEST,
                        "status": "completed",
                        "exitCode": 0,
                        "aggregatedOutput": PASSING
                    }
                }
            }
        }),
        &turn.state,
        &runtime.runtime_id,
        &Arc::new(Mutex::new(HashMap::new())),
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("the runtime terminal is handled");
    turn.record(|record| {
        assert!(
            record.engram.carried_checks[0].check.end.is_some(),
            "the exact legacy terminal must reach the original carried check"
        );
        assert!(record.engram.running_codex_tests.is_empty());
    });
    wait_for_carried_snapshots(&turn);
    turn.record(|record| {
        assert!(!record.engram.carried_checks[0].check.open_to_writes());
        assert!(record.engram.carried_checks[0].fence.is_none());
    });

    let edited = fs::canonicalize(&worktree).unwrap().join("README.md");
    fs::write(&edited, "a source change after the terminal and closing capture\n").unwrap();
    // Report the new turn's own edit through its recorder. An untimed
    // watcher notification could describe an earlier write delivered late
    // and deliberately remains conservative even after a check has ended.
    turn.recorder().push_diff(
        &edited.to_string_lossy(),
        "Edit after the carried command's closing capture",
        "+a source change after the terminal and closing capture",
        ChangeType::Edit,
    ).unwrap();
    turn.record(|record| assert!(record.engram.carried_checks[0].fence.is_none()));
    let successor = turn.finish();
    assert!(observed_checks(&successor).is_empty(),
        "a late terminal is not the successor's command: {successor:#}");
    assert_no_automatic_credit(&turn);
}

#[test]
fn a_codex_test_that_outlives_its_turn_is_recorded_from_its_late_end() {
    let (turn, worktree) = two_turns("reported-end-credited");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let owner = turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        (carried.work_id.clone(), carried.claim_id.clone(), carried.claim_fence,
            carried.check.key.clone())
    });
    report_late_end(&turn, THREAD, ITEM, 0);
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(
            carried.check.end.is_some(),
            "the late end is taken"
        );
        assert_eq!((carried.work_id.clone(), carried.claim_id.clone(),
            carried.claim_fence, carried.check.key.clone()), owner,
            "the late end retains its exact original claim and command");
    });
    wait_for_carried_snapshots(&turn);
    let successor = next_turn_observation(&turn);
    assert!(observed_checks(&successor).is_empty(),
        "no late fact is reassigned to the successor: {successor:#}");
    assert_no_automatic_credit(&turn);
}

/// A Codex test that ends within its turn (inside the exec tool's initial
/// yield) is observed in that turn exactly once, and nothing is carried.
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
    let observation = turn.finish();
    let checks = observed_checks(&observation);
    assert_eq!(checks.len(), 1, "{observation:#}");
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert!(checks[0]["finished_at"].is_string());
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    // The same end reported again after the close is no second record.
    report_late_end(&turn, THREAD, ITEM, 0);
    let next = next_turn_observation(&turn);
    assert!(observed_checks(&next).is_empty(), "{next:#}");
    assert_no_automatic_credit(&turn);
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
    let next = next_turn_observation(&turn);
    assert!(observed_checks(&next).is_empty(), "{next:#}");
    assert_no_automatic_credit(&turn);
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
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.as_deref().is_some_and(|cause|
            cause.contains("this session ran a `git` command there")));
    });
    let settling = turn.finish();
    assert!(observed_checks(&settling).is_empty(), "{settling:#}");
    assert_no_automatic_credit(&turn);
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
    assert_no_automatic_credit(&turn);
}

/// A full gate Codex runs in the foreground, still running when its turn
/// closes, is carried until its end is reported; a file change the watcher
/// saw in its worktree while it ran in its turn fences it once carried, as it
/// fences a launched gate. The carry keeps that fence; nothing settles it at
/// a later turn, and its late passing end is recorded on the fenced carry and
/// earns no credit.
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
    // A long checkout path can make the bounded prompt summary cut the
    // refusal's identity. Observe the existing notice owner before that merge.
    let fingerprint = turn.record_mut(|record| {
        record.engram.test_source_root_lines = Some(Vec::new());
        let check = record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == ITEM)
            .expect("the running check owns the refusal");
        engram_check_fingerprint(&check.command)
    });
    let first = turn.finish();
    assert!(first.get("verification_evidence").is_none(), "{first:#}");
    // The close carries it with the watcher's change as its fence; no
    // settlement refuses it there.
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1, "the fenced test is carried");
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.check.key, ITEM);
        assert!(carried.check.end.is_none());
        assert_eq!(engram_check_fingerprint(&carried.check.command), fingerprint);
        let fence = carried.fence.as_deref().unwrap_or_default();
        assert!(
            fence.contains("a file in its worktree changed")
                && fence.contains("(while it was running)"),
            "the change while it ran is its fence: {fence}"
        );
    });
    let notices = turn.record_mut(|record| {
        record
            .engram
            .test_source_root_lines
            .take()
            .expect("this close owns its notice witness")
    });
    assert!(
        notices.iter().any(|line| {
            line.contains("a Codex test still running when its turn closed")
                && line.contains(&fingerprint)
        }),
        "the initial carry is announced: {notices:#?}"
    );
    assert!(
        notices.iter().all(|line| !line.contains("earned no credit")),
        "the close issues no refusal for the fenced carry: {notices:#?}"
    );
    // The fence does not end its process: it still runs.
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
    let settling = next_turn_observation(&turn);
    assert!(
        settling.get("verification_evidence").is_none(),
        "{settling:#}"
    );
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1, "no later turn settles it");
        let carried = &record.engram.carried_checks[0];
        assert!(carried.check.end.is_some(), "its reported end is recorded on it");
        assert!(carried.fence.is_some(), "its end keeps its fence");
    });
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
    let settling = turn.finish();
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
    let settling = next_turn_observation(&turn);

    assert!(observed_checks(&settling).is_empty(),
        "the original terminal is not a command of the new runtime: {settling:#}");
    assert_no_automatic_credit(&turn);
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
    assert_no_automatic_credit(&turn);
}

/// A test carried under a runtime the session no longer runs on, its runtime
/// replaced without being cleared, stays carried through the next turn and is
/// closed with outcome Unknown by the runtime clear's rule: its end can never
/// reach the host. A test carried under the session's current runtime keeps
/// waiting for its own end.
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

    let next = next_turn_observation(&turn);
    assert!(next.get("verification_evidence").is_none(), "{next:#}");
    // No turn settles a carry: both stay carried until a runtime clear.
    turn.record(|record| {
        let keys = record
            .engram
            .carried_checks
            .iter()
            .map(|carried| (carried.check.key.as_str(), carried.check.end.is_none()))
            .collect::<Vec<_>>();
        assert_eq!(keys, [("exec-earlier-item", true), (ITEM, true)]);
    });
    assert!(
        !holder_line(&turn).contains("since its runtime was stopped or replaced"),
        "{}",
        holder_line(&turn)
    );
    // The runtime clear's rule closes the replaced runtime's carry alone.
    let lines = turn.record_mut(|record| engram_take_stranded_reported_ends(record));
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert_eq!(record.engram.carried_checks[0].check.key, ITEM);
        assert!(record.engram.carried_checks[0].check.end.is_none());
    });
    let line = lines.join(" ");
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
        let settling = turn.finish();
        assert!(observed_checks(&settling).is_empty(),
            "{label}: a routed late terminal cannot become successor evidence: {settling:#}");
        assert_no_automatic_credit(&turn);
    }
}

/// Runs, in the session's next turn, a second size test in `worktree` under the
/// Codex item `SECOND_ITEM` that passes within that turn, and returns the
/// turn's ordinary observation.
fn second_test_observation(turn: &EOCheckedTurn, worktree: &FsPath) -> Value {
    passing_test_observation(turn, worktree, SECOND_ITEM)
}

/// Runs, in the session's next turn, a size test in `worktree` under the
/// Codex item `key` that passes within that turn, and returns the turn's
/// ordinary observation.
fn passing_test_observation(turn: &EOCheckedTurn, worktree: &FsPath, key: &str) -> Value {
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
    turn.finish()
}

/// A check that starts in the worktree while a carried Codex test of the
/// session still runs there runs beside it, as beside a command still running
/// in its turn: it is overlapped, with the carried test named as still
/// running, and earns no credit, even when it ends before the carried test.
/// The overlap is recorded on the check itself.
#[test]
fn a_check_started_while_a_carried_test_still_runs_is_overlapped_by_it() {
    let (turn, worktree) = two_turns("reported-end-overlaps-next-check");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);

    let checkpoint = second_test_observation(&turn, &worktree);

    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "the second test is not credited: {checkpoint:#}"
    );
    assert_overlapped_by(&turn, SECOND_ITEM, ITEM);
}

/// A carried Codex test whose end was reported runs no more: a check that
/// starts in its worktree after that is not overlapped by it. The new trace
/// reports only its own check, while the original result stays local.
#[test]
fn a_check_started_after_the_carried_tests_end_is_not_overlapped_by_it() {
    let (turn, worktree) = two_turns("reported-end-ended-before-next-check");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);

    let checkpoint = second_test_observation(&turn, &worktree);

    let checks = observed_checks(&checkpoint);
    assert_eq!(checks.len(), 1, "only the successor's check: {checkpoint:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_no_automatic_credit(&turn);
    let line = holder_line(&turn);
    assert!(
        !line.contains("was still running") && !line.contains("earned no credit"),
        "{line}"
    );
}

/// A second claimed session in the same AppState, with its own runtime and
/// control replies. Its checks run in the original session's named worktree.
fn another_checking_session(owner: &EOCheckedTurn, worktree: &FsPath) -> EOCheckedTurn {
    let project = owner.record(|record| record.session.project_id.clone().unwrap());
    let session_id = create_test_project_session(&owner.state, Agent::Codex, &project, worktree);
    {
        let mut inner = owner.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    let binding = test_control_work_binding("other-running-test", 1);
    let transport = ScriptedEngramControlTransport::new([]);
    transport.set_current_work_binding(Some(binding.clone()));
    transport.enable_named_roots(&session_id, &binding);
    // The ordinary work service answers for S2, as the main fixture's does
    // for its session, so naming can associate S2 with its held claim.
    transport.named_roots.lock().unwrap().as_mut().unwrap().work_session_read_reply =
        Some(Ok(json!({
            "project_id": "github.com/example/source-root", "session_id": session_id,
            "work_session": true, "capabilities": ["host_work_v1"],
        })));
    transport.replace_held_claims([Ok(claim_deferral::held_snapshot(&[&binding], &binding))]);
    owner.state.install_control_test_transport(transport.clone());
    let mut other = EOCheckedTurn::from_session(owner.state.clone(), session_id,
        owner.root.clone(), transport, owner._runtime_rx.clone());
    other.host = owner.host.clone();
    // Naming the shared worktree is what associates S2 with its claim.
    other.state.name_engram_source_root(&other.session_id, EngramSourceRootRequest {
        work: binding.work_id.clone(),
        path: Some(Some(worktree.to_string_lossy().into_owned())),
    }).expect("ordinary work services name S2's root without admitting a turn");
    assert_eq!(
        other.record(|record| record.engram.work_binding.clone()),
        Some(binding),
        "naming established S2's claim association"
    );
    dispatch_next_turn(&other);
    other.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(
            record.engram.unadmitted_active_turn.is_some(),
            "S2 owns its distinct ordinary trace"
        );
    });
    assert_no_automatic_credit(&other);
    other
}

/// Session S1 is Idle, but its Codex process still writes in W. Session S2's
/// check in W cannot earn credit until S1 reports that process's end. Codex
/// item keys are session-scoped, so deliberately reuse the same key in S2.
#[test]
fn another_sessions_carried_codex_test_overlaps_until_its_reported_end() {
    let (owner, worktree) = two_turns("reported-end-other-session");
    start_test(&owner, &worktree, ITEM);
    assert_carried_at_close(&owner);
    assert_eq!(
        owner.record(|record| record.session.status),
        SessionStatus::Idle
    );
    let other = another_checking_session(&owner, &worktree);

    start_test(&other, &worktree, ITEM);
    other
        .recorder()
        .command_completed_with_exit(
            ITEM,
            SIZE_TEST,
            PASSING,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .expect("S2's passing check ends");
    other.wait_for_snapshots();
    let checkpoint = other.finish();
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "S1's still-running test prevents S2's credit: {checkpoint:#}"
    );
    let (overlapped, cause) = overlap_of(&other, ITEM);
    let cause = cause.unwrap_or_default();
    assert!(
        overlapped
            && cause.contains(&owner.session_id)
            && cause.contains("another session's still-running test")
            && cause.contains(&sha256_hex(ITEM.as_bytes())[..12]),
        "the original owner and test are identified on S2's check: {cause}"
    );
    assert!(owner.record(|record| !record.engram.running_codex_tests.is_empty()));

    report_late_end(&owner, THREAD, ITEM, 0);
    assert!(owner.record(|record| record.engram.running_codex_tests.is_empty()));
    let checkpoint = passing_test_observation(&other, &worktree, ITEM);
    let checks = observed_checks(&checkpoint);
    assert_eq!(checks.len(), 1, "only S2's own check: {checkpoint:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_no_automatic_credit(&other);
}

/// The cross-session hazard is scoped to its owner's current runtime, its
/// captured worktree, and a writable owner. Refusing the carried evidence
/// alone must not erase the hazard; runtime exit does end its lifetime.
#[test]
fn another_sessions_carried_codex_test_respects_owner_scope() {
    let (owner, worktree) = two_turns("reported-end-other-scope");
    start_test(&owner, &worktree, ITEM);
    assert_carried_at_close(&owner);
    let project = owner.record(|record| record.session.project_id.clone().unwrap());
    let other = create_test_project_session(&owner.state, Agent::Codex, &project, &worktree);
    let running = owner.record(|record| record.engram.running_codex_tests[0].clone());
    let cause = || {
        let inner = owner.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).unwrap();
        engram_other_writer_cause(&inner, index, &running.root_key)
    };
    assert!(cause().is_some(), "the idle owner's live test overlaps S2");
    {
        let inner = owner.state.inner.lock().unwrap();
        let index = inner.find_session_index(&owner.session_id).unwrap();
        assert!(
            engram_other_writer_cause(&inner, index, &running.root_key).is_none(),
            "the other-writer lookup does not include its own session"
        );
    }

    owner.record_mut(|record| {
        record.engram.running_codex_tests[0].runtime =
            RuntimeToken::Codex("replaced-owner-runtime".to_owned());
    });
    assert!(
        cause().is_none(),
        "a replaced runtime's stale record is ignored"
    );
    owner.record_mut(|record| record.engram.running_codex_tests[0] = running.clone());
    let elsewhere = sibling_worktree(&owner, "other-running-root");
    owner.record_mut(|record| {
        record.engram.running_codex_tests[0].root_key = engram_worktree_root(&elsewhere);
    });
    assert!(
        cause().is_none(),
        "a different worktree does not overlap S2"
    );
    owner.record_mut(|record| {
        record.engram.running_codex_tests[0] = running.clone();
        record.engram.carried_checks.clear();
    });
    assert!(
        cause().is_some(),
        "discarded evidence does not end its process"
    );

    let (delegation, _) = super::super::install_required_review_delegation(&owner.state, &other);
    {
        let mut inner = owner.state.inner.lock().unwrap();
        let review = inner
            .delegations
            .iter_mut()
            .find(|entry| entry.id == delegation)
            .unwrap();
        review.child_session_id = owner.session_id.clone();
        let index = inner.find_session_index(&owner.session_id).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some(delegation.clone());
    }
    assert!(cause().is_none(), "a read-only owner cannot overlap S2");
    {
        let mut inner = owner.state.inner.lock().unwrap();
        inner.delegations.retain(|entry| entry.id != delegation);
        let index = inner.find_session_index(&owner.session_id).unwrap();
        inner.sessions[index].session.parent_delegation_id = None;
    }
    assert!(
        cause().is_some(),
        "the live writable owner is still counted"
    );
    owner
        .state
        .handle_runtime_exit_if_matches(&owner.session_id, &running.runtime, Some("runtime exited"))
        .expect("the owner's runtime exit is handled");
    assert!(
        cause().is_none(),
        "runtime exit ends the cross-session hazard"
    );
}

/// Fencing a carried Codex test does not end its process: a check B started
/// in its worktree fences it, and it stays carried, yet a check C a turn
/// later is still overlapped with it named as still running. Once its end is
/// reported, a check D started after it is not overlapped by it and is
/// observed as passed.
#[test]
fn a_refused_carried_test_still_overlaps_later_checks_until_its_end() {
    let (turn, worktree) = two_turns("reported-end-refused-still-running");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    let checkpoint = passing_test_observation(&turn, &worktree, "exec-b");
    assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}");
    assert_overlapped_by(&turn, "exec-b", ITEM);
    // B's command fenced the carry; no turn settles or refuses it, and its
    // process still counts as running.
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1, "still carried");
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.check.key, ITEM);
        assert!(carried.check.end.is_none());
        assert!(
            carried.fence.as_deref().is_some_and(|cause|
                cause.contains("this session ran a `cargo` command there")),
            "B's command fenced it: {:?}",
            carried.fence
        );
        assert!(record.engram.running_codex_tests.iter().any(|running| running.key == ITEM));
    });
    let refused = holder_line(&turn);
    assert!(
        !refused.contains("earned no credit")
            && !refused.contains(STILL_RUNNING_AFTER_REFUSAL),
        "no refusal is issued for the fenced carry: {refused}"
    );

    let checkpoint = passing_test_observation(&turn, &worktree, "exec-c");
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "C runs beside it and earns no credit: {checkpoint:#}"
    );
    assert_overlapped_by(&turn, "exec-c", ITEM);

    report_late_end(&turn, THREAD, ITEM, 0);
    assert!(
        turn.record(|record| record.engram.running_codex_tests.is_empty()),
        "its reported end ends it, carried or not"
    );
    let checkpoint = passing_test_observation(&turn, &worktree, "exec-d");
    let checks = observed_checks(&checkpoint);
    assert_eq!(checks.len(), 1, "only D's own check: {checkpoint:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_no_automatic_credit(&turn);
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

/// Six hours after its launch a carried Codex test is past its age bound,
/// but its process may still run: it stays carried, and a check started in
/// its worktree after that is still overlapped with it named.
#[test]
fn an_expired_carried_test_still_overlaps_later_checks() {
    let (turn, worktree) = two_turns("reported-end-expired-still-running");
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    turn.record_mut(|record| {
        record.engram.carried_checks[0].check.started_at =
            (chrono::Utc::now() - chrono::Duration::hours(7)).to_rfc3339();
    });

    let expiring = next_turn_observation(&turn);
    assert!(expiring.get("verification_evidence").is_none(), "{expiring:#}");
    // Past its age bound it is still carried: only its reported end or its
    // runtime's going removes it, and its process still counts as running.
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1, "still carried");
        let carried = &record.engram.carried_checks[0];
        assert_eq!(carried.check.key, ITEM);
        assert!(carried.check.end.is_none());
        assert!(
            engram_carried_check_expired(carried, chrono::Utc::now()),
            "its launch is past the carried-check age bound"
        );
        assert!(record.engram.running_codex_tests.iter().any(|running| running.key == ITEM));
    });
    let expired = holder_line(&turn);
    assert!(
        !expired.contains("within six hours of its launch")
            && !expired.contains(STILL_RUNNING_AFTER_REFUSAL),
        "no expiry refusal is issued: {expired}"
    );

    let checkpoint = passing_test_observation(&turn, &worktree, "exec-after-expiry");
    assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}");
    assert_overlapped_by(&turn, "exec-after-expiry", ITEM);
}

/// A carried Codex test whose end never comes, its runtime alive, is never
/// closed by a count of turns: Codex reports no other fact that its process
/// is gone, and it may still write what later checks read. It stays carried
/// across later turns, its holder is told it is still running, what that
/// holds up and how to end the wait, and a check started in its worktree
/// turns later is overlapped with it named as still running.
#[test]
fn a_carried_test_whose_end_never_comes_stays_carried_across_later_turns() {
    let (turn, worktree) = two_turns("reported-end-never-comes");
    start_test(&turn, &worktree, ITEM);
    let carried = holder_line_after_carry(&turn);
    assert!(
        carried.contains("remains owned by its original turn until its end is reported")
            && carried.contains("Later checks in its worktree remain overlapped while it runs")
            && carried.contains(
                "stopping the session closes this observation with an unknown outcome"
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
        let checkpoint = next_turn_observation(&turn);
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "turn {later}: {checkpoint:#}"
        );
        still_carried(&format!("still carried after later turn {later}"));
    }

    let checkpoint = second_test_observation(&turn, &worktree);
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "a check beside it earns no credit: {checkpoint:#}"
    );
    assert_overlapped_by(&turn, SECOND_ITEM, ITEM);
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
fn holder_line_after_carry(turn: &EOCheckedTurn) -> String {
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
            line.contains("no end was reported for it by the ordinary turn's end")
                && line.contains("closed with outcome Unknown"),
            "{label}: {line}"
        );
    }
}
