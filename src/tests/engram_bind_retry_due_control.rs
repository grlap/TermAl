// A saved bind-retry timestamp is passive on either side of its old due
// time. Only the current ordinary queue owner can activate the queued work.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[test]
fn retired_bind_retry_due_time_neither_activates_nor_blocks_an_ordinary_head() {
    for due in ["2000-01-01T00:00:00Z", "9999-01-01T00:00:00Z"] {
        let turn = OrdinaryRoot::new("ordinary-beside-historical-retry");
        let id = turn.enqueue("ordinary due-check successor", QueuedPromptSource::User);
        let history = json!({"dueAt":due,"attempts":7,"phase":"beforeWire",
            "originalKey":"historical-bind-key","unknown":[null,"żółw"]});
        turn.edit(|record| {
            record.engram.historical_control.insert("engramBindRetry".into(), history.clone());
        });
        let owner = turn.record(|record| {
            assert!(record.provider_capacity_retry.is_none(),
                "a historical due time cannot manufacture a live retry owner");
            QueuedTurnOwner::capture(record).unwrap()
        });
        let original = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
        turn.assert_no_prompt();
        turn.assert_no_control();
        turn.record(|record| {
            assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), original);
            assert_eq!(record.engram.historical_control["engramBindRetry"], history);
            assert!(owner.matches(record));
        });

        let started = turn.state.start_next_queued_turn_off_lock_for_owner(
            &turn.session, false, false, Some(QueuedDrainOwner::Activation(owner)),
        ).unwrap().expect("ordinary activation never waits for the removed bind clock");
        assert!(matches!(deliver_turn_dispatch(&turn.state, started.dispatch),
            TurnDispatchDeliveryOutcome::Delivered));
        assert!(turn.prompt().prompt.contains("ordinary due-check successor"));
        turn.assert_no_prompt();
        turn.assert_no_control();
        turn.record(|record| {
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.session.messages.iter().filter(|message| message.id() == id).count(), 1);
            assert_eq!(record.engram.historical_control["engramBindRetry"], history,
                "ordinary handoff preserves the complete raw retry image");
            assert!(record.provider_capacity_retry.is_none());
        });
    }
}
