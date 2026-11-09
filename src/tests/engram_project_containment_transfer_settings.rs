//! Observation settings do not grant or withdraw ordinary queue permissions.
use super::*;

#[test]
fn observation_off_does_not_turn_an_unknown_handoff_into_a_replay_permission() {
    let (turn, history) = ordinary_with_history("ordinary-off-unknown-handoff");
    turn.enqueue("unknown provider write", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let queued = turn.saved()["queuedPrompts"].clone();
    let mut settings = observation_settings(&turn);
    settings.execution_observability = false;
    turn.state
        .update_project_engram_settings(&turn.project, settings)
        .unwrap();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert_eq!(turn.saved()["queuedPrompts"], queued);
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn observation_off_reload_keeps_ambiguous_receipts_without_reconstructing_a_gate() {
    let (turn, history) = ordinary_with_history("ordinary-off-reload-receipt");
    let (id, _) = fixture_handoff_receipt(&turn, "ambiguous original input");
    let mut settings = observation_settings(&turn);
    settings.execution_observability = false;
    turn.state
        .update_project_engram_settings(&turn.project, settings)
        .unwrap();
    reload_fixture(&turn);
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.record(|record| {
        assert_eq!(
            record.prompt_deliveries[&id].disposition,
            PromptDeliveryDisposition::Ambiguous
        );
        assert!(!record.prompt_deliveries[&id].write_ahead_acknowledged);
        assert!(record.prompt_deliveries[&id].resume.is_none());
    });
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn observation_reenable_never_requires_history_confirmation_before_ordinary_delivery() {
    let (turn, history) = ordinary_with_history("ordinary-observation-reenable");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.enqueue("ordinary queued input", QueuedPromptSource::User);
    for observing in [true, false, true] {
        let mut settings = observation_settings(&turn);
        settings.execution_observability = observing;
        turn.state
            .update_project_engram_settings(&turn.project, settings)
            .unwrap();
        turn.assert_no_prompt();
        assert_history(&turn, &history);
    }
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("ordinary queued input"));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn observation_setting_change_does_not_replace_the_current_ordinary_queue_owner() {
    let (turn, history) = ordinary_with_history("ordinary-setting-owner-cut");
    turn.enqueue("owned ordinary input", QueuedPromptSource::User);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let mut settings = observation_settings(&turn);
    settings.execution_observability = true;
    turn.state
        .update_project_engram_settings(&turn.project, settings)
        .unwrap();
    turn.record(|record| {
        assert!(
            owner.matches(record),
            "an observation toggle is not a new queue/Stop owner"
        )
    });
    let outcome = turn
        .state
        .resume_local_session_queue(&turn.session, Some(owner))
        .unwrap();
    assert!(matches!(
        outcome,
        Some(TurnDispatchDeliveryOutcome::Delivered)
    ));
    assert!(turn.prompt().prompt.contains("owned ordinary input"));
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}
