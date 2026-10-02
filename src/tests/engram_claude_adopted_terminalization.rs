// Owns the tests that a turn Claude Code started by itself, adopted by the
// host, keeps its "granted nothing" provenance until it actually
// terminalizes, on every lifecycle path that can end it: an immediate or a
// deferred completion or error and its replay after a stop, a runtime exit,
// a reader failure, the atomic failure in each of its modes, a user Stop,
// session deletion and revocation teardown, each through the host's own
// entry point. With a leftover grant on the session, no terminal path of
// the adopted turn reports execution on that grant, an independent
// settlement closes it with no adopted report (repeating an earlier
// attempt's report rather than replacing it), a successor generation is not
// excluded, and a change of context between a checkpoint's plan and claim
// closes nothing. Each test drives the host's real lifecycle functions and
// reads the checkpoint requests Engram receives.
// Does not own the frame-ownership rules (src/tests/claude_turn_ownership.rs),
// the frame application (src/tests/claude_frame_router.rs) or the adoption
// and attribution tests (src/tests/engram_claude_runtime_turns.rs, whose
// frame helpers this module repeats). New module.
use super::*;

const LEFTOVER: &str = "leftover-grant";

fn task_notification() -> Value {
    json!({"type": "system", "subtype": "task_notification", "task_id": "b1",
        "tool_use_id": "toolu-b1", "status": "completed"})
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "probe"})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
}

fn result(is_error: bool) -> Value {
    json!({"type": "result", "subtype": if is_error { "error" } else { "success" },
        "is_error": is_error, "result": "done"})
}

fn checkpoint_requests(turn: &CheckedTurn) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .filter(|request| request.request["operation"] == "turn_checkpoint")
        .map(|request| request.request)
        .collect()
}

/// A granted turn that completed, a grant left open on the session after it,
/// and a turn Claude Code then started by itself, adopted by the idle
/// session. Returns its runtime token, its turn generation and the runtime's
/// turn ownership.
fn adopted_with_leftover_grant(
    label: &str,
    extra_checkpoints: Vec<ScriptedEngramControlResponse>,
) -> (CheckedTurn, RuntimeToken, u64, ClaudeTurnOwnership) {
    let turn = CheckedTurn::start_with(
        label,
        true,
        None,
        std::iter::once(checkpoint_reply(CHECK_GRANT))
            .chain(extra_checkpoints)
            .collect(),
    );
    turn.finish();
    turn.record_mut(|record| record.engram.active_grant_id = Some(LEFTOVER.to_owned()));
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&turn, &token, &ownership, frame);
    }
    let generation = turn.record(|record| {
        assert!(
            unmediated_claude_turn_is_current(record),
            "the turn is adopted"
        );
        record.active_turn_generation
    });
    (turn, token, generation, ownership)
}

/// Feeds one frame as the Claude stdout reader's ownership and finalization
/// steps do.
fn feed(turn: &CheckedTurn, token: &RuntimeToken, ownership: &ClaudeTurnOwnership, frame: Value) {
    if frame.get("type").and_then(Value::as_str) == Some("result") {
        let owner = lock_claude_turn_ownership(ownership).close_for_result(&frame);
        let error = frame.get("is_error").and_then(Value::as_bool) == Some(true);
        turn.state
            .finish_claude_result(&turn.session_id, token, owner, error.then_some("it failed"))
            .expect("the result should be routed");
    } else {
        turn.state
            .observe_claude_frame_ownership(&turn.session_id, token, ownership, &frame);
    }
}

fn assert_leftover_untouched(turn: &CheckedTurn, checkpoints_before: usize, path: &str) {
    assert_eq!(
        checkpoint_requests(turn).len(),
        checkpoints_before,
        "{path}: no checkpoint reports the adopted turn on the leftover grant"
    );
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some(LEFTOVER),
        "{path}: the leftover grant stays for its own settlement"
    );
}

