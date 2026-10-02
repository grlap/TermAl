// Owns the tests of Claude work that outlives its frame
// (src/claude_outstanding_work.rs), driven through the production frame
// application and the checkpoint path: a subagent command running across the
// prompt boundary excludes the next grant with no later frame; a checkpoint
// racing a foreign frame never installs a clean report after the frame was
// admitted; a replaced runtime's frame credits nothing to its successor, and
// a turnover on the same runtime leaves old work foreign; a background
// launch's acknowledgement retires nothing, its terminal notice does.
// Does not own the router's origin rules (src/tests/claude_frame_router.rs) or
// the turns no prompt owns (src/tests/engram_claude_runtime_turns.rs, whose
// reader and frame helpers this child module uses). New module.
use super::*;

/// A top-level Bash tool call.
fn root_bash(id: &str, command: &str, background: bool) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": id,
        "name": "Bash", "input": {"command": command, "run_in_background": background}}]}})
}

/// A top-level Bash result.
fn root_bash_result(id: &str, stdout: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result",
        "tool_use_id": id, "content": stdout}]},
        "tool_use_result": {"stdout": stdout, "stderr": "", "interrupted": false}})
}

/// A subagent's Bash call of `command`.
fn nested_command(id: &str, parent: &str, command: &str) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": parent, "message": {"content": [{
        "type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}]}})
}

/// A subagent's file write, by a parent no frame this runtime read named.
fn unknown_parent_write(id: &str) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": "toolu-before-restart",
        "message": {"content": [{"type": "tool_use", "id": id, "name": "Write",
            "input": {"file_path": "late.txt", "content": "late"}}]}})
}

fn command_cards(record: &SessionRecord, command: &str) -> Vec<CommandStatus> {
    record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command: shown,
                status,
                ..
            } if shown == command => Some(*status),
            _ => None,
        })
        .collect()
}

const PASSING: &str = "running 3 tests\ntest result: ok. 3 passed";
const LONG_COMMAND: &str = "sleep 30 && echo done";

/// Prompt A launches a background task whose subagent starts a long command,
/// and A ends while it runs. Returns the reader with B's prompt open.
fn straddling_root(
    label: &str,
) -> (
    CheckedTurn,
    ClaudeReaderOn,
    ClaudePromptCommand,
    ClaudeTurnOwnership,
) {
    let turn = two_turn_root(label);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let first = host_prompt(
        "Launch the background work.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&first));
    reader.feed(capable_init());
    reader.feed(lifecycle(&first, "started"));
    reader.feed(root_task("toolu-background"));
    reader.feed(nested_command(
        "long-under-a",
        "toolu-background",
        LONG_COMMAND,
    ));
    reader.feed(named_result(&first));
    begin_next_turn(&turn);
    let second = host_prompt(
        "The next prompt.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&second));
    reader.feed(lifecycle(&second, "started"));
    (turn, reader, second, ownership)
}

#[test]
fn a_subagent_command_running_across_the_prompt_boundary_excludes_the_next_grant_with_no_later_frame()
 {
    let (turn, mut reader, second, _ownership) = straddling_root("outstanding-straddle");
    // No frame of A's subagent has arrived since B began: B is excluded from
    // the command still running all the same.
    turn.record(|record| {
        assert_eq!(
            record.engram.active_grant_id.as_deref(),
            Some(CONTINUITY_NEXT_GRANT)
        );
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CONTINUITY_NEXT_GRANT),
            "B begins mixed"
        );
        assert!(record.claude_outstanding.holds("long-under-a"));
    });
    // A test B runs while that command may write is fenced.
    reader.feed(root_bash("b-test", SIZE_TEST, false));
    turn.wait_for_snapshots();
    turn.record(|record| {
        let checks = &record.engram.active_turn_checks;
        assert_eq!(checks.len(), 1);
        assert!(checks[0].overlapped, "B's check is fenced");
    });
    reader.feed(root_bash_result("b-test", PASSING));
    // The late result still finds its call: the same card ends, and B gets
    // nothing from it.
    reader.feed(
        json!({"type": "user", "parent_tool_use_id": "toolu-background",
        "message": {"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "long-under-a", "content": "done"}]},
        "tool_use_result": {"stdout": "done", "stderr": "", "interrupted": false}}),
    );
    turn.record(|record| {
        assert_eq!(
            command_cards(record, LONG_COMMAND),
            vec![CommandStatus::Success],
            "one card, updated in place, not left running"
        );
        assert!(!record.claude_outstanding.holds("long-under-a"));
        assert!(
            record
                .unassigned_claude_observations
                .iter()
                .any(|kept| kept.what.contains("long-under-a") && kept.what.contains("finished")),
            "its end is kept for no grant"
        );
    });
    reader.feed(named_result(&second));
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 2);
    let report = &checkpoints[1];
    assert_eq!(
        report["verification_evidence"]
            .as_array()
            .map_or(0, Vec::len),
        0,
        "B's fenced test is not B's evidence: {report:#}"
    );
    assert!(
        report["observations"]
            .as_array()
            .is_none_or(|observations| observations
                .iter()
                .all(|observation| observation["source_changed"] != true)),
        "B claims no source change: {report:#}"
    );
}

