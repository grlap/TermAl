// Owns atomic retained no-grant decision recovery after a card commit failure.
// Does not grant authority or simulate provider delivery.
use super::*;

fn no_grant_decision_commit_failure_keeps_local_recovery(defer: bool) {
    let (state, project, session, root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram = EngramSessionState::default();
        record.engram.context_nudge_pending = false;
        record.session.status = SessionStatus::Idle;
        state.commit_locked(&mut inner).unwrap();
    }
    let decision = if defer {
        defer_reply("busy")
    } else {
        evaluation_refusal_reply("operator_review_required")
    };
    let transport = ScriptedEngramControlTransport::new([
        bind_reply("original-token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "fixture lost the original no-grant evaluation reply",
        ))),
        status_reply("ready"),
        decision,
    ]);
    state.install_control_test_transport(transport.clone());
    let dispatch = match state.dispatch_turn(&session, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the initial admission must run"),
    };
    deliver_turn_dispatch(&state, dispatch).unwrap();
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let owner = {
        let inner = state.inner.lock().unwrap();
        EngramQueuedAdmissionOwner::capture(
            &inner.sessions[inner.find_session_index(&session).unwrap()],
        )
        .unwrap()
    };
    let _dispatch = state
        .dispatch_next_queued_turn_for_resume(&session, owner)
        .unwrap()
        .expect("the original retained evaluation must promote");
    let generation = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(
            matches!(
                record.engram.pending_dispatch.as_ref().unwrap().evaluated,
                EngramDispatchEvaluation::Defer { .. }
            ) == defer
        );
        record.engram.dispatch_generation
    };
    let before = transport.requests().len();
    let card = EngramControlCard {
        causal_failure: None,
        schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
        stage: EngramControlStage::Dispatch,
        assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
        decision: if defer {
            EngramControlCardDecision::Defer
        } else {
            EngramControlCardDecision::Refuse
        },
        dispatch: EngramControlCardDispatch::Withheld,
        refusal_code: (!defer).then(|| "operator_review_required".to_owned()),
        defer_code: defer.then(|| "busy".to_owned()),
        grant_id: None,
        directives: Vec::new(),
        delivered_range: None,
        latency_ms: EngramControlLatencyCard {
            admission_fence: None,
            preparation_fence: None,
            evaluate: Some(0),
            begin: None,
            checkpoint: None,
            total: 0,
        },
        fail_mode: EngramControlFailMode::Enforced,
        repair_armed: false,
        next_intent: None,
        source_root: None,
    };
    let failing = AppState {
        persistence_path: Arc::new(root),
        ..state.clone()
    };
    assert!(matches!(
        failing.finish_engram_dispatch_record(&session, generation, None, None, card),
        EngramDispatchRecordFinish::PersistenceUnknown
    ));
    drop(scope);
    {
        let mut inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert_eq!(
            entry.phase,
            EngramSessionObligationPhase::Confirmed,
            "an unknown commit must retain the no-grant decision beside its local effects"
        );
        assert!(containment_admission_decision_matches(entry));
        assert!(!inner.engram_confirmed_containment_ids.contains(&entry.id));
        state.commit_locked(&mut inner).unwrap();
    }
    state
        .retry_containment_admission_transfer(&session)
        .unwrap();
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    assert_eq!(
        transport.requests().len(),
        before,
        "decision recovery is local only"
    );
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_refuse_unknown_commit_preserves_local_decision_recovery() {
    no_grant_decision_commit_failure_keeps_local_recovery(false);
}

#[test]
fn project_containment_defer_unknown_commit_preserves_local_decision_recovery() {
    no_grant_decision_commit_failure_keeps_local_recovery(true);
}