#[test]
fn an_adopted_turns_immediate_success_or_error_checkpoints_no_leftover_grant() {
    for is_error in [false, true] {
        let (turn, token, generation, ownership) =
            adopted_with_leftover_grant(&format!("adopted-immediate-{is_error}"), Vec::new());
        let before = checkpoint_requests(&turn).len();
        feed(&turn, &token, &ownership, result(is_error));
        turn.record(|record| {
            assert_eq!(
                record.session.status,
                if is_error {
                    SessionStatus::Error
                } else {
                    SessionStatus::Idle
                }
            );
            assert_eq!(record.active_turn_generation, generation);
            assert!(
                unmediated_claude_turn_is_current(record),
                "the provenance outlives the turn's closed segment"
            );
        });
        assert_leftover_untouched(&turn, before, &format!("immediate (error: {is_error})"));
    }
}

#[test]
fn a_deferred_adopted_finish_replayed_after_a_stop_still_checkpoints_no_leftover_grant() {
    for is_error in [false, true] {
        let (turn, token, generation, ownership) =
            adopted_with_leftover_grant(&format!("adopted-deferred-{is_error}"), Vec::new());
        let before = checkpoint_requests(&turn).len();
        // The result arrives while a stop owns the runtime: the finish is
        // deferred, and the turn's segment closes with the result.
        turn.record_mut(|record| record.runtime_stop_in_progress = true);
        feed(&turn, &token, &ownership, result(is_error));
        let callbacks = turn.record(|record| {
            assert_eq!(
                record.session.status,
                SessionStatus::Active,
                "not yet terminal"
            );
            assert_eq!(record.unmediated_claude_turn, None, "the segment closed");
            assert!(
                unmediated_claude_turn_is_current(record),
                "the provenance holds"
            );
            record.deferred_stop_callbacks.clone()
        });
        assert_eq!(callbacks.len(), 1, "the finish is deferred");
        assert_leftover_untouched(&turn, before, "deferred");

        // The stop fails and releases its fence; the deferred finish replays
        // on the same generation.
        turn.record_mut(|record| {
            record.runtime_stop_in_progress = false;
            record.deferred_stop_callbacks.clear();
        });
        turn.state
            .replay_deferred_runtime_stop_callbacks(&turn.session_id, &token, callbacks);
        turn.record(|record| {
            assert_eq!(
                record.session.status,
                if is_error {
                    SessionStatus::Error
                } else {
                    SessionStatus::Idle
                }
            );
            assert_eq!(record.active_turn_generation, generation);
        });
        assert_leftover_untouched(&turn, before, &format!("replayed (error: {is_error})"));
    }
}