/// How far a foreign frame got when the checkpoint built its report.
#[derive(Clone, Copy, Debug)]
enum ForeignStage {
    /// No foreign frame at all: the contrast.
    None,
    /// Admitted, its handler not run yet.
    Admitted,
    /// Admitted and observed, not settled.
    Observed,
    /// Applied whole.
    Settled,
}

/// The report a checkpoint racing a foreign frame installs, with the frame
/// at `stage` while the checkpoint sits between its capture and its claim.
fn report_racing_a_foreign_frame(label: &str, stage: ForeignStage) -> Value {
    let turn = CheckedTurn::start(label, true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let prompt = host_prompt("The prompt.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    reader.feed(capable_init());
    reader.feed(lifecycle(&prompt, "started"));
    turn.change_readme("changed in the turn\n");

    let gate = install_test_engram_turn_report_gate(&turn.state, &turn.session_id);
    let checkpoint = {
        let state = turn.state.clone();
        let session_id = turn.session_id.clone();
        std::thread::spawn(move || {
            state.checkpoint_engram_turn_off_lock(
                &session_id,
                EngramCheckpointPurpose::Teardown,
                None,
                None,
                EngramNextIntent::Exit,
                Some(EngramExecutionOutcome::Succeeded),
                None,
            )
        })
    };
    gate.wait_until_claimed();
    let frame = unknown_parent_write("racing-write");
    let provenance = ClaudeObservationProvenance {
        token: token.clone(),
        origin: ClaudeWorkOrigin::Unattributed,
    };
    let work = claude_frame_work(&frame, true);
    match stage {
        ForeignStage::None => {}
        ForeignStage::Admitted => {
            turn.state
                .admit_claude_frame(&turn.session_id, &provenance, true, &work)
        }
        ForeignStage::Observed => {
            turn.state
                .admit_claude_frame(&turn.session_id, &provenance, true, &work);
            turn.state.engram_host().observe(
                &turn.session_id,
                &EngramObservationProvenance::Claude(provenance.clone()),
                EngramRecorderObservation::WorkspaceEdit,
            );
        }
        ForeignStage::Settled => reader.feed(frame.clone()),
    }
    gate.release();
    checkpoint.join().expect("the checkpoint should not panic");
    // Whatever the frame does after the report was installed changes it not.
    if matches!(stage, ForeignStage::Admitted | ForeignStage::Observed) {
        turn.state
            .settle_claude_frame(&turn.session_id, &token, &work);
    }
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), 1, "{stage:?}");
    checkpoints[0].clone()
}

#[test]
fn a_checkpoint_racing_a_foreign_frame_never_installs_a_clean_report_after_its_admission() {
    // The contrast: with no foreign frame, the turn reports its own change.
    let clean = report_racing_a_foreign_frame("race-contrast", ForeignStage::None);
    assert!(
        clean["observations"]
            .as_array()
            .is_some_and(|observations| observations
                .iter()
                .any(|observation| observation["source_changed"] == true)),
        "the contrast reports the turn's change: {clean:#}"
    );
    for (label, stage) in [
        ("race-admitted", ForeignStage::Admitted),
        ("race-observed", ForeignStage::Observed),
        ("race-settled", ForeignStage::Settled),
    ] {
        let report = report_racing_a_foreign_frame(label, stage);
        assert!(
            report["observations"]
                .as_array()
                .is_none_or(|observations| observations
                    .iter()
                    .all(|observation| observation["source_changed"] != true)),
            "{stage:?}: the grant's own change is withheld: {report:#}"
        );
    }
}

#[test]
fn the_sink_alone_excludes_the_live_grant_from_another_turns_observation() {
    // Without the frame's admission: the sink decides from the provenance
    // the observation carries, under the lock that applies it.
    for (label, foreign) in [("sink-foreign", true), ("sink-current", false)] {
        let turn = CheckedTurn::start(label, true);
        let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
        let generation = turn.record(|record| record.active_turn_generation);
        let provenance = EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token,
            origin: if foreign {
                ClaudeWorkOrigin::Attempt {
                    turn_generation: generation.wrapping_sub(1),
                }
            } else {
                ClaudeWorkOrigin::Attempt {
                    turn_generation: generation,
                }
            },
        });
        turn.state.engram_host().observe(
            &turn.session_id,
            &provenance,
            EngramRecorderObservation::WorkspaceEdit,
        );
        turn.record(|record| {
            assert_eq!(
                record.engram.active_turn_mixed_attribution.as_deref() == Some(CHECK_GRANT),
                foreign,
                "{label}"
            );
        });
    }
}

