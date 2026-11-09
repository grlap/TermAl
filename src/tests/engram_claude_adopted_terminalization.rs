// Adopted Claude lifecycle ownership through actual terminalization, Stop,
// reader failure, deferred callback and revocation paths. Old grant fields
// are passive history; ordinary observations never settle or execute them.
use super::*;

pub(super) const HISTORICAL_GRANT: &str = "leftover-grant";

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

fn forbidden_requests(turn: &EOCheckedTurn) -> Vec<Value> {
    turn.transport
        .requests()
        .into_iter()
        .filter(|sent| {
            matches!(
                sent.request["operation"].as_str(),
                Some(
                    "session_bind"
                        | "session_status"
                        | "turn_grant_read"
                        | "turn_evaluate"
                        | "turn_begin"
                        | "turn_checkpoint"
                        | "verification_record"
                        | "verification_bind"
                )
            )
        })
        .map(|sent| sent.request)
        .collect()
}

// The old grant is opaque history, not a current execution/settlement owner.
fn adopted_with_historical_grant(
    label: &str,
) -> (EOCheckedTurn, RuntimeToken, u64, ClaudeTurnOwnership) {
    let turn = EOCheckedTurn::start(label, true);
    turn.finish();
    turn.record_mut(|record| {
        record
            .engram
            .historical_control
            .insert("engramOpenGrantId".into(), json!(HISTORICAL_GRANT));
    });
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    let ownership = new_claude_turn_ownership();
    for frame in [task_notification(), init(), assistant_text("noticed")] {
        feed(&turn, &token, &ownership, frame);
    }
    let generation = turn.record(|record| {
        assert!(unmediated_claude_turn_is_current(record));
        assert!(
            record.engram.unadmitted_active_turn.is_some(),
            "the adopted interval has its own EO trace"
        );
        record.active_turn_generation
    });
    (turn, token, generation, ownership)
}
/// Feeds one frame as the Claude stdout reader's ownership and finalization
/// steps do.
fn feed(turn: &EOCheckedTurn, token: &RuntimeToken, ownership: &ClaudeTurnOwnership, frame: Value) {
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

fn assert_historical_grant_untouched(turn: &EOCheckedTurn, before: usize, path: &str) {
    assert_eq!(
        before, 0,
        "the ordinary fixture starts with no gated/native request"
    );
    assert!(
        forbidden_requests(turn).is_empty(),
        "{path}: no current request consumes old grant history"
    );
    turn.record(|record| {
        assert_eq!(
            record.engram.historical_control.get("engramOpenGrantId"),
            Some(&json!(HISTORICAL_GRANT)),
            "{path}: old grant history is unchanged"
        );
    });
}
#[test]
fn an_adopted_turns_immediate_success_or_error_preserves_historical_grant_without_control_calls() {
    for is_error in [false, true] {
        let (turn, token, generation, ownership) =
            adopted_with_historical_grant(&format!("adopted-immediate-{is_error}"));
        let before = forbidden_requests(&turn).len();
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
        assert_historical_grant_untouched(&turn, before, &format!("immediate (error: {is_error})"));
    }
}

#[test]
fn a_deferred_adopted_finish_replayed_after_a_stop_still_preserves_historical_grant_without_control_calls()
 {
    for is_error in [false, true] {
        let (turn, token, generation, ownership) =
            adopted_with_historical_grant(&format!("adopted-deferred-{is_error}"));
        let before = forbidden_requests(&turn).len();
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
        assert_historical_grant_untouched(&turn, before, "deferred");

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
        assert_historical_grant_untouched(&turn, before, &format!("replayed (error: {is_error})"));
    }
}

#[test]
fn a_runtime_exit_or_missing_runtime_failure_of_an_adopted_turn_preserves_historical_grant_without_control_calls()
 {
    let (turn, token, generation, _ownership) = adopted_with_historical_grant("adopted-exit");
    let before = forbidden_requests(&turn).len();
    turn.state
        .handle_runtime_exit_if_matches(&turn.session_id, &token, Some("the runtime exited"))
        .expect("the exit should be handled");
    turn.record(|record| {
        assert_ne!(record.session.status, SessionStatus::Active);
        assert_eq!(record.active_turn_generation, generation);
    });
    assert_historical_grant_untouched(&turn, before, "runtime exit");

    let (turn, _token, generation, _ownership) =
        adopted_with_historical_grant("adopted-missing-runtime");
    let before = forbidden_requests(&turn).len();
    turn.record_mut(|record| record.clear_runtime());
    turn.state
        .fail_active_turn_if_runtime_missing(&turn.session_id, generation, "the runtime is gone")
        .expect("the missing-runtime failure should be handled");
    assert_historical_grant_untouched(&turn, before, "missing-runtime atomic failure");

    let (turn, token, _generation, _ownership) =
        adopted_with_historical_grant("adopted-reader-failure");
    let before = forbidden_requests(&turn).len();
    turn.state
        .fail_turn_if_runtime_matches(&turn.session_id, &token, "the reader failed")
        .expect("the reader failure should be handled");
    assert_historical_grant_untouched(&turn, before, "reader failure");
}

/// The failed-Stop handover that gives revocation teardown its fence, as the
/// host performs it, for the session's current runtime.
fn revocation_target(turn: &EOCheckedTurn) -> EngramMcpRuntimeRevocationTarget {
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

fn revoke(turn: &EOCheckedTurn, context: &str) {
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
fn revocation_teardown_ends_the_adopted_runtime_without_consuming_historical_grant_data() {
    let (turn, _token, generation, _ownership) =
        adopted_with_historical_grant("adopted-revocation");
    let turn_ref = turn.record(|record| {
        record
            .engram
            .unadmitted_active_turn
            .as_ref()
            .unwrap()
            .host_turn_ref
            .clone()
    });
    revoke(&turn, "test revocation of an adopted turn");
    let report = turn.observation_for(&turn_ref);
    assert_eq!(report["occurrence"]["outcome"], "unknown");
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
    assert_historical_grant_untouched(&turn, 0, "revocation teardown");
    turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation);
        assert!(!record.runtime_stop_in_progress);
        assert!(record.runtime.runtime_token().is_none());
        assert_ne!(record.session.status, SessionStatus::Active);
    });
}