#[test]
fn a_runtime_exit_or_missing_runtime_failure_of_an_adopted_turn_checkpoints_no_leftover_grant() {
    let (turn, token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-exit", Vec::new());
    let before = checkpoint_requests(&turn).len();
    turn.state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, Some("the runtime exited"))
        .expect("the exit should be handled");
    turn.record(|record| {
        assert_ne!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
    });
    assert_leftover_untouched(&turn, before, "runtime exit");

    let (turn, _token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-missing-runtime", Vec::new());
    let before = checkpoint_requests(&turn).len();
    turn.record_mut(|record| record.clear_runtime());
    turn.state
        .fail_active_turn_if_runtime_missing(&turn.session_id, generation, "the runtime is gone")
        .expect("the missing-runtime failure should be handled");
    assert_leftover_untouched(&turn, before, "missing-runtime atomic failure");

    let (turn, token, _generation, _ownership) =
        adopted_with_leftover_grant("adopted-reader-failure", Vec::new());
    let before = checkpoint_requests(&turn).len();
    turn.state
        .fail_turn_if_runtime_matches(&turn.session_id, &token, "the reader failed")
        .expect("the reader failure should be handled");
    assert_leftover_untouched(&turn, before, "reader failure");
}

#[test]
fn a_settlement_repeats_an_earlier_attempts_report_instead_of_an_empty_one() {
    let (turn, _token, _generation, _ownership) =
        adopted_with_leftover_grant("adopted-cached", vec![checkpoint_reply(LEFTOVER)]);
    // An earlier, uncertain attempt for the leftover grant built its report.
    let cached = turn.record(|record| {
        record
            .engram
            .active_turn_report
            .clone()
            .map(|(_, report)| report)
            .expect("the first turn's report is cached")
    });
    assert!(
        !cached.observations.is_empty(),
        "the fixture report carries an observation"
    );
    turn.record_mut(|record| {
        record.engram.active_turn_report = Some((LEFTOVER.to_owned(), cached.clone()));
    });
    let before = checkpoint_requests(&turn).len();
    turn.state.checkpoint_engram_turn_off_lock(
        &turn.session_id,
        EngramCheckpointPurpose::Teardown,
        None,
        None,
        EngramNextIntent::Exit,
        Some(EngramExecutionOutcome::Unknown),
        None,
    );
    let checkpoints = checkpoint_requests(&turn);
    assert_eq!(checkpoints.len(), before + 1);
    let first_report = checkpoint_requests(&turn)[0]["observations"].clone();
    assert_eq!(
        checkpoints[before]["observations"], first_report,
        "the settlement repeats the earlier attempt's report verbatim"
    );
}

/// The settlement checkpoint a teardown sent for the leftover grant, asserted
/// to carry no execution of the adopted turn.
fn assert_settled_without_adopted_report(turn: &CheckedTurn, before: usize, path: &str) {
    let checkpoints = checkpoint_requests(turn);
    assert_eq!(checkpoints.len(), before + 1, "{path}: one settlement");
    let settlement = &checkpoints[before];
    assert_eq!(settlement["grant_id"], LEFTOVER, "{path}");
    assert!(
        settlement
            .get("observations")
            .and_then(Value::as_array)
            .is_none_or(|observations| observations.is_empty()),
        "{path}: a settlement reports no execution of the adopted turn: {settlement:#}"
    );
}

/// The failed-Stop handover that gives revocation teardown its fence, as the
/// host performs it, for the session's current runtime.
fn revocation_target(turn: &CheckedTurn) -> EngramMcpRuntimeRevocationTarget {
    let mut inner = turn.state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&turn.session_id)
        .expect("the session exists");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    let token = record.runtime.runtime_token().expect("runtime");
    let stop_owner_generation = record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token);
    record.engram_mcp_revocation_pending = true;
    take_pending_engram_mcp_revocation_after_stop_failure_locked(
        &mut inner,
        index,
        stop_owner_generation,
        StopSessionOptions::default(),
    )
    .expect("the failed Stop hands its fence to revocation")
}

fn revoke(turn: &CheckedTurn, context: &str) {
    let target = revocation_target(turn);
    turn.state
        .teardown_revoked_engram_mcp_runtimes(
            EngramMcpRuntimeRevocationBatch {
                targets: vec![target],
                pending_session_ids: Vec::new(),
                newly_pending_session_ids: Vec::new(),
            },
            context,
        )
        .expect("the revocation teardown should complete");
}

