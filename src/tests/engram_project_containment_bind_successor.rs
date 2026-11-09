//! Exact ordinary owner and receipt identity across delayed acknowledgements.
use super::*;

#[test]
fn ordinary_successor_nonce_invalidates_the_captured_queue_owner() {
    let (turn, history) = ordinary_with_history("ordinary-nonce-successor");
    let (id, _) = fixture_handoff_receipt(&turn, "exact original payload");
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    turn.edit(|record| {
        assert!(owner.matches(record));
        record.prompt_deliveries.get_mut(&id).unwrap().write_ahead = "new-attempt-nonce".into();
        assert!(!owner.matches(record));
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

#[test]
fn ordinary_reload_never_restores_write_ahead_acknowledgement_or_resume_permission() {
    let (turn, history) = ordinary_with_history("ordinary-receipt-reload");
    let (id, _) = fixture_handoff_receipt(&turn, "unknown handoff original");
    turn.edit(|record| {
        record
            .prompt_deliveries
            .get_mut(&id)
            .unwrap()
            .write_ahead_acknowledged = true
    });
    reload_fixture(&turn);
    turn.record(|record| {
        let receipt = &record.prompt_deliveries[&id];
        assert_eq!(receipt.disposition, PromptDeliveryDisposition::Ambiguous);
        assert!(!receipt.write_ahead_acknowledged);
        assert!(receipt.resume.is_none());
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_old_write_ahead_ack_cannot_settle_a_newer_receipt_attempt() {
    let (turn, history) = ordinary_with_history("ordinary-old-receipt-ack");
    let (id, old_nonce) = fixture_handoff_receipt(&turn, "first attempt content");
    turn.state.settle_prompt_delivery_write_ahead(
        &turn.session,
        &id,
        &old_nonce,
        Err(PersistFenceError::Deadline),
    );
    turn.edit(|record| {
        let receipt = record.prompt_deliveries.get_mut(&id).unwrap();
        assert_ne!(receipt.write_ahead, old_nonce);
        receipt.disposition = PromptDeliveryDisposition::HandingOff;
        receipt.attempt += 1;
    });
    let before = turn.saved();
    turn.state
        .settle_prompt_delivery_write_ahead(&turn.session, &id, &old_nonce, Ok(()));
    assert_eq!(turn.saved(), before);
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_current_ack_cannot_deliver_past_pause_stop_or_a_different_head() {
    for change in ["pause", "stop", "head"] {
        let (turn, history) = ordinary_with_history("ordinary-ack-owner-drift");
        let (id, nonce) = fixture_handoff_receipt(&turn, "never reassign this input");
        let successor = turn.enqueue("successor input", QueuedPromptSource::User);
        turn.edit(|record| match change {
            "pause" => record.set_auto_dispatch_blocked(true),
            "stop" => {
                record.runtime_stop_in_progress = true;
                record.runtime_stop_generation += 1;
            }
            _ => {
                record.queued_prompts.pop_front();
                record.set_auto_dispatch_blocked(true);
            }
        });
        turn.state
            .settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
        turn.assert_no_prompt();
        turn.record(|record| {
            assert!(
                record
                    .queued_prompts
                    .iter()
                    .any(|entry| entry.pending_prompt.id == successor)
            )
        });
        assert_history(&turn, &history);
    }
}
