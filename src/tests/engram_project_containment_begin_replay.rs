//! Ordinary ambiguity, cancellation and exact owner tuples; no Begin replay.
use super::*;

#[test]
fn ordinary_ambiguous_receipt_stays_unreplayable_across_reload_and_settings() {
    let (turn, history) = ordinary_with_history("ordinary-ambiguous-receipt");
    let (id, _) = fixture_handoff_receipt(&turn, "possibly written content");
    reload_fixture(&turn);
    for observing in [false, true] {
        let mut settings = observation_settings(&turn);
        settings.execution_observability = observing;
        turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
        turn.state.resume_session_queue(&turn.session).unwrap();
        turn.assert_no_prompt();
        turn.record(|record| assert_eq!(record.prompt_deliveries[&id].disposition, PromptDeliveryDisposition::Ambiguous));
        assert_history(&turn, &history);
    }
}

#[test]
fn ordinary_public_cancel_of_unknown_head_allows_only_the_fifo_successor() {
    let (turn, history) = ordinary_with_history("ordinary-cancel-unknown");
    let first = turn.enqueue("unknown original", QueuedPromptSource::User);
    let second = turn.enqueue("explicit successor", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    turn.assert_no_prompt();
    turn.state.cancel_queued_prompt(&turn.session, &first).unwrap();
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, second));
    turn.state.resume_session_queue(&turn.session).unwrap();
    let command = turn.prompt();
    assert!(command.prompt.contains("explicit successor"));
    assert!(!command.prompt.contains("unknown original"));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_ack_for_a_removed_head_cannot_consume_the_reloaded_successor() {
    let (turn, history) = ordinary_with_history("ordinary-missing-head-reload");
    let (id, nonce) = fixture_handoff_receipt(&turn, "removed head");
    let next = turn.enqueue("new queue head", QueuedPromptSource::User);
    turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
    reload_fixture(&turn);
    let before = turn.saved()["queuedPrompts"].clone();
    turn.state.settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
    assert_eq!(turn.saved()["queuedPrompts"], before);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, next));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_owner_accepts_only_its_exact_turn_and_stop_generation_tuple() {
    let (turn, history) = ordinary_with_history("ordinary-owner-tuples");
    turn.enqueue("owned tuple", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    turn.edit(|record| {
        let turn_generation = record.active_turn_generation;
        let stop_generation = record.runtime_stop_generation;
        for (turn_delta, stop_delta) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            record.active_turn_generation = turn_generation + turn_delta;
            record.runtime_stop_generation = stop_generation + stop_delta;
            assert_eq!(owner.matches(record), turn_delta == 0 && stop_delta == 0);
        }
        record.active_turn_generation = turn_generation;
        record.runtime_stop_generation = stop_generation;
    });
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_accepted_or_ambiguous_receipt_is_never_a_resume_permission() {
    for disposition in [PromptDeliveryDisposition::Accepted, PromptDeliveryDisposition::Ambiguous] {
      for reload in [false, true] {
        let (turn, history) = ordinary_with_history("ordinary-settled-receipt");
        let (id, nonce) = fixture_handoff_receipt(&turn, "settled original content");
        turn.edit(|record| record.prompt_deliveries.get_mut(&id).unwrap().disposition = disposition);
        if reload { reload_fixture(&turn); }
        turn.state.settle_prompt_delivery_write_ahead(&turn.session, &id, &nonce, Ok(()));
        turn.state.resume_session_queue(&turn.session).unwrap();
        turn.assert_no_prompt();
        turn.record(|record| assert_eq!(record.prompt_deliveries[&id].disposition, disposition));
        assert_history(&turn, &history);
      }
    }
}

#[test]
fn ordinary_stale_dispatch_cannot_deliver_another_queue_heads_content() {
    let (turn, history) = ordinary_with_history("ordinary-dispatch-owner-cut");
    turn.enqueue("original queued content", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let first = owner.prompt_id.clone();
    let second = turn.enqueue("successor queued content", QueuedPromptSource::User);
    turn.state.cancel_queued_prompt(&turn.session, &first).unwrap();
    let before = turn.saved();
    let result = turn.state.resume_local_session_queue(&turn.session, Some(owner));
    assert!(!matches!(result, Ok(Some(TurnDispatchDeliveryOutcome::Delivered))));
    assert_eq!(turn.saved()["queuedPrompts"], before["queuedPrompts"]);
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, second));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}