#[test]
fn deletion_and_revocation_teardown_settle_a_leftover_grant_without_the_adopted_report() {
    // Session deletion, through the host's own entry point.
    let (turn, _token, _generation, _ownership) =
        adopted_with_leftover_grant("adopted-deletion", vec![checkpoint_reply(LEFTOVER)]);
    let before = checkpoint_requests(&turn).len();
    turn.state
        .kill_session(&turn.session_id)
        .expect("the session should be deleted");
    assert_settled_without_adopted_report(&turn, before, "session deletion");

    // Revocation teardown, after a failed Stop handed it the fence.
    let (turn, _token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-revocation", vec![checkpoint_reply(LEFTOVER)]);
    let before = checkpoint_requests(&turn).len();
    revoke(&turn, "test revocation of an adopted turn");
    assert_settled_without_adopted_report(&turn, before, "revocation teardown");
    turn.record(|record| {
        assert_eq!(
            record.engram.active_grant_id, None,
            "the leftover grant is settled"
        );
        assert_eq!(record.active_turn_generation, generation);
        assert!(!record.runtime_stop_in_progress);
    });
}

#[test]
fn a_mediated_turns_deletion_and_revocation_still_report_its_own_execution() {
    for path in ["deletion", "revocation"] {
        let turn = CheckedTurn::start_with(
            &format!("mediated-{path}"),
            true,
            None,
            vec![checkpoint_reply(CHECK_GRANT)],
        );
        if path == "deletion" {
            turn.state
                .kill_session(&turn.session_id)
                .expect("the session should be deleted");
        } else {
            revoke(&turn, "test revocation of a mediated turn");
        }
        let checkpoints = checkpoint_requests(&turn);
        assert_eq!(checkpoints.len(), 1, "{path}");
        assert_eq!(checkpoints[0]["grant_id"], CHECK_GRANT, "{path}");
        let observations = checkpoints[0]["observations"]
            .as_array()
            .unwrap_or_else(|| panic!("{path}: the mediated turn reports its execution"));
        assert_eq!(observations.len(), 1, "{path}");
        assert_eq!(observations[0]["outcome"], "unknown", "{path}");
    }
}

#[test]
fn the_atomic_failure_of_an_adopted_turn_checkpoints_no_leftover_grant_in_any_mode() {
    // A matching runtime's confirmed failure, under its terminalization claim.
    let (turn, token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-atomic-matching", Vec::new());
    let before = checkpoint_requests(&turn).len();
    let owner_generation = turn
        .state
        .claim_turn_terminalization_if_runtime_matches(&turn.session_id, &token, generation)
        .expect("the claim should be taken")
        .expect("the matching runtime is claimed");
    assert!(
        turn.state
            .fail_turn_and_clear_runtime_if_owned(
                &turn.session_id,
                &token,
                generation,
                owner_generation,
                "the runtime failed",
            )
            .expect("the atomic failure should apply"),
        "the owned failure terminalizes the adopted turn"
    );
    assert_leftover_untouched(&turn, before, "atomic MatchingRuntime failure");
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Error));

    // A rejected runtime-command delivery.
    let (turn, token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-atomic-rejected", Vec::new());
    let before = checkpoint_requests(&turn).len();
    assert!(
        turn.state
            .fail_rejected_turn_delivery(
                &turn.session_id,
                &token,
                generation,
                None,
                "the delivery was rejected",
            )
            .expect("the rejected delivery should be recorded"),
        "the rejected delivery terminalizes the adopted turn"
    );
    assert_leftover_untouched(&turn, before, "atomic RejectedDelivery failure");
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Error));
}