#[test]
fn ordinary_deletion_waits_for_its_observed_turn_and_revocation_reports_its_own_execution() {
    for path in ["deletion", "revocation"] {
        let turn = EOCheckedTurn::start(&format!("ordinary-{path}"), true);
        let (turn_ref, token, generation) = turn.record(|record| {
            (
                record
                    .engram
                    .unadmitted_active_turn
                    .as_ref()
                    .unwrap()
                    .host_turn_ref
                    .clone(),
                record.runtime.runtime_token().unwrap(),
                record.active_turn_generation,
            )
        });
        if path == "deletion" {
            let error = turn
                .state
                .kill_session(&turn.session_id)
                .err()
                .expect("captured active facts cannot be deleted before their turn ends");
            assert_eq!(error.status, StatusCode::CONFLICT);
            turn.record(|record| {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.active_turn_generation, generation);
                assert!(record.runtime.matches_runtime_token(&token));
                assert_eq!(
                    record
                        .engram
                        .unadmitted_active_turn
                        .as_ref()
                        .unwrap()
                        .host_turn_ref,
                    turn_ref
                );
            });
            turn.state.stop_session(&turn.session_id).unwrap();
        } else {
            revoke(&turn, "test revocation of an ordinary turn");
        }
        let report = turn.observation_for(&turn_ref);
        assert_eq!(
            report["occurrence"]["outcome"],
            if path == "deletion" {
                "cancelled"
            } else {
                "unknown"
            }
        );
        assert_ordinary_observation_has_no_native_credit(&turn, &report);
        assert!(forbidden_requests(&turn).is_empty());
        if path == "deletion" {
            let guard = phase_sync::PollGuard::new();
            loop {
                let pending = turn.record(runtime_observation_has_removal_duty);
                if !pending {
                    break;
                }
                guard.wait(format_args!(
                    "the exact ordinary report must settle before Delete"
                ));
            }
            turn.state.kill_session(&turn.session_id).unwrap();
            assert!(
                turn.state
                    .inner
                    .lock()
                    .unwrap()
                    .find_session_index(&turn.session_id)
                    .is_none()
            );
        }
    }
}
#[test]
fn the_atomic_failure_of_an_adopted_turn_preserves_historical_grant_without_control_calls_in_any_mode()
 {
    // A matching runtime's confirmed failure, under its terminalization claim.
    let (turn, token, generation, _ownership) =
        adopted_with_historical_grant("adopted-atomic-matching");
    let before = forbidden_requests(&turn).len();
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
    assert_historical_grant_untouched(&turn, before, "atomic MatchingRuntime failure");
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Error));

    // A rejected runtime-command delivery.
    let (turn, token, generation, _ownership) =
        adopted_with_historical_grant("adopted-atomic-rejected");
    let before = forbidden_requests(&turn).len();
    assert!(
        turn.state
            .fail_rejected_turn_delivery(
                &turn.session_id,
                &token,
                generation,
                "the delivery was rejected",
            )
            .expect("the rejected delivery should be recorded"),
        "the rejected delivery terminalizes the adopted turn"
    );
    assert_historical_grant_untouched(&turn, before, "atomic RejectedDelivery failure");
    turn.record(|record| assert_eq!(record.session.status, SessionStatus::Error));
}

