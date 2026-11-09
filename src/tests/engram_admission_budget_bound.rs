//! Saved admission budgets are passive history beside ordinary delivery.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

fn saved_budget() -> Value {
    json!({"version":1,"admitted_at":"2026-10-20T10:00:00Z",
        "deadline":"2026-10-20T10:04:00Z","attempts":17,"ended":true,
        "original_request":{"operation":"turn_evaluate","idempotency_key":"historical-budget-key"},
        "last_cause":"deadline_exceeded","unknown":[null,{"exact":"źródło 🦀"}]})
}

#[test]
fn historical_budget_keeps_its_original_bytes_and_unknown_outcome_across_real_reload() {
    for last_cause in [
        "control_request_expired",
        "control_store_busy",
        "deadline_exceeded",
        "control_unavailable",
    ] {
        let turn = OrdinaryRoot::new("historical-budget-reload");
        let id = turn.enqueue("unknown original payload", QueuedPromptSource::User);
        let mut budget = saved_budget();
        budget["last_cause"] = json!(last_cause);
        let head = turn.edit(|record| {
            record
                .engram
                .historical_control
                .insert("engramBeginPreparationLineage".into(), budget.clone());
            let queued = &mut record.queued_prompts[0];
            queued.engram_ordinary_handoff = true;
            queued.engram_interrupted = true;
            queued.engram_evaluate =
                Some(json!({"request":budget["original_request"],"outcome":"unknown"}));
            sync_pending_prompts(record);
            serde_json::to_value(&record.queued_prompts[0]).unwrap()
        });
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.state.shutdown_persist_blocking();
        let loaded = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
        assert_eq!(
            record.engram.historical_control["engramBeginPreparationLineage"],
            budget
        );
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            head
        );
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert!(ordinary_queue_position(record).is_none());
        assert!(record.prompt_deliveries.is_empty());
        assert!(loaded.test_run_observations.is_empty());
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn fresh_ordinary_delivery_does_not_consume_or_renew_a_historical_admission_budget() {
    let turn = OrdinaryRoot::new("ordinary-beside-old-budget");
    let budget = saved_budget();
    turn.edit(|record| {
        record
            .engram
            .historical_control
            .insert("engramBeginPreparationLineage".into(), budget.clone());
    });
    let dispatch = turn.prepare("new ordinary work");
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    assert!(turn.prompt().prompt.contains("new ordinary work"));
    turn.record(|record| {
        assert_eq!(
            record.engram.historical_control["engramBeginPreparationLineage"],
            budget
        );
        assert!(record.prompt_deliveries.is_empty());
        assert!(record.queued_prompts.is_empty());
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn human_selection_of_unknown_budget_history_never_renews_it_or_substitutes_a_successor() {
    let turn = OrdinaryRoot::new("selected-old-budget");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue("unknown original payload", QueuedPromptSource::User);
    let budget = saved_budget();
    turn.edit(|record| {
        record
            .engram
            .historical_control
            .insert("engramBeginPreparationLineage".into(), budget);
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].engram_interrupted = true;
        sync_pending_prompts(record);
    });
    let successor = turn.enqueue("independent queued successor", QueuedPromptSource::User);
    let selected = turn.record(|record| HumanPromptSelection {
        prompt: projected_pending_prompts(record)[0].clone(),
        session_mutation_stamp: record.mutation_stamp,
    });
    assert_eq!(selected.prompt.id, id);
    let before = turn.saved();
    let error = match turn
        .state
        .resume_selected_human_prompt(&turn.session, selected)
    {
        Err(error) => error,
        Ok(_) => panic!("unknown historical head must not resume its successor"),
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(turn.saved(), before);
    turn.record(|record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
        assert_eq!(record.queued_prompts[1].pending_prompt.id, successor);
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}
