//! Explicit ordinary Resume retains the exact unsent prompt and receipt owner.
//! DeliveryWriter controls acknowledgements only; it writes no SQLite rows.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

fn wait_for_content_ack(turn: &OrdinaryRoot, id: &str) {
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| record.queued_prompt_durability_holds[id].acknowledged) {
        guard.wait("the exact ordinary content acknowledgement");
    }
}

#[test]
fn ordinary_resume_after_failed_write_ahead_requires_a_fresh_ack_and_sends_once() {
    let mut turn = OrdinaryRoot::new("ordinary-resume-fresh-wal");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("original unsent Resume content", true);
    let original = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    writer.take(HeldFence::Durability).finish(Ok(()));
    wait_for_content_ack(&turn, &id);
    assert!(matches!(
        turn.state
            .resume_local_session_queue(&turn.session, None)
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    let first = writer.take(HeldFence::WriteAhead);
    let failed_nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    let generation = turn.record(|record| record.active_turn_generation);
    first.finish(Err(PersistFenceError::WorkerStopped));
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::Pending
    }) {
        guard.wait("the actual failed write-ahead returns this prompt to Pending");
    }
    turn.record(|record| {
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            original
        );
        let delivery = &record.prompt_deliveries[&id];
        assert_ne!(delivery.write_ahead, failed_nonce);
        assert!(!delivery.write_ahead_acknowledged && delivery.turn_generation.is_none());
        assert!(
            delivery
                .reason
                .as_deref()
                .unwrap()
                .contains("WorkerStopped")
        );
        assert_eq!(record.active_turn_generation, generation);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    assert!(matches!(
        turn.state
            .resume_local_session_queue(&turn.session, None)
            .unwrap(),
        Some(TurnDispatchDeliveryOutcome::Scheduled)
    ));
    let fresh = writer.take(HeldFence::WriteAhead);
    let fresh_nonce = turn.record(|record| record.prompt_deliveries[&id].write_ahead.clone());
    assert_ne!(fresh_nonce, failed_nonce);
    // Apply a late old success synchronously: it cannot settle the new attempt.
    let before = turn.saved();
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &failed_nonce, Ok(()));
    assert_eq!(turn.saved(), before);
    turn.assert_no_prompt();
    fresh.finish(Ok(()));
    let command = turn.prompt();
    assert!(command.prompt.contains("original unsent Resume content"));
    assert!(command.delivery_receipt.is_some());
    turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.write_ahead, fresh_nonce);
        assert_eq!(
            delivery.turn_generation,
            Some(command.active_turn_generation)
        );
        assert!(command.active_turn_generation > generation);
    });
    let handed_off = turn.saved();
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &fresh_nonce, Ok(()));
    assert_eq!(turn.saved(), handed_off);
    turn.assert_no_prompt();
    turn.assert_no_control();
    drop(writer);
}

#[test]
fn ordinary_resume_refuses_busy_stopping_or_unresolved_runtime_without_consuming_the_head() {
    for fault in [
        "active",
        "approval",
        "stopping",
        "stop_claim",
        "runtime_identity",
    ] {
        let turn = OrdinaryRoot::new("ordinary-resume-local-refusal");
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        let id = turn.enqueue("held ordinary user input", QueuedPromptSource::User);
        turn.edit(|record| match fault {
            "active" => record.session.status = SessionStatus::Active,
            "approval" => record.session.status = SessionStatus::Approval,
            "stopping" => record.session.status = SessionStatus::Stopping,
            "stop_claim" => record.runtime_stop_in_progress = true,
            "runtime_identity" => record.codex_terminal_identity_unresolved = true,
            _ => unreachable!(),
        });
        let before = turn.saved();
        let error = turn
            .state
            .resume_local_session_queue(&turn.session, None)
            .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT, "{fault}");
        assert!(
            error.message.contains(if fault == "runtime_identity" {
                "runtime identity is unavailable"
            } else {
                "busy, resetting or stopping"
            }),
            "{fault}: {}",
            error.message
        );
        assert_eq!(turn.saved(), before, "{fault}: no head or pause mutation");
        turn.record(|record| {
            assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
            assert!(record.orchestrator_auto_dispatch_blocked);
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_resume_owner_cannot_cross_content_pause_stop_turn_or_head_replacement() {
    for change in ["content", "pause", "stop", "turn", "replacement"] {
        let turn = OrdinaryRoot::new("ordinary-resume-exact-owner");
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        let first = turn.enqueue("original selected input", QueuedPromptSource::User);
        let next = turn.enqueue("independent successor", QueuedPromptSource::User);
        let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
        if change == "replacement" {
            turn.state
                .cancel_queued_prompt(&turn.session, &first)
                .unwrap();
        } else {
            turn.edit(|record| match change {
                "content" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "pause" => record.set_auto_dispatch_blocked(true),
                "stop" => record.runtime_stop_generation += 1,
                "turn" => record.active_turn_generation += 1,
                _ => unreachable!(),
            });
        }
        let before = turn.saved();
        let result = turn
            .state
            .resume_local_session_queue(&turn.session, Some(owner))
            .unwrap();
        assert!(matches!(
            result,
            None | Some(TurnDispatchDeliveryOutcome::Superseded)
        ));
        assert_eq!(
            turn.saved(),
            before,
            "{change}: stale Resume cannot release a new owner"
        );
        turn.record(|record| {
            assert!(record.orchestrator_auto_dispatch_blocked);
            let expected = if change == "replacement" {
                &next
            } else {
                &first
            };
            assert_eq!(&record.queued_prompts[0].pending_prompt.id, expected);
        });
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}