#[test]
fn a_frame_routed_before_its_runtime_was_replaced_credits_nothing_to_the_successor() {
    for (label, root_frame) in [("replaced-root", true), ("replaced-subagent", false)] {
        let turn = CheckedTurn::start(label, true);
        let generation = turn.record(|record| record.active_turn_generation);
        let ownership = new_claude_turn_ownership();
        let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
        let prompt = host_prompt("The prompt.", generation);
        lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
        reader.feed(capable_init());
        reader.feed(lifecycle(&prompt, "started"));
        reader.feed(root_task("toolu-task"));
        let frame = if root_frame {
            root_bash("racing-test", SIZE_TEST, false)
        } else {
            nested_bash("racing-test", "toolu-task")
        };
        let plan = reader
            .frames
            .router
            .route(&frame, !reader.frames.root.replay_became_unsafe);
        assert_eq!(
            plan.origin == ClaudeWorkOrigin::Unattributed,
            !root_frame,
            "{label}: the router calls the root frame the prompt's, a subagent's nobody's"
        );
        // The runtime is replaced after routing, before the frame's effects.
        let (successor, _commands) = test_claude_runtime_handle(&format!("{label}-successor"));
        turn.record_mut(|record| record.runtime = SessionRuntime::Claude(successor));
        let unassigned_before = turn.record(|record| record.unassigned_claude_observations.len());
        apply_claude_frame_plan(
            &reader.context,
            &mut reader.frames,
            &mut reader.recorder,
            &frame,
            &plan,
        );
        turn.wait_for_snapshots();
        turn.record(|record| {
            assert!(
                record.engram.active_turn_checks.is_empty(),
                "{label}: no check of the successor's grant"
            );
            assert!(
                !record
                    .engram
                    .running_command_keys
                    .contains_key("racing-test"),
                "{label}: no running key"
            );
            assert!(
                !record
                    .unassigned_claude_observations
                    .iter()
                    .skip(unassigned_before)
                    .any(|kept| kept.what.contains("racing-test")),
                "{label}: nothing of the replaced runtime is kept on the session"
            );
            assert_eq!(
                record.unmediated_claude_turn, None,
                "{label}: no turn marker moved"
            );
            assert!(
                record.claude_outstanding.holds("racing-test"),
                "{label}: what it may still run stays outstanding"
            );
            assert!(
                claude_session_work_restricts(record),
                "{label}: and it restricts the successor"
            );
        });
    }
}

#[test]
fn a_grant_turnover_after_routing_leaves_old_work_foreign_on_the_same_runtime() {
    let turn = two_turn_root("turnover-after-routing");
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let first = host_prompt(
        "Launch.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&first));
    reader.feed(capable_init());
    reader.feed(lifecycle(&first, "started"));
    // Routed while A's turn is open: the router calls it A's.
    let frame = root_bash("racing-test", SIZE_TEST, false);
    let plan = reader
        .frames
        .router
        .route(&frame, !reader.frames.root.replay_became_unsafe);
    assert!(matches!(plan.origin, ClaudeWorkOrigin::Attempt { .. }));
    // A ends and B begins on the same runtime before the frame's effects.
    reader.feed(named_result(&first));
    begin_next_turn(&turn);
    turn.record_mut(|record| record.engram.active_turn_mixed_attribution = None);
    apply_claude_frame_plan(
        &reader.context,
        &mut reader.frames,
        &mut reader.recorder,
        &frame,
        &plan,
    );
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert!(
            record.engram.active_turn_checks.is_empty(),
            "A's work starts no check of B's grant"
        );
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CONTINUITY_NEXT_GRANT),
            "B is excluded from it"
        );
        assert!(
            record
                .unassigned_claude_observations
                .iter()
                .any(|kept| kept.what.contains("racing-test") && kept.what.contains("started")),
            "it is kept for no grant"
        );
    });
}

