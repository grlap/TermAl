//! Old refusal/healing records are passive data beside ordinary work.
//! There is no grant-healing, rebind loop or admission budget to execute.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[test]
fn historical_healing_refusal_is_preserved_without_rebinding_an_ordinary_turn() {
    let turn = OrdinaryRoot::new("ordinary-historical-healing");
    let refusal = json!({"decision":"refuse","code":"grant_scope_mismatch",
        "directive":{"code":"stale_fence","satisfaction":"rebind"},
        "routing_token":"historical-token","future":[null,"żółw"]});
    turn.edit(|record| {
        record.engram.historical_control.insert("engramRefusal".into(), refusal.clone());
    });
    deliver_turn_dispatch(&turn.state, turn.prepare("Independent ordinary work.")).unwrap();
    assert!(turn.prompt().prompt.contains("Independent ordinary work."));
    turn.assert_no_prompt();
    turn.record(|record| assert_eq!(record.engram.historical_control["engramRefusal"], refusal));
    turn.assert_no_control();
}

#[test]
fn repeated_historical_stale_fence_never_overrides_an_ordinary_pause() {
    let turn = OrdinaryRoot::new("ordinary-stale-fence-pause");
    let history = json!({"code":"stale_fence","attempts":2,"rebind":"unresolved"});
    turn.edit(|record| {
        record.set_auto_dispatch_blocked(true);
        record.engram.historical_control.insert("engramAdmissionRetry".into(), history.clone());
    });
    let id = turn.enqueue("Explicitly paused work.", QueuedPromptSource::User);
    let before = turn.saved();
    for _ in 0..3 {
        assert!(turn.state.dispatch_next_queued_turn(&turn.session, false).unwrap().is_none());
    }
    assert_eq!(turn.saved(), before);
    turn.assert_no_prompt();
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert!(turn.prompt().prompt.contains("Explicitly paused work."));
    turn.assert_no_prompt();
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(record.session.messages.iter().filter(|message| message.id() == id).count(), 1);
        assert_eq!(record.engram.historical_control["engramAdmissionRetry"], history);
    });
    turn.assert_no_control();
}

#[test]
fn elapsed_historical_admission_budget_cannot_withhold_an_ordinary_turn() {
    let turn = OrdinaryRoot::new("ordinary-no-healing-budget");
    let history = json!({"deadline":"2000-01-01T00:00:00Z","ended":true,
        "code":"dispatch_budget_exhausted"});
    turn.edit(|record| {
        record.engram.historical_control.insert("engramBeginPreparationLineage".into(), history.clone());
    });
    turn.state.engram_budget_clock().advance(Duration::from_secs(3_600));
    deliver_turn_dispatch(&turn.state, turn.prepare("No deleted admission window.")).unwrap();
    assert!(turn.prompt().prompt.contains("No deleted admission window."));
    turn.assert_no_prompt();
    turn.record(|record| assert_eq!(record.engram.historical_control["engramBeginPreparationLineage"], history));
    turn.assert_no_control();
}
