//! Provider-capacity Resume owns the exact ordinary head through WAL and cancellation.
//! The held writer is an acknowledgement double; no control evaluation window exists.
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

fn capacity_turn() -> (OrdinaryRoot, String, u64) {
    let mut turn = OrdinaryRoot::new("ordinary-capacity-ownership");
    let (runtime, receiver, process) = test_shared_codex_runtime("ordinary-capacity");
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    turn.receiver = receiver;
    let generation = turn.edit(|record| {
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: turn.session.clone(),
            }),
        });
        record.active_turn_generation
    });
    let nonce = turn
        .state
        .prepare_provider_capacity_failure(
            &turn.session,
            &token,
            Some(generation),
            true,
            &json!({"codexErrorInfo":"serverOverloaded"}),
            false,
        )
        .unwrap();
    turn.state
        .fail_turn_if_runtime_matches(&turn.session, &token, "capacity")
        .unwrap();
    turn.state
        .finish_provider_capacity_failure(&turn.session, &nonce)
        .unwrap();
    // The scheduling seam after the tick charged its attempt and captured an
    // empty or exact head. This does not claim a wall-clock retry was executed.
    turn.edit(|record| {
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.recovering = true;
        retry.dispatched = true;
        retry.attempts = 1;
        retry.due_at = None;
        record.session.status = SessionStatus::Error;
        record.session.provider_capacity_waiting = false;
    });
    (turn, nonce, generation)
}