#[test]
fn a_successor_generation_is_not_excluded_and_a_changed_context_closes_nothing() {
    // A successor generation begins: the adopted provenance is inert, and the
    // successor's own terminal path reports on its grant.
    let (turn, _token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-successor", Vec::new());
    turn.record_mut(|record| {
        record.active_turn_generation = generation + 1;
        assert!(!unmediated_claude_turn_is_current(record));
    });
    {
        let inner = turn.state.inner.lock().expect("state mutex poisoned");
        assert!(
            AppState::engram_checkpoint_grant_locked(
                &inner,
                &turn.session_id,
                EngramCheckpointPurpose::TurnTerminal,
                None,
                Some(generation + 1),
                None,
            )
            .is_some_and(|(_, _, _, resolved)| resolved == EngramCheckpointPurpose::TurnTerminal),
            "the successor generation is not the adopted turn"
        );
    }

    // Between a teardown's plan (a settlement of the leftover grant while the
    // adopted turn is current) and its claim, a successor generation begins:
    // the claim resolves differently and closes nothing.
    let (turn, _token, generation, _ownership) =
        adopted_with_leftover_grant("adopted-race", Vec::new());
    let before = checkpoint_requests(&turn).len();
    let gate = install_test_engram_turn_report_gate(&turn.state, &turn.session_id);
    let teardown = {
        let state = turn.state.clone();
        let session_id = turn.session_id.clone();
        std::thread::spawn(move || {
            state.checkpoint_engram_turn_off_lock(
                &session_id,
                EngramCheckpointPurpose::Teardown,
                None,
                None,
                EngramNextIntent::Exit,
                Some(EngramExecutionOutcome::Unknown),
                None,
            )
        })
    };
    gate.wait_until_claimed();
    turn.record_mut(|record| record.active_turn_generation = generation + 1);
    gate.release();
    let outcome = teardown.join().expect("the teardown should not panic");
    assert!(
        matches!(outcome, EngramCheckpointOutcome::Skipped),
        "{outcome:?}"
    );
    assert_leftover_untouched(&turn, before, "plan/claim race");
}

#[test]
fn an_error_session_adopts_a_runtime_started_turn_and_its_result_ends_it() {
    let turn = CheckedTurn::start("adopted-from-error", true);
    turn.finish();
    turn.record_mut(|record| record.session.status = SessionStatus::Error);
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let before = turn.record(|record| record.active_turn_generation);
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&turn, &token, &ownership, frame);
    }
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, before + 1);
        assert!(unmediated_claude_turn_is_current(record));
    });
    feed(&turn, &token, &ownership, result(false));
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Idle));
}

#[test]
fn a_user_stop_of_an_adopted_turn_checkpoints_no_leftover_grant() {
    let (turn, _token, _generation, _ownership) =
        adopted_with_leftover_grant("adopted-stop", Vec::new());
    let before = checkpoint_requests(&turn).len();
    turn.state
        .stop_session(&turn.session_id)
        .expect("the stop should be accepted");
    turn.record(|record| assert_ne!(record.session.status, SessionStatus::Active));
    assert_leftover_untouched(&turn, before, "user Stop");
}

#[test]
fn an_adopted_interval_ended_by_an_unresolved_result_checkpoints_no_leftover_grant() {
    // A granted turn completed, a grant was left open, and the session is
    // idle. Every frame then goes through the reader's own application
    // function, which adopts the turn Claude Code starts by itself.
    let turn = CheckedTurn::start("adopted-unresolved", true);
    turn.finish();
    turn.record_mut(|record| record.engram.active_grant_id = Some(LEFTOVER.to_owned()));
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let context = ClaudeRuntimeContext::new(
        turn.state.clone(),
        turn.session_id.clone(),
        token,
        new_claude_turn_ownership(),
        Arc::new(Mutex::new(None)),
        "/tmp".to_owned(),
    );
    let mut frames = ClaudeReaderFrames::new(&context, input_tx, None);
    let mut recorder = SessionRecorder::new(turn.state.clone(), turn.session_id.clone());
    let mut apply = |frame: Value| {
        let outcome = apply_claude_frame(&context, &mut frames, &mut recorder, &frame);
        assert_eq!(outcome.next, ClaudeFrameApplied::Continue);
    };
    for frame in [
        task_notification(),
        init(),
        assistant_text("the runtime's own work"),
    ] {
        apply(frame);
    }
    let generation = turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active, "adopted");
        record.active_turn_generation
    });
    let before = checkpoint_requests(&turn).len();
    apply(
        json!({"type": "result", "subtype": "success", "is_error": false,
        "result": "done", "user_message_uuid": "99999999-0000-4000-8000-000000000009"}),
    );
    turn.record(|record| {
        assert_eq!(
            record.session.status,
            SessionStatus::Idle,
            "the interval closed"
        );
        assert_eq!(record.active_turn_generation, generation);
        assert!(
            unmediated_claude_turn_is_current(record),
            "the provenance holds"
        );
    });
    assert_leftover_untouched(
        &turn,
        before,
        "adopted interval ended by an unresolved result",
    );
}