#[test]
fn a_stale_adopted_generation_cannot_terminalize_a_successor() {
    let (turn, token, generation, _ownership) = adopted_with_historical_grant("adopted-successor");
    turn.record_mut(|record| {
        record.active_turn_generation = generation + 1;
        assert!(!unmediated_claude_turn_is_current(record));
    });
    assert!(
        turn.state
            .claim_turn_terminalization_if_runtime_matches(&turn.session_id, &token, generation)
            .unwrap()
            .is_none()
    );
    assert!(
        !turn
            .state
            .fail_rejected_turn_delivery(
                &turn.session_id,
                &token,
                generation,
                "a stale predecessor failure"
            )
            .unwrap()
    );
    turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation + 1);
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(record.runtime.matches_runtime_token(&token));
    });
    assert_historical_grant_untouched(&turn, 0, "successor generation");
}
#[test]
fn an_error_session_adopts_a_runtime_started_turn_and_its_result_ends_it() {
    let turn = EOCheckedTurn::start("adopted-from-error", true);
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
fn a_user_stop_of_an_adopted_turn_preserves_historical_grant_without_control_calls() {
    let (turn, _token, _generation, _ownership) = adopted_with_historical_grant("adopted-stop");
    let before = forbidden_requests(&turn).len();
    turn.state
        .stop_session(&turn.session_id)
        .expect("the stop should be accepted");
    turn.record(|record| assert_ne!(record.session.status, SessionStatus::Active));
    assert_historical_grant_untouched(&turn, before, "user Stop");
}

#[test]
fn an_adopted_interval_ended_by_an_unresolved_result_preserves_historical_grant_without_control_calls()
 {
    // An ordinary turn completed and the idle session retains opaque grant
    // history. Every frame then goes through the reader's own application
    // function, which adopts the turn Claude Code starts by itself.
    let turn = EOCheckedTurn::start("adopted-unresolved", true);
    turn.finish();
    turn.record_mut(|record| {
        record
            .engram
            .historical_control
            .insert("engramOpenGrantId".into(), json!(HISTORICAL_GRANT));
    });
    let token = turn.record(|record| record.runtime.runtime_token().expect("runtime"));
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let context = ClaudeRuntimeContext::new(
        turn.state.clone(),
        turn.session_id.clone(),
        token,
        new_claude_turn_ownership(),
        Arc::new(Mutex::new(None)),
        turn.root.to_string_lossy().into_owned(),
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
    let before = forbidden_requests(&turn).len();
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
    assert_historical_grant_untouched(
        &turn,
        before,
        "adopted interval ended by an unresolved result",
    );
}
