//! Current queue identity, Stop and write-ahead ownership; no parked gate timer.
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

#[test]
fn ordinary_queue_owner_rejects_changed_payload_head_pause_stop_or_generation() {
    for change in ["payload", "head", "pause", "stop", "generation"] {
        let (turn, history) = ordinary_with_history("ordinary-owner-cut");
        turn.enqueue("original queue head", QueuedPromptSource::User);
        let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
        turn.edit(|record| {
            assert!(owner.matches(record));
            match change {
                "payload" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "head" => {
                    record.queued_prompts.pop_front();
                }
                "pause" => record.set_auto_dispatch_blocked(true),
                "stop" => record.runtime_stop_generation += 1,
                _ => record.active_turn_generation += 1,
            }
            assert!(!owner.matches(record), "{change}");
        });
        let before = turn.saved();
        let result = turn
            .state
            .resume_local_session_queue(&turn.session, Some(owner));
        assert!(!matches!(
            result,
            Ok(Some(TurnDispatchDeliveryOutcome::Delivered))
        ));
        assert_eq!(turn.saved()["queuedPrompts"], before["queuedPrompts"]);
        turn.assert_no_prompt();
        assert_history(&turn, &history);
    }
}

#[test]
fn ordinary_repeated_resume_never_dispatches_a_second_turn_while_busy() {
    let (turn, history) = ordinary_with_history("ordinary-resume-once");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.enqueue("first FIFO", QueuedPromptSource::User);
    let second = turn.enqueue("second FIFO", QueuedPromptSource::User);
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("first FIFO"));
    let queued_before = turn.saved()["queuedPrompts"].clone();
    let generation = turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, second);
        record.active_turn_generation
    });
    for _ in 0..2 {
        let error = turn
            .state
            .resume_session_queue(&turn.session)
            .err()
            .expect("a busy ordinary Resume must refuse");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("busy"), "{error:?}");
        turn.assert_no_prompt();
        assert_eq!(
            turn.saved()["queuedPrompts"],
            queued_before,
            "each busy Resume preserves the exact FIFO head"
        );
        turn.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Active);
            assert_eq!(record.active_turn_generation, generation);
        });
    }
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(
            record
                .queued_prompts
                .iter()
                .any(|entry| entry.pending_prompt.id == second)
        )
    });
    assert_history(&turn, &history);
}

#[test]
fn ordinary_prepared_handoff_waits_for_the_current_stop_owner() {
    let (turn, history) = ordinary_with_history("ordinary-handoff-stop-cut");
    let dispatch = turn.prepare("pending during Stop");
    turn.edit(|record| {
        record.runtime_stop_in_progress = true;
        record.runtime_stop_generation += 1;
    });
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Scheduled
    ));
    turn.record(|record| {
        assert!(record.runtime_stop_in_progress);
        assert_eq!(record.deferred_stop_callbacks.len(), 1);
    });
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_public_resume_coalesces_write_ahead_and_ignores_the_old_ack() {
    let (mut turn, history) = ordinary_with_history("ordinary-resume-wal-owner");
    let writer = DeliveryWriter::attach(&mut turn.state);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("receipted FIFO head", true);
    assert!(
        turn.state
            .request_queued_prompt_durability(&turn.session, &id)
    );
    writer.take(HeldFence::Durability).finish(Ok(()));
    let guard = crate::tests::phase_sync::PollGuard::new();
    while !turn.record(|record| {
        record
            .queued_prompt_durability_holds
            .get(&id)
            .is_some_and(|hold| hold.acknowledged)
    }) {
        guard.wait("ordinary queued entry's durability acknowledgement");
    }
    turn.state.resume_session_queue(&turn.session).unwrap();
    let first_fence = writer.take(HeldFence::WriteAhead);
    let first = turn.record(|record| record.prompt_deliveries[&id].clone());
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.record(|record| {
        assert_eq!(record.prompt_deliveries[&id].write_ahead, first.write_ahead);
        assert_eq!(record.prompt_deliveries[&id].attempt, first.attempt);
    });
    turn.assert_no_prompt();
    first_fence.finish(Err(PersistFenceError::Deadline));
    while !turn.record(|record| {
        record.prompt_deliveries[&id].disposition == PromptDeliveryDisposition::Pending
    }) {
        guard.wait("failed write-ahead returns to pending");
    }
    turn.state.resume_session_queue(&turn.session).unwrap();
    let second_fence = writer.take(HeldFence::WriteAhead);
    let second = turn.record(|record| record.prompt_deliveries[&id].clone());
    assert_ne!(first.write_ahead, second.write_ahead);
    assert_eq!(second.attempt, first.attempt + 1);
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &first.write_ahead, Ok(()));
    turn.assert_no_prompt();
    turn.record(|record| assert!(!record.prompt_deliveries[&id].write_ahead_acknowledged));
    second_fence.finish(Ok(()));
    assert!(turn.prompt().prompt.contains("receipted FIFO head"));
    turn.assert_no_prompt();
    // DeliveryWriter is a shared controlled writer double, not real SQLite
    // proof. The decision-persistence child separately exercises real WAL.
    turn.assert_no_control();
    assert_eq!(
        serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap(),
        history
    );
}

#[test]
fn ordinary_failed_write_ahead_retains_pending_bytes_through_reload() {
    let (turn, history) = ordinary_with_history("ordinary-failed-write-ahead");
    let (id, old_nonce) = fixture_handoff_receipt(&turn, "retained ordinary content");
    turn.state.settle_prompt_delivery_write_ahead(
        &turn.session,
        &id,
        &old_nonce,
        Err(PersistFenceError::WriteFailed(
            "fixture write was not acknowledged".into(),
        )),
    );
    let replacement = turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending);
        assert!(!delivery.write_ahead_acknowledged);
        assert!(delivery.resume.is_none());
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "retained ordinary content"
        );
        delivery.write_ahead.clone()
    });
    assert_ne!(replacement, old_nonce);
    reload_fixture(&turn);
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &old_nonce, Ok(()));
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::Pending
        )
    });
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}