#[test]
fn a_background_launch_acknowledgement_retires_nothing_and_its_terminal_notice_does() {
    const THIRD_GRANT: &str = "outstanding-third-grant";
    let turn = CheckedTurn::start_with_turns(
        "background-launch",
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            grant_reply(CONTINUITY_NEXT_GRANT),
            begin_reply(CONTINUITY_NEXT_GRANT),
            checkpoint_reply(CONTINUITY_NEXT_GRANT),
            grant_reply(THIRD_GRANT),
            begin_reply(THIRD_GRANT),
            checkpoint_reply(THIRD_GRANT),
        ],
        3,
    );
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let first = host_prompt(
        "Run it in the background.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&first));
    reader.feed(capable_init());
    reader.feed(lifecycle(&first, "started"));
    reader.feed(root_bash("toolu-bg", LONG_COMMAND, true));
    reader.feed(root_bash_result(
        "toolu-bg",
        "Command running in background with ID: b1.",
    ));
    turn.record(|record| {
        assert!(
            record.claude_outstanding.holds("toolu-bg"),
            "the acknowledgement marks only the launch"
        );
    });
    reader.feed(named_result(&first));

    // B begins while it runs: mixed, and fenced until its end is reported.
    begin_next_turn(&turn);
    let second = host_prompt(
        "The next prompt.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&second));
    reader.feed(lifecycle(&second, "started"));
    reader.feed(root_bash("fenced-test", SIZE_TEST, false));
    turn.wait_for_snapshots();
    assert!(turn.record(|record| record.engram.active_turn_checks[0].overlapped));
    reader.feed(root_bash_result("fenced-test", PASSING));

    // Its terminal notice retires it: a test B runs from then on keeps its
    // eligibility, and B, already mixed, stays mixed.
    reader.feed(task_ended("toolu-bg"));
    turn.record(|record| assert!(!record.claude_outstanding.any()));
    reader.feed(root_bash("clean-test", SIZE_TEST, false));
    turn.wait_for_snapshots();
    turn.record(|record| {
        let clean = record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == "clean-test")
            .expect("the later test starts a check");
        assert!(!clean.overlapped, "nothing foreign may still run");
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CONTINUITY_NEXT_GRANT)
        );
    });
    reader.feed(root_bash_result("clean-test", PASSING));
    reader.feed(named_result(&second));
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(
        checkpoints[1]["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "only the test run after the notice is B's evidence: {:#}",
        checkpoints[1]
    );

    // A grant that begins once nothing is outstanding begins clean.
    begin_next_turn(&turn);
    turn.record(|record| {
        assert_eq!(record.engram.active_grant_id.as_deref(), Some(THIRD_GRANT));
        assert_ne!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(THIRD_GRANT)
        );
    });
}

fn system_texts(record: &SessionRecord, starting: &str) -> usize {
    record
        .session
        .messages
        .iter()
        .filter(|message| {
            matches!(message, Message::Text { author: Author::System, text, .. }
                if text.starts_with(starting))
        })
        .count()
}

#[test]
fn a_stop_and_the_old_runtimes_exit_leave_its_command_outstanding_and_only_its_correlated_end_retires_it()
 {
    let turn = CheckedTurn::start("orphan-stop", true);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let prompt = host_prompt("Run it.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    reader.feed(capable_init());
    reader.feed(lifecycle(&prompt, "started"));
    reader.feed(root_bash("survivor", LONG_COMMAND, false));
    assert!(turn.record(|record| record.claude_outstanding.holds("survivor")));

    // The user stops the session; the old runtime's process exit is handled.
    turn.state
        .stop_session(&turn.session_id)
        .expect("the stop should succeed");
    let _ = turn
        .state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, None);
    turn.record(|record| {
        assert!(
            record.claude_outstanding.holds("survivor"),
            "neither the stop nor the exit shows the command ended"
        );
        assert!(claude_session_work_restricts(record));
    });
    // A late result the stopped runtime's reader still delivers retires
    // nothing either: the reader is no longer the session's.
    reader.feed(root_bash_result("survivor", "done"));
    assert!(turn.record(|record| record.claude_outstanding.holds("survivor")));

    // The same correlated end from the session's current runtime retires it.
    let (successor, _commands) = test_claude_runtime_handle("orphan-stop-successor");
    let successor_token = RuntimeToken::Claude(successor.runtime_id.clone());
    turn.record_mut(|record| {
        record.runtime = SessionRuntime::Claude(successor);
        record.claude_outstanding.entries[0].token = successor_token.clone();
    });
    turn.state.settle_claude_frame(
        &turn.session_id,
        &successor_token,
        &claude_frame_work(&root_bash_result("survivor", "done"), false),
    );
    assert!(!turn.record(|record| record.claude_outstanding.any()));
}

#[test]
fn an_abandoned_call_and_a_parents_end_without_proof_for_its_subagent_keep_work_outstanding() {
    let (turn, mut reader, _second, _ownership) = straddling_root("parent-end");
    // A's background task reports its end; its subagent's command reported
    // none, and nothing says the task's end ended it.
    reader.feed(task_ended("toolu-background"));
    turn.record(|record| {
        assert!(!record.claude_outstanding.holds("toolu-background"));
        assert!(
            record.claude_outstanding.holds("long-under-a"),
            "a parent's end is no proof its subagent's command ended"
        );
    });
    // An abandonment the recorder reports names no end of the command.
    turn.state.engram_host().observe(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        EngramRecorderObservation::CommandAbandoned {
            key: "long-under-a",
        },
    );
    assert!(turn.record(|record| record.claude_outstanding.holds("long-under-a")));
}

