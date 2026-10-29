//! A transient transport failure is not a producer certificate about the
//! original checkpoint. The exact duty stays separate from the successor's
//! admission: returning control, timer ticks and additional uncertainty cannot
//! authorize status/rebind recovery or consume the queued prompt.
use super::*;

fn read<T>(fixture: &CausalRoot, look: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = fixture.state.inner.lock().unwrap();
    look(&inner.sessions[inner.find_session_index(&fixture.session).unwrap()])
}

fn closed_with_control_unavailable(mode: CausalFaultMode) -> (CausalRoot, Value) {
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 1)));
    *fixture.transport.mode.lock().unwrap() = mode;
    let (token, head, _) = fixture.start_and_queue();
    let _ = fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    fixture.await_causal_publication();
    assert!(fixture.durable_cards().iter().any(|card|
        card["causalFailure"]["operation"] == "turn_checkpoint"
            && card["causalFailure"]["originalCode"] == "control_unavailable"),
        "the real closing request failed with control_unavailable");
    assert!(fixture.receiver.try_recv().is_err(), "the continuation is withheld");
    (fixture, head)
}

fn exact_checkpoint(fixture: &CausalRoot) -> Value {
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    let operation = record.engram.checkpoint_operation.as_ref().expect("original checkpoint");
    assert!(matches!(operation.attempts.last().unwrap().result,
        Some(EngramCheckpointAttemptResult::Unknown { .. })));
    let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|current| current.original.id == operation.original.id).expect("independent duty");
    let original = serde_json::to_value(operation).unwrap();
    assert_eq!(original, serde_json::to_value(canonical).unwrap());
    assert_eq!(original["original"]["request"],
        fixture.transport.lost_request.lock().unwrap().clone().unwrap());
    assert!(engram_session_obligation_locked(&inner, &fixture.session).is_some());
    original
}

fn tick_keeps_the_duty(fixture: &CausalRoot, head: &Value, checkpoint: &Value) {
    let before = fixture.transport.attempts.lock().unwrap().clone();
    fixture.state.engram_budget_clock().advance(Duration::from_secs(3600));
    let at = chrono::Utc::now() + chrono::Duration::hours(1);
    fixture.state.engram_abort_retry_tick(at);
    fixture.state.engram_abort_retry_tick(at);
    assert_eq!(*fixture.transport.attempts.lock().unwrap(), before,
        "the protected close cannot turn into status, bind, checkpoint or Evaluate");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    assert_eq!(exact_checkpoint(fixture), *checkpoint);
    let (retry, queued) = read(fixture, |record| (
        record.engram.admission_retry.clone(),
        serde_json::to_value(&record.queued_prompts.front().unwrap().pending_prompt).unwrap(),
    ));
    assert!(retry.is_none(), "no admission retry is scheduled behind independent authority");
    assert_eq!(queued, *head, "the successor remains queued");
}

#[test]
fn a_control_unavailable_close_retains_its_request_when_control_returns() {
    for (mode, applied) in [
        (CausalFaultMode::NeverStarted, 0),
        (CausalFaultMode::LostReply, 1),
    ] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, head) = closed_with_control_unavailable(mode);
        let checkpoint = exact_checkpoint(&fixture);
        assert_eq!(fixture.transport.application_count("turn_checkpoint"), applied);
        assert_eq!(fixture.transport.attempts.lock().unwrap()["turn_checkpoint"], 1);
        assert!(read(&fixture, |record| record.engram.admission_retry.is_none()));
        tick_keeps_the_duty(&fixture, &head, &checkpoint);
        assert_eq!(fixture.transport.application_count("turn_checkpoint"), applied,
            "local recovery does not invent another application or receipt");
    }
}

