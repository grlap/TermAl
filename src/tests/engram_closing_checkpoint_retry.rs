//! Unresolved closing checkpoints retain their original authority and request.
//! Admission ticks, Stop, Cancel and replacement grants do not authorize status
//! reconciliation or a new close. Certified original replay has separate tests.
use super::*;

fn prompts(receiver: &mpsc::Receiver<CodexRuntimeCommand>) -> usize {
    receiver.try_iter().filter(|c| matches!(c, CodexRuntimeCommand::Prompt { .. })).count()
}

fn read<T>(fixture: &CausalRoot, look: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = fixture.state.inner.lock().unwrap();
    look(&inner.sessions[inner.find_session_index(&fixture.session).unwrap()])
}

fn operations(fixture: &CausalRoot) -> Vec<String> {
    fixture.transport.order.lock().unwrap().clone()
}

fn unsettled(mode: CausalFaultMode) -> (CausalRoot, Value) {
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    *fixture.transport.mode.lock().unwrap() = mode;
    if matches!(mode, CausalFaultMode::DeadlineAfterApply) {
        *fixture.transport.clock.lock().unwrap() = Some(fixture.state.engram_budget_clock());
    }
    let (token, head, _) = fixture.start_and_queue();
    let _ = fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    fixture.await_causal_publication();
    (fixture, head)
}

fn retained_operation(fixture: &CausalRoot) -> Value {
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.session).unwrap();
    let operation = inner.sessions[index].engram.checkpoint_operation.as_ref()
        .expect("the original close is retained");
    operation.validate().unwrap();
    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::PossiblySent));
    assert!(matches!(operation.attempts.last().unwrap().result,
        Some(EngramCheckpointAttemptResult::Unknown { .. })));
    let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|current| current.original.id == operation.original.id)
        .expect("the independent ledger protects the close");
    assert_eq!(serde_json::to_value(operation).unwrap(), serde_json::to_value(canonical).unwrap());
    assert!(engram_session_obligation_locked(&inner, &fixture.session).is_some());
    serde_json::to_value(operation).unwrap()
}

fn tick_without_bypassing(fixture: &CausalRoot, original: &Value) {
    let sent = operations(fixture);
    fixture.state.engram_budget_clock().advance(Duration::from_secs(3600));
    let at = chrono::Utc::now() + chrono::Duration::hours(1);
    fixture.state.engram_abort_retry_tick(at);
    fixture.state.engram_abort_retry_tick(at);
    assert_eq!(operations(fixture), sent, "no status, rebind, checkpoint or Evaluate bypass");
    assert_eq!(prompts(&fixture.receiver), 0, "no provider handoff");
    assert_eq!(retained_operation(fixture), *original, "the exact original duty survives");
    assert!(read(fixture, |record| record.engram.admission_retry.is_none()));
}

fn assert_head(fixture: &CausalRoot, head: &Value) {
    assert_eq!(read(fixture, |record|
        serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap()), *head);
}

#[test]
fn a_closing_deadline_retains_the_original_duty_without_automatic_admission() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, head) = unsettled(CausalFaultMode::DeadlineAfterApply);
    let original = retained_operation(&fixture);
    assert_eq!(original["original"]["request"],
        fixture.transport.lost_request.lock().unwrap().clone().unwrap());
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 1);
    assert_head(&fixture, &head);
    let cards = fixture.durable_cards();
    let closing = cards.iter().find(|card|
        card["stage"] == "checkpoint" && card["decision"] == "degraded")
        .expect("the failed close has its durable causal card");
    assert_eq!(closing["causalFailure"]["operation"], "turn_checkpoint");
    assert_eq!(closing["causalFailure"]["originalCode"], "deadline_exceeded");
    assert_eq!(closing["causalFailure"]["failureClass"], "deadline");
    tick_without_bypassing(&fixture, &original);
}

#[test]
fn a_lost_closing_reply_cannot_be_replaced_by_status_and_fresh_admission() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, head) = unsettled(CausalFaultMode::LostReply);
    let original = retained_operation(&fixture);
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 1,
        "producer application is not a local receipt");
    assert_eq!(original["original"]["request"],
        fixture.transport.lost_request.lock().unwrap().clone().unwrap());
    tick_without_bypassing(&fixture, &original);
    assert_head(&fixture, &head);
}

#[test]
fn a_never_started_close_keeps_its_original_request_until_supported_recovery() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, _) = unsettled(CausalFaultMode::NeverStarted);
    let original = retained_operation(&fixture);
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 0);
    assert_eq!(original["original"]["request"],
        fixture.transport.lost_request.lock().unwrap().clone().unwrap());
    tick_without_bypassing(&fixture, &original);
    assert_eq!(fixture.transport.application_count("turn_checkpoint"), 0,
        "a new recovery request cannot replace the retained original");
}

#[test]
fn an_unresolved_close_never_reaches_a_different_recovery_receipt_or_refusal() {
    for mode in [CausalFaultMode::RecoveryRefusal, CausalFaultMode::RecoveryReceiptSecret] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, head) = unsettled(CausalFaultMode::LostReply);
        let original = retained_operation(&fixture);
        *fixture.transport.fault.lock().unwrap() = Some(("turn_checkpoint", 2));
        *fixture.transport.mode.lock().unwrap() = mode;
        tick_without_bypassing(&fixture, &original);
        assert_eq!(fixture.transport.attempts.lock().unwrap()["turn_checkpoint"], 1,
            "neither alternate recovery outcome was requested");
        assert_head(&fixture, &head);
    }
}

#[test]
fn stop_refusal_and_cancel_do_not_erase_an_unsettled_checkpoint() {
    for stop in [true, false] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, head) = unsettled(CausalFaultMode::LostReply);
        let original = retained_operation(&fixture);
        if stop {
            assert!(!read(&fixture, |record| matches!(record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)));
            let error = fixture.state.request_stop_session(&fixture.session)
                .err().expect("the completed turn has no running provider or scheduled retry to stop");
            assert_eq!(error.status, StatusCode::CONFLICT);
            assert_eq!(error.message, "session is not currently running");
        } else {
            fixture.state.cancel_queued_prompt(&fixture.session, head["id"].as_str().unwrap()).unwrap();
        }
        tick_without_bypassing(&fixture, &original);
    }
}

#[test]
fn an_unrelated_active_grant_cannot_replace_the_original_checkpoint() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, _) = unsettled(CausalFaultMode::LostReply);
    let original = retained_operation(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.active_grant_id =
            Some("unrelated-active-grant".to_owned());
    }
    tick_without_bypassing(&fixture, &original);
    assert_eq!(read(&fixture, |record| record.engram.active_grant_id.clone()).as_deref(),
        Some("unrelated-active-grant"), "historical authority cannot close the replacement");
}