#[test]
fn a_replaced_runtimes_buffered_result_is_not_successor_work_but_its_new_command_excludes_the_grant()
 {
    let turn = CheckedTurn::start("stale-history", true);
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let prompt = host_prompt("The prompt.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    reader.feed(capable_init());
    reader.feed(lifecycle(&prompt, "started"));
    reader.feed(root_bash("old-command", LONG_COMMAND, false));
    // The runtime is replaced; the grant's turn goes on under the successor.
    let (successor, _commands) = test_claude_runtime_handle("stale-history-successor");
    turn.record_mut(|record| record.runtime = SessionRuntime::Claude(successor));
    let unassigned = turn.record(|record| record.unassigned_claude_observations.len());

    // The old reader's buffered result: history, not successor work during
    // the grant, and no proof that the old runtime's command ended. That
    // command is now outstanding work of a runtime that can never report its
    // end, overlapping the live grant: the grant is mixed by it, as by any of
    // the session's own outstanding work, when the result's end reconciles
    // the session's records with it (`engram_claude_interference.rs`).
    reader.feed(root_bash_result("old-command", "done"));
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT)
        );
        assert!(record.claude_outstanding.holds("old-command"));
        // The only line kept is the grant's exclusion; the result itself is
        // not kept as anyone's work.
        assert_eq!(record.unassigned_claude_observations.len(), unassigned + 1);
        let kept = &record.unassigned_claude_observations.back().unwrap().what;
        assert!(kept.contains("cannot be credited"), "{kept}");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
    });
    // A command it starts now is new activity: the live grant is excluded,
    // and nothing else of the successor's turn changes.
    reader.feed(root_bash("old-new-command", SIZE_TEST, false));
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT)
        );
        assert!(record.engram.active_turn_checks.is_empty());
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(record.unmediated_claude_turn, None);
    });
}

#[test]
fn a_late_duplicate_of_work_proven_complete_does_not_mix_the_successor_grant() {
    let turn = CheckedTurn::start("stale-duplicate", true);
    let generation = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let prompt = host_prompt("The prompt.", generation);
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    reader.feed(capable_init());
    reader.feed(lifecycle(&prompt, "started"));
    // The command ends, proven by its correlated result, while its runtime
    // is still the session's: nothing of it is outstanding.
    reader.feed(root_bash("done-command", LONG_COMMAND, false));
    reader.feed(root_bash_result("done-command", "done"));
    assert!(turn.record(|record| !record.claude_outstanding.holds("done-command")));
    // The runtime is replaced, and the old reader delivers the same result
    // again: receiving a stale frame alone shows no work overlapping the
    // grant, so it stays clean.
    let (successor, _commands) = test_claude_runtime_handle("stale-duplicate-successor");
    turn.record_mut(|record| record.runtime = SessionRuntime::Claude(successor));
    reader.feed(root_bash_result("done-command", "done"));
    turn.record(|record| {
        assert_eq!(record.engram.active_turn_mixed_attribution, None);
        assert!(!record.claude_outstanding.any());
    });
}

#[test]
fn an_excluded_session_says_why_once_and_says_so_again_when_the_work_ends() {
    let (turn, mut reader, second, _ownership) = straddling_root("evidence-notice");
    // The restriction took effect when A launched its background task: the
    // session said so then, in the transcript and in the line A's next
    // prompt carried (B's), and B, beginning while it lasts, repeats nothing.
    reader.feed(root_bash("b-test", SIZE_TEST, false));
    reader.feed(root_bash_result("b-test", PASSING));
    reader.feed(nested_command(
        "more-under-a",
        "toolu-background",
        "echo more",
    ));
    turn.record(|record| {
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            1
        );
        assert!(record.claude_outstanding.restricting);
        assert!(record.session.messages.iter().any(|message| matches!(
            message,
            Message::Text { author: Author::System, text, .. }
                if text.contains("TermAl has no reset for this")
        )));
    });
    // The work reports its end: the restriction is lifted, said once.
    reader.feed(nested_bash_result("long-under-a", "toolu-background"));
    reader.feed(nested_bash_result("more-under-a", "toolu-background"));
    reader.feed(task_ended("toolu-background"));
    turn.record(|record| {
        assert!(!record.claude_outstanding.any());
        assert_eq!(
            system_texts(record, "Evidence restriction lifted for this session"),
            1
        );
    });
    // B's fenced test is refused with the outstanding work as its reason.
    reader.feed(named_result(&second));
    turn.record(|record| {
        let pending = record
            .engram
            .pending_source_root_line
            .clone()
            .unwrap_or_default();
        assert!(
            pending.contains("this session's own Claude work"),
            "the refused check names its reason: {pending}"
        );
        assert!(!pending.contains("Run it again on its own"), "{pending}");
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            1
        );
    });
}