#[test]
fn a_successor_stays_unadmitted_while_the_prior_checkpoint_is_unsettled() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, head) = closed_with_control_unavailable(CausalFaultMode::NeverStarted);
    let checkpoint = exact_checkpoint(&fixture);
    let (grant, lineage, prepared) = read(&fixture, |record| (
        record.engram.active_grant_id.clone(),
        record.engram.begin_preparation_lineage.clone(),
        record.queued_prompts.front().unwrap().has_engram_intent(),
    ));
    assert!(grant.is_some(), "the original grant remains locally open");
    assert!(lineage.is_none(), "an excluded successor has not started an admission window");
    assert!(!prepared, "no successor Bind/Evaluate/Begin is prepared");
    tick_keeps_the_duty(&fixture, &head, &checkpoint);
}

#[test]
fn an_uncertain_grant_after_a_control_unavailable_close_keeps_the_hold() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (fixture, head) = closed_with_control_unavailable(CausalFaultMode::NeverStarted);
    let checkpoint = exact_checkpoint(&fixture);
    let uncertain = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let grant = record.engram.active_grant_id.clone().expect("original grant");
        record.engram.uncertain_grant_id = Some(grant.clone());
        grant
    };
    tick_keeps_the_duty(&fixture, &head, &checkpoint);
    assert_eq!(read(&fixture, |record| record.engram.uncertain_grant_id.clone()).as_deref(),
        Some(uncertain.as_str()), "the extra uncertainty is not silently cleared");
}

#[test]
fn project_containment_applied_and_unapplied_unknown_reports_allow_fresh_off_use() {
    for (mode, applied) in [
        (CausalFaultMode::NeverStarted, 0),
        (CausalFaultMode::LostReply, 1),
    ] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (fixture, head) = closed_with_control_unavailable(mode);
        let checkpoint = exact_checkpoint(&fixture);
        fixture.state.cancel_queued_prompt(&fixture.session, head["id"].as_str().unwrap()).unwrap();
        let project_id = {
            let mut inner = fixture.state.inner.lock().unwrap();
            let project_id = engram_project_for_session_locked(&inner, &fixture.session).unwrap().id.clone();
            inner.projects.iter_mut().find(|project| project.id == project_id).unwrap()
                .engram.as_mut().unwrap().turn_gated_control = false;
            fixture.state.commit_locked(&mut inner).unwrap();
            project_id
        };
        let attempts = fixture.transport.attempts.lock().unwrap().clone();
        let dispatch = match fixture.state.dispatch_turn(&fixture.session, SendMessageRequest {
            text: "Fresh OFF work; do not replay the old prompt.".into(),
            expanded_text: None, attachments: Vec::new(), source_session_id: None, source_mailbox: None,
        }).unwrap() {
            DispatchTurnResult::Dispatched(dispatch) => dispatch,
            DispatchTurnResult::Queued | DispatchTurnResult::DispatchedAfterQueue(_) =>
                panic!("quiescent OFF report debt must dispatch the fresh prompt directly"),
        };
        assert!(dispatch.engram_dispatch_generation().is_none());
        let token = dispatch.runtime_token().clone();
        assert!(matches!(deliver_turn_dispatch(&fixture.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
        assert!(matches!(fixture.receiver.recv_timeout(DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
        fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token).unwrap();
        assert_eq!(exact_checkpoint(&fixture), checkpoint);
        assert_eq!(*fixture.transport.attempts.lock().unwrap(), attempts);
        assert_eq!(fixture.transport.application_count("turn_checkpoint"), applied);
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.projects.iter_mut().find(|project| project.id == project_id).unwrap()
            .engram.as_mut().unwrap().turn_gated_control = true;
        fixture.state.commit_locked(&mut inner).unwrap();
        assert!(AppState::engram_binding_target_for_session_shape_locked(&inner, &fixture.session, true)
            .unwrap().is_none());
        assert!(require_no_engram_local_dispatch_obligation(&inner, &fixture.session).is_err());
        assert!(engram_session_obligation_locked(&inner, &fixture.session).is_some());
    }
}
