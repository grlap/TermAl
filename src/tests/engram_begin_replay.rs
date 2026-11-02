//! Ordinary delivery has no Begin replay or control retry owner.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[path = "engram_begin_boot.rs"]
mod boot;

#[test]
fn ordinary_prompt_reaches_the_provider_without_a_control_receipt() {
    let turn = OrdinaryRoot::new("ordinary-without-begin");
    let dispatch = turn.prepare("ordinary content");
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(turn.prompt().prompt.contains("ordinary content"));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_paused_queue_waits_for_explicit_resume_and_delivers_once() {
    let turn = OrdinaryRoot::new("ordinary-paused-queue");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("held ordinary content", QueuedPromptSource::User);
    assert!(turn.state.dispatch_next_queued_turn_with_prompt(&turn.session, false).unwrap().is_none());
    turn.assert_no_prompt();
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, id));
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("held ordinary content"));
    turn.record(|record| assert!(record.queued_prompts.is_empty()));
    assert!(turn.state.resume_local_session_queue(&turn.session, None).is_err(), "an active turn cannot be resumed again");
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_cancel_removes_only_the_selected_queued_prompt() {
    let turn = OrdinaryRoot::new("ordinary-cancel-head");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let old = turn.enqueue("cancelled content", QueuedPromptSource::User);
    let next = turn.enqueue("successor content", QueuedPromptSource::User);
    turn.state.cancel_queued_prompt(&turn.session, &old).unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, next);
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    let prompt = turn.prompt().prompt;
    assert!(prompt.contains("successor content"));
    assert!(!prompt.contains("cancelled content"));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_retry_capacity_is_bounded_and_released_by_its_actual_owner() {
    let slots = RuntimeRetrySlots::default();
    let mut held = (0..4).map(|_| slots.try_acquire().expect("one free runtime retry slot")).collect::<Vec<_>>();
    assert!(slots.try_acquire().is_none());
    drop(held.pop());
    let replacement = slots.try_acquire().expect("dropping one owner releases one slot");
    assert!(slots.try_acquire().is_none());
    drop(replacement);
    drop(held);
    let all = (0..4).map(|_| slots.try_acquire().unwrap()).collect::<Vec<_>>();
    assert!(slots.try_acquire().is_none());
    drop(all);
}

#[test]
fn ordinary_public_stop_pauses_queued_work_until_an_explicit_resume() {
    let turn = OrdinaryRoot::new("ordinary-public-stop");
    deliver_turn_dispatch(&turn.state, turn.prepare("actual turn before Stop")).into_public_result().unwrap();
    assert!(turn.prompt().prompt.contains("actual turn before Stop"));
    let id = turn.enqueue("queued before Stop", QueuedPromptSource::User);
    turn.state.stop_session(&turn.session).unwrap();
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(!record.runtime_stop_in_progress);
        assert_ne!(record.session.status, SessionStatus::Active);
    });
    assert!(turn.state.dispatch_next_queued_turn_with_prompt(&turn.session, false).unwrap().is_none());
    turn.assert_no_prompt();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("queued before Stop"));
    turn.assert_no_prompt();
    turn.assert_no_control();
}