#[test]
fn another_sessions_outstanding_work_fences_checks_only_in_the_worktree_it_may_write() {
    for (label, same_worktree) in [("cross-session-same", true), ("cross-session-other", false)] {
        let turn = CheckedTurn::start(label, true);
        let other = other_session_in_worktree(&turn);
        let other_token = RuntimeToken::Claude(format!("{label}-stopped"));
        let root_key = engram_worktree_root(&turn.root);
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            let record = &mut inner.sessions[index];
            record.session.status = SessionStatus::Idle;
            record.claude_outstanding.register(ClaudeOutstandingEntry {
                token: other_token,
                key: "orphaned".to_owned(),
                origin: ClaudeWorkOrigin::Unattributed,
                background: true,
                nested: false,
                self_gate: false,
                locations: vec![Some(if same_worktree {
                    root_key.clone()
                } else {
                    "another-worktree".to_owned()
                })],
                registered_at: EngramHost::interference_tick(),
            });
        }
        let mut recorder = turn.recorder();
        recorder
            .command_started("check", SIZE_TEST)
            .expect("the start should record");
        turn.wait_for_snapshots();
        turn.record(|record| {
            let check = &record.engram.active_turn_checks[0];
            assert_eq!(check.overlapped, same_worktree, "{label}");
            assert_eq!(
                check.fenced_by_outstanding.is_some(),
                same_worktree,
                "{label}"
            );
        });
    }
}

#[test]
fn an_acceptance_evaluation_requested_under_a_restriction_is_told_why() {
    let (turn, _reader, _second, _ownership) = straddling_root("evaluation-notice");
    let place = EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned());
    let notice = turn
        .state
        .claude_evidence_restriction_notice(&turn.session_id, &place)
        .expect("the restriction is told");
    assert!(notice.contains("cannot pass on those runs"), "{notice}");
    let clean = CheckedTurn::start("evaluation-notice-clean", true);
    assert_eq!(
        clean.state.claude_evidence_restriction_notice(
            &clean.session_id,
            &EngramBasisPlace::Workdir(clean.root.to_string_lossy().into_owned())
        ),
        None
    );
}

/// A claimed root with one begun grant, its reader, and its prompt's turn
/// open.
fn open_prompt(label: &str) -> (CheckedTurn, ClaudeReaderOn, ClaudePromptCommand) {
    let turn = CheckedTurn::start(label, true);
    let ownership = new_claude_turn_ownership();
    let mut reader = ClaudeReaderOn::new(&turn, ownership.clone());
    let prompt = host_prompt(
        "The prompt.",
        turn.record(|record| record.active_turn_generation),
    );
    lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
    reader.feed(capable_init());
    reader.feed(lifecycle(&prompt, "started"));
    (turn, reader, prompt)
}

#[test]
fn a_clean_top_level_test_keeps_its_credit_when_no_claude_work_is_outstanding() {
    let (turn, mut reader, prompt) = open_prompt("clean-root");
    reader.feed(root_bash("root-test", SIZE_TEST, false));
    turn.wait_for_snapshots();
    turn.record(|record| {
        let check = &record.engram.active_turn_checks[0];
        assert!(!check.overlapped);
        assert_eq!(check.fenced_by_outstanding, None);
    });
    reader.feed(root_bash_result("root-test", PASSING));
    turn.record(|record| {
        assert!(!record.claude_outstanding.any(), "its result retired it");
        assert_eq!(record.engram.active_turn_mixed_attribution, None);
    });
    reader.feed(named_result(&prompt));
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(
        checkpoints[0]["verification_evidence"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{:#}",
        checkpoints[0]
    );
    turn.record(|record| {
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            0
        );
    });
}

#[test]
fn the_grant_that_launches_background_work_is_excluded_and_told_at_once() {
    let (turn, mut reader, _prompt) = open_prompt("launching-grant");
    reader.feed(root_bash("toolu-bg", LONG_COMMAND, true));
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT),
            "the launching grant's interval overlaps its background work"
        );
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            1
        );
        let pending = record
            .engram
            .pending_source_root_line
            .clone()
            .unwrap_or_default();
        assert!(
            pending.contains("Evidence restricted for this session"),
            "{pending}"
        );
        let entry = &record.claude_outstanding.entries[0];
        assert!(
            entry
                .locations
                .contains(&Some(engram_worktree_root(&turn.root))),
            "it may write in the session's worktree: {:?}",
            entry.locations
        );
    });
}

