//! Unknown historical checkpoint evidence is data, never a new closing RPC.
use super::*;
use crate::tests::engram_host_adapter::turn_checks::checkpoint_result_recovery::{
    install_historical_checkpoint, passive_checkpoint,
};
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

fn retain_unknown_close(turn: &OrdinaryRoot, detail: &str) -> (String, Value, Value) {
    let id = turn.enqueue(
        "Exact historical unknown continuation",
        QueuedPromptSource::User,
    );
    let head = turn.edit(|record| {
        let queued = &mut record.queued_prompts[0];
        queued.engram_ordinary_handoff = true;
        queued.engram_interrupted = true;
        queued.engram_evaluate = Some(json!({"request":{"operation":"turn_evaluate",
            "idempotency_key":"historical-unknown-key"}, "outcome":"unknown", "detail":detail}));
        sync_pending_prompts(record);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    let operation = passive_checkpoint(&turn.root, &turn.session);
    let checkpoint = serde_json::to_value(&operation).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        install_historical_checkpoint(&mut inner, &turn.session, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    (id, head, checkpoint)
}

fn assert_close_unchanged(turn: &OrdinaryRoot, head: &Value, checkpoint: &Value) {
    let (queued, operation, native) = {
        let inner = turn.state.inner.lock().unwrap();
        (
            serde_json::to_value(
                &inner.sessions[inner.find_session_index(&turn.session).unwrap()].queued_prompts[0],
            )
            .unwrap(),
            serde_json::to_value(&inner.engram_session_obligations.checkpoint_operations[0])
                .unwrap(),
            inner.test_run_observations.is_empty(),
        )
    };
    assert_eq!(&queued, head);
    assert_eq!(&operation, checkpoint);
    assert!(
        native,
        "historical evidence creates no native check observation"
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn an_unknown_close_remains_exact_data_without_an_application_claim() {
    for detail in [
        "transport_never_started",
        "reply_lost_after_possible_application",
    ] {
        let turn = OrdinaryRoot::new("unknown-close-data");
        let (_, head, checkpoint) = retain_unknown_close(&turn, detail);
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.state.dispatch_orphaned_workflow_prompts();
        assert_close_unchanged(&turn, &head, &checkpoint);
        assert_eq!(checkpoint["attempts"][0]["result"]["result"], "unknown");
        // These are stored specimens, not a live independent producer witness.
        assert!(checkpoint["protection"]["settlementEvidence"].is_null());
    }
}

#[test]
fn unknown_prior_content_is_not_replayed_by_ordinary_drains() {
    let turn = OrdinaryRoot::new("unknown-ordinary-drain");
    let (_, head, checkpoint) = retain_unknown_close(&turn, "unknown_prior_send");
    for _ in 0..2 {
        turn.state.resume_session_queue(&turn.session).unwrap();
    }
    assert_close_unchanged(&turn, &head, &checkpoint);
    turn.record(|record| assert!(ordinary_queue_position(record).is_none()));
}

#[test]
fn ordinary_drains_preserve_extra_historical_grant_uncertainty() {
    let turn = OrdinaryRoot::new("extra-uncertain-history");
    let (_, head, checkpoint) = retain_unknown_close(&turn, "unknown");
    let grant = json!("historical-uncertain-grant");
    turn.edit(|record| {
        record
            .engram
            .historical_control
            .insert("engramUncertainGrantId".into(), grant.clone());
    });
    turn.state.dispatch_orphaned_workflow_prompts();
    assert_close_unchanged(&turn, &head, &checkpoint);
    turn.record(|record| {
        assert_eq!(
            record.engram.historical_control["engramUncertainGrantId"],
            grant
        )
    });
}

#[test]
fn cancel_then_fresh_ordinary_work_preserves_independent_unknown_checkpoint() {
    let turn = OrdinaryRoot::new("fresh-beside-unknown-close");
    let (id, _, checkpoint) = retain_unknown_close(&turn, "original_close_unknown");
    turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
    let dispatch = turn.prepare("Fresh work; do not replay the old prompt.");
    let token = dispatch.runtime_token().clone();
    deliver_turn_dispatch(&turn.state, dispatch)
        .into_public_result()
        .unwrap();
    let prompt = turn.prompt();
    assert!(
        prompt
            .prompt
            .contains("Fresh work; do not replay the old prompt.")
    );
    assert!(
        !prompt
            .prompt
            .contains("Exact historical unknown continuation")
    );
    turn.state
        .finish_turn_ok_if_runtime_matches(&turn.session, &token)
        .unwrap();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0]).unwrap(),
        checkpoint
    );
    assert!(
        loaded.sessions[loaded.find_session_index(&turn.session).unwrap()]
            .queued_prompts
            .is_empty()
    );
    assert!(loaded.test_run_observations.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}