#[test]
fn ordinary_empty_capacity_capture_selects_a_late_plain_head_but_cancel_revokes_nonce() {
    for cancel in [false, true] {
        let (turn, nonce, generation) = capacity_turn();
        assert!(turn.record(QueuedTurnOwner::capture).is_none());
        let head = turn.enqueue("late capacity head", QueuedPromptSource::User);
        let tail = turn.enqueue("late capacity tail", QueuedPromptSource::User);
        if cancel {
            turn.state
                .cancel_queued_prompt(&turn.session, &tail)
                .unwrap();
        }
        let result = turn
            .state
            .resume_local_session_queue_inner(&turn.session, None, Some(&nonce))
            .unwrap();
        if cancel {
            assert!(matches!(
                result,
                Some(TurnDispatchDeliveryOutcome::Superseded)
            ));
            turn.record(|record| {
                assert_eq!(record.active_turn_generation, generation);
                assert_eq!(
                    record.queued_prompts.front().unwrap().pending_prompt.id,
                    head
                );
                assert!(record.provider_capacity_retry.is_none());
            });
        } else {
            assert!(matches!(
                result,
                Some(TurnDispatchDeliveryOutcome::Delivered)
            ));
            assert!(turn.prompt().prompt.contains("late capacity head"));
            turn.record(|record| {
                assert_eq!(
                    record.active_turn_generation,
                    generation.wrapping_add(1).max(1)
                );
                assert_eq!(
                    record.queued_prompts.front().unwrap().pending_prompt.id,
                    tail
                );
            });
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_late_receipted_capacity_head_waits_for_its_exact_write_ahead() {
    for cancel in [false, true] {
        let (mut turn, nonce, _) = capacity_turn();
        let head = turn.enqueue("late receipted head", QueuedPromptSource::User);
        let tail = turn.enqueue("late tail", QueuedPromptSource::User);
        super::receipted_admission::mark_pending_receipt(&turn, &head);
        let writer = DeliveryWriter::attach(&mut turn.state);
        assert!(matches!(
            turn.state
                .resume_local_session_queue_inner(&turn.session, None, Some(&nonce))
                .unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        let fence = writer.take(HeldFence::WriteAhead);
        let write_ahead = turn.record(|record| record.prompt_deliveries[&head].write_ahead.clone());
        turn.assert_no_prompt();
        if cancel {
            turn.state
                .cancel_queued_prompt(&turn.session, &tail)
                .unwrap();
        }
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &head, &write_ahead, Ok(()));
        if cancel {
            turn.record(|record| {
                assert!(record.provider_capacity_retry.is_none());
                assert_eq!(
                    record.queued_prompts.front().unwrap().pending_prompt.id,
                    head
                );
                assert_eq!(
                    record.prompt_deliveries[&head].disposition,
                    PromptDeliveryDisposition::Pending
                );
            });
        } else {
            assert!(turn.prompt().prompt.contains("late receipted head"));
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
        drop(fence);
    }
}

#[test]
fn ordinary_capacity_tick_preserves_owned_wal_and_respects_settings_pause_stop_and_payload() {
    for change in [
        "none",
        "settings",
        "tail_cancel",
        "pause",
        "operator",
        "head_content",
        "stop",
    ] {
        let (mut turn, nonce, generation) = capacity_turn();
        let head = turn.enqueue("capacity head", QueuedPromptSource::User);
        let tail = turn.enqueue("capacity tail", QueuedPromptSource::User);
        super::receipted_admission::mark_pending_receipt(&turn, &head);
        let mut owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
        owner.capacity_nonce = Some(nonce.clone());
        let writer = DeliveryWriter::attach(&mut turn.state);
        assert!(matches!(
            turn.state
                .resume_local_session_queue_inner(&turn.session, Some(owner), Some(&nonce))
                .unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        let fence = writer.take(HeldFence::WriteAhead);
        let write_ahead = turn.record(|record| record.prompt_deliveries[&head].write_ahead.clone());
        match change {
            "tail_cancel" => {
                turn.state
                    .cancel_queued_prompt(&turn.session, &tail)
                    .unwrap();
            }
            "settings" => turn.edit(|record| record.session.model.push_str("-changed")),
            "pause" => turn.edit(|record| record.set_auto_dispatch_blocked(true)),
            "operator" => turn.edit(|record| record.engram.operator_paused = true),
            "head_content" => turn.edit(|record| {
                record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" edited")
            }),
            "stop" => turn.edit(|record| record.runtime_stop_generation += 1),
            _ => {}
        }
        turn.state.provider_capacity_retry_tick(chrono::Utc::now());
        turn.state.provider_capacity_retry_tick(chrono::Utc::now());
        if change == "none" {
            turn.record(|record| {
                assert_eq!(
                    record.provider_capacity_retry.as_ref().unwrap().nonce,
                    nonce
                )
            });
        }
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &head, &write_ahead, Ok(()));
        if change == "none" {
            assert!(turn.prompt().prompt.contains("capacity head"));
            turn.record(|record| {
                assert_eq!(
                    record.active_turn_generation,
                    generation.wrapping_add(1).max(1)
                )
            });
            turn.state.provider_capacity_retry_tick(chrono::Utc::now());
        } else {
            turn.record(|record| {
                assert_eq!(record.active_turn_generation, generation);
                assert_eq!(
                    record.prompt_deliveries[&head].disposition,
                    PromptDeliveryDisposition::Pending
                );
                assert_eq!(
                    record.queued_prompts.front().unwrap().pending_prompt.id,
                    head
                );
            });
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
        drop(fence);
    }
}

#[test]
fn ordinary_tail_cancel_invalidates_the_captured_capacity_owner_before_resume() {
    let (turn, nonce, generation) = capacity_turn();
    let head = turn.enqueue("exact capacity head", QueuedPromptSource::User);
    let tail = turn.enqueue("cancelled tail", QueuedPromptSource::User);
    let mut owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    owner.capacity_nonce = Some(nonce.clone());
    turn.state
        .cancel_queued_prompt(&turn.session, &tail)
        .unwrap();
    assert!(!turn.record(|record| owner.matches(record)));
    assert!(matches!(
        turn.state
            .resume_local_session_queue_inner(&turn.session, Some(owner), Some(&nonce))
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Superseded)
    ));
    turn.record(|record| {
        assert_eq!(record.active_turn_generation, generation);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            head
        );
        assert!(
            record.orchestrator_auto_dispatch_blocked && record.provider_capacity_retry.is_none()
        );
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}