#[test]
fn a_command_moved_to_the_background_outlives_its_result_until_its_notice() {
    let (turn, mut reader, _prompt) = open_prompt("moved-to-background");
    reader.feed(root_bash("toolu-moved", LONG_COMMAND, false));
    reader.feed(
        json!({"type": "user", "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "toolu-moved",
            "content": "Command did not complete within its 120s timeout and was moved to the \
                background (ID: b7)."}]},
        "tool_use_result": {"stdout": "", "stderr": "", "interrupted": false,
            "backgroundTaskId": "b7"}}),
    );
    turn.record(|record| {
        let entry = record
            .claude_outstanding
            .entries
            .iter()
            .find(|entry| entry.key == "toolu-moved")
            .expect("the moved command is still outstanding");
        assert!(entry.background);
        assert!(claude_session_work_restricts(record));
    });
    reader.feed(task_ended("toolu-moved"));
    assert!(!turn.record(|record| record.claude_outstanding.any()));
}

#[test]
fn deleting_or_collecting_a_session_keeps_its_work_fencing_the_workspace() {
    for (label, collect) in [("orphan-delete", false), ("orphan-collect", true)] {
        let turn = CheckedTurn::start(label, true);
        let other = other_session_in_worktree(&turn);
        let root_key = engram_worktree_root(&turn.root);
        {
            let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&other).expect("other session");
            inner.sessions[index]
                .claude_outstanding
                .register(ClaudeOutstandingEntry {
                    token: RuntimeToken::Claude(format!("{label}-runtime")),
                    key: "left-behind".to_owned(),
                    origin: ClaudeWorkOrigin::Unattributed,
                    background: true,
                    nested: false,
                    self_gate: false,
                    locations: vec![Some(root_key.clone())],
                    registered_at: EngramHost::interference_tick(),
                });
            if collect {
                inner.retain_sessions(|record| record.session.id != other);
            }
        }
        if !collect {
            turn.state
                .kill_session(&other)
                .expect("the session should be deleted");
        }
        {
            let inner = turn.state.inner.lock().expect("state mutex poisoned");
            assert!(inner.find_session_index(&other).is_none(), "{label}");
            assert_eq!(inner.claude_orphaned_work.len(), 1, "{label}");
        }
        // A test of another session in that worktree is fenced, with the
        // deleted session named as the cause.
        let mut recorder = turn.recorder();
        recorder
            .command_started("check", SIZE_TEST)
            .expect("the start should record");
        turn.wait_for_snapshots();
        turn.record(|record| {
            let check = &record.engram.active_turn_checks[0];
            assert!(check.overlapped, "{label}");
            assert_eq!(
                check.fenced_by_outstanding,
                Some(ClaudeHazardCause::DeletedSession {
                    session_id: other.clone()
                }),
                "{label}"
            );
        });
        // A requester measured there is told why.
        let notice = turn
            .state
            .claude_evidence_restriction_notice(
                &turn.session_id,
                &EngramBasisPlace::Workdir(turn.root.to_string_lossy().into_owned()),
            )
            .expect("the requester is told");
        assert!(notice.contains("a deleted session"), "{label}: {notice}");
        assert!(notice.contains(&other), "{label}: {notice}");
    }
}

/// Whether `work`, left by a deleted session, may write in the worktree with
/// key `root` for an open check of another session, by the interference rule.
fn may_write_in(work: &ClaudeOutstandingWork, root: &str) -> bool {
    let owner = EngramHazardOwner::Deleted {
        cause: ClaudeHazardCause::DeletedSession {
            session_id: "gone".to_owned(),
        },
    };
    let subject = EngramInterferenceSubject {
        session: usize::MAX,
        key: "check",
        root: root.to_owned(),
        ended_at: None,
        simple_full_gate: false,
    };
    engram_hazards_of_work(&owner, work, None, 0, true, None)
        .iter()
        .any(|hazard| engram_claude_interference(hazard, &subject).is_some())
}

#[test]
fn outstanding_work_keeps_every_place_it_may_write_until_it_ends() {
    let token = RuntimeToken::Claude("locations".to_owned());
    let entry = |key: &str, locations: Vec<Option<String>>| ClaudeOutstandingEntry {
        token: token.clone(),
        key: key.to_owned(),
        origin: ClaudeWorkOrigin::Unattributed,
        background: true,
        nested: true,
        self_gate: false,
        locations,
        registered_at: EngramHost::interference_tick(),
    };
    let mut work = ClaudeOutstandingWork::default();
    // Registered where its session worked and the root its grant named.
    work.register(entry(
        "task",
        vec![Some("workdir".to_owned()), Some("named-root".to_owned())],
    ));
    // Later placed elsewhere: the place joins, nothing is lost.
    work.place("task", &[Some("placed".to_owned())]);
    for root in ["workdir", "named-root", "placed"] {
        assert!(may_write_in(&work, root), "{root}");
    }
    assert!(
        !may_write_in(&work, "disjoint"),
        "a disjoint worktree stays eligible"
    );
    // A place TermAl could not name stands for any.
    work.place("task", &[None]);
    assert!(may_write_in(&work, "disjoint"));

    // Detail dropped past the bound keeps where the dropped work may write.
    let mut bounded = ClaudeOutstandingWork::default();
    let first = entry("first", vec![Some("evicted-root".to_owned())]);
    let first_registered = first.registered_at;
    bounded.register(first);
    for index in 0..CLAUDE_OUTSTANDING_WORK_LIMIT {
        bounded.register(entry(
            &format!("more-{index}"),
            vec![Some("other".to_owned())],
        ));
    }
    assert!(bounded.unknown);
    assert!(!bounded.holds("first"));
    assert!(may_write_in(&bounded, "evicted-root"));
    // And since when: the earliest registration of what was dropped.
    assert_eq!(bounded.unknown_registered_at, first_registered);

    // A removed session's work keeps its places on the host, and taking it
    // twice adds nothing.
    let mut host = ClaudeOutstandingWork::default();
    host.absorb(&bounded);
    host.absorb(&bounded);
    assert_eq!(host.entries.len(), bounded.entries.len());
    assert!(host.unknown && may_write_in(&host, "evicted-root"));
    assert_eq!(host.unknown_registered_at, first_registered);
}

#[test]
fn a_command_moved_to_the_background_excludes_its_grant_before_its_partial_output_can_pass() {
    let (turn, mut reader, prompt) = open_prompt("moved-fenced");
    reader.feed(root_bash("moved-test", SIZE_TEST, false));
    turn.wait_for_snapshots();
    assert!(turn.record(|record| !record.engram.active_turn_checks[0].overlapped));
    // Its output so far already shows passing tests, but Claude Code moved
    // it to the background: it runs on.
    reader.feed(
        json!({"type": "user", "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "moved-test",
            "content": "Command did not complete within its 120s timeout and was moved to the \
                background (ID: b9)."}]},
        "tool_use_result": {"stdout": PASSING, "stderr": "", "interrupted": false,
            "backgroundTaskId": "b9"}}),
    );
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT),
            "the launching grant is excluded"
        );
        let check = &record.engram.active_turn_checks[0];
        assert!(check.overlapped);
        assert_eq!(
            check.fenced_by_outstanding,
            Some(ClaudeHazardCause::OwnSession)
        );
        assert_eq!(
            check.end.as_ref().map(|end| end.exit),
            Some(EngramCommandExit::NotFinished),
            "the move is not its end"
        );
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            1
        );
    });
    reader.feed(named_result(&prompt));
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(
        checkpoints[0]["verification_evidence"]
            .as_array()
            .map_or(0, Vec::len),
        0,
        "its partial output is no evidence: {:#}",
        checkpoints[0]
    );
}

#[test]
fn only_a_top_level_simple_full_gate_call_is_its_own_gate() {
    let started = |frame: Value, nested: bool| claude_frame_work(&frame, nested).started;
    let bash = |command: &str| {
        json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
            "id": "call", "name": "Bash", "input": {"command": command,
                "run_in_background": true}}]}})
    };
    let gate = "pushd \"C:/work/root\" && node scripts/test-launcher.mjs full";
    assert_eq!(
        started(bash(gate), false),
        vec![("call".to_owned(), true, true)]
    );
    assert_eq!(
        started(bash(gate), true),
        vec![("call".to_owned(), true, false)],
        "a subagent's launch is not"
    );
    assert_eq!(
        started(
            bash("cd C:/work/root && node scripts/test-launcher.mjs full && echo done"),
            false
        ),
        vec![("call".to_owned(), true, false)],
        "a compound launch is not"
    );
    assert_eq!(
        started(
            bash("node scripts/test-launcher.mjs focused -- cargo test"),
            false
        ),
        vec![("call".to_owned(), true, false)],
        "another mode is not"
    );
}

#[test]
fn a_one_off_exclusion_with_nothing_outstanding_gives_no_restriction_notice() {
    let (turn, mut reader, _prompt) = open_prompt("one-off-exclusion");
    // A subagent's file write, by a parent this reader never saw: the grant
    // is excluded from it, but nothing is left running.
    reader.feed(unknown_parent_write("one-off"));
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_mixed_attribution.as_deref(),
            Some(CHECK_GRANT)
        );
        assert!(!claude_session_work_restricts(record));
        assert_eq!(
            system_texts(record, "Evidence restricted for this session"),
            0
        );
        assert_eq!(system_texts(record, "Evidence restriction lifted"), 0);
    });
}
