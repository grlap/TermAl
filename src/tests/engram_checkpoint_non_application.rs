//! A producer-certified no-change reply must retain the complete closing report.
//! Uses the real recorder and closing path, with a scripted control response.

use super::*;

fn assert_report_survives(code: &str, next_intent: EngramNextIntent) {
    let turn = CheckedTurn::start_with(
        &format!("non-application-{code}-{next_intent:?}"),
        true,
        None,
        vec![remote_error_reply(code), checkpoint_reply(CHECK_GRANT)],
    );
    turn.record_mut(|record| {
        record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
    });
    turn.run(
        "retained-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    let (token, generation) = turn.record(|record| {
        (
            record.runtime.runtime_token().unwrap(),
            record.active_turn_generation,
        )
    });
    let close = || {
        turn.state.checkpoint_engram_turn_off_lock(
            &turn.session_id,
            EngramCheckpointPurpose::TurnTerminal,
            Some(&token),
            Some(generation),
            next_intent,
            Some(EngramExecutionOutcome::Succeeded),
            None,
        )
    };
    let _ = close();
    let first = checkpoints(&turn);
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0]["verification_evidence"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(first[0]["verification_evidence"][0]["check_kind"], "test");
    let retained = turn
        .state
        .cached_engram_turn_report(&turn.session_id, CHECK_GRANT);
    assert_eq!(
        retained.verification_evidence.len(),
        1,
        "{code}: a certified no-change invocation cannot discard the recorded test"
    );
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some(CHECK_GRANT)
    );

    let _ = close();
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1], sent[0],
        "{code}: report, grant, routing and key are replayed exactly"
    );
    assert!(turn.record(|record| record.engram.active_grant_id.is_none()));
    eprintln!(
        "certified invocation replay: {code} {next_intent:?} retained the report and exact key"
    );
}

#[test]
fn control_non_application_checkpoint_keeps_recorded_test_on_retry() {
    for code in ["control_request_expired", "control_store_busy"] {
        assert_report_survives(code, EngramNextIntent::Wait);
    }
}

#[test]
fn control_non_application_exit_keeps_report_and_same_key_on_retry() {
    for code in ["control_request_expired", "control_store_busy"] {
        assert_report_survives(code, EngramNextIntent::Exit);
    }
}

// The next ordinary admission, rather than a second direct call to close,
// must settle the retained operation before it may evaluate a new turn.
fn assert_ordinary_continuation(code: &str, next_intent: EngramNextIntent) {
    const NEXT: &str = "ordinary-continuation-grant";
    let mut replies = vec![remote_error_reply(code), checkpoint_reply(CHECK_GRANT)];
    if next_intent == EngramNextIntent::Exit {
        // A successful Exit, unlike its certified failure, ends the binding.
        replies.extend([status_reply("closed"), bind_reply("after-exit-token")]);
    }
    replies.extend([grant_reply(NEXT), begin_reply(NEXT)]);
    let turn = CheckedTurn::start_with_turns(
        &format!("ordinary-{code}-{next_intent:?}"),
        true,
        None,
        replies,
        3,
    );
    turn.record_mut(|record| {
        record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite);
    });
    turn.run(
        "ordinary-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    if next_intent == EngramNextIntent::Wait {
        turn.finish();
    } else {
        let (token, generation) = turn.record(|record| {
            (
                record.runtime.runtime_token().unwrap(),
                record.active_turn_generation,
            )
        });
        assert!(matches!(
            turn.state.checkpoint_engram_turn_off_lock(
                &turn.session_id,
                EngramCheckpointPurpose::TurnTerminal,
                Some(&token),
                Some(generation),
                next_intent,
                Some(EngramExecutionOutcome::Succeeded),
                None,
            ),
            EngramCheckpointOutcome::Failed(_)
        ));
        // Exit normally accompanies lifecycle teardown. Keep this fixture's
        // receiver alive to observe a later ordinary admission on the session.
        turn.record_mut(|record| record.session.status = SessionStatus::Idle);
    }
    let first = checkpoints(&turn).pop().unwrap();
    assert_eq!(
        first["verification_evidence"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(
        !turn.record(|record| record.engram.rebind_required),
        "{code}: certified non-application does not invalidate a live binding"
    );
    if next_intent == EngramNextIntent::Exit {
        use_scripted_recovery_status(&turn);
    }
    let before = turn.transport.requests().len();
    begin_check_outcome_turn(&turn);
    let requests = turn.transport.requests();
    assert_eq!(
        requests[before].request, first,
        "{code}: the next admission replays the whole original checkpoint first"
    );
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1], first,
        "{code}: no restart key or intent substitution"
    );
    let operations: Vec<_> = requests[before..]
        .iter()
        .map(|request| request.request["operation"].as_str().unwrap())
        .filter(|operation| *operation != "named_root_read")
        .collect();
    if next_intent == EngramNextIntent::Wait {
        assert_eq!(
            operations,
            ["turn_checkpoint", "turn_evaluate", "turn_begin"]
        );
    } else {
        assert_eq!(
            operations,
            [
                "turn_checkpoint",
                "session_status",
                "session_bind",
                "turn_evaluate",
                "turn_begin"
            ]
        );
    }
    assert!(matches!(
        receive(&turn._runtime_rx, "continued prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some(NEXT)
    );
    eprintln!("ordinary continuation: {code} {next_intent:?} exact checkpoint before Evaluate");
}

#[test]
fn control_non_application_ordinary_expired_wait_replays_original_request() {
    assert_ordinary_continuation("control_request_expired", EngramNextIntent::Wait);
}

#[test]
fn control_non_application_ordinary_busy_wait_replays_original_request() {
    assert_ordinary_continuation("control_store_busy", EngramNextIntent::Wait);
}

#[test]
fn control_non_application_ordinary_expired_exit_replays_original_request() {
    assert_ordinary_continuation("control_request_expired", EngramNextIntent::Exit);
}

#[test]
fn control_non_application_ordinary_busy_exit_replays_original_request() {
    assert_ordinary_continuation("control_store_busy", EngramNextIntent::Exit);
}

// Evidence preparation has already finished. Its readiness-model status
// allowance must not intercept the explicit closing/rebind status replies.
fn use_scripted_recovery_status(turn: &CheckedTurn) {
    let mut roots = turn.transport.named_roots.lock().unwrap();
    let roots = roots.as_mut().unwrap();
    roots.scripted_status = true;
    roots.observation_status_pending = false;
}

#[test]
fn control_non_application_expired_binding_still_requires_reconciliation() {
    for code in ["control_request_expired", "control_store_busy"] {
        let turn = CheckedTurn::start_with_turns(
            &format!("expired-binding-{code}"),
            true,
            None,
            vec![
                remote_error_reply(code),
                remote_error_reply("invalid_routing_token"),
                remote_error_reply("invalid_routing_token"),
                bind_reply("replacement-token"),
            ],
            3,
        );
        turn.finish();
        use_scripted_recovery_status(&turn);
        let first = checkpoints(&turn).pop().unwrap();
        let before = turn.transport.requests().len();
        let error = turn
            .state
            .ensure_engram_session_bound_off_lock(&turn.session_id)
            .err()
            .expect("replacement process rejects the old token");
        assert_eq!(error.code.as_deref(), Some("invalid_routing_token"));
        assert_eq!(checkpoints(&turn)[1], first);
        assert!(turn.record(|record| record.engram.rebind_required));
        let target = turn
            .state
            .ensure_engram_session_bound_off_lock(&turn.session_id)
            .unwrap()
            .unwrap();
        assert_eq!(target.routing_token.as_deref(), Some("replacement-token"));
        // ensure returns a captured target; ownership settlement belongs to
        // the live record, which status recovery and the fresh bind update.
        turn.record(|record| {
            assert_eq!(
                record.engram.routing_token.as_deref(),
                Some("replacement-token")
            );
            assert!(record.engram.active_grant_id.is_none());
            assert!(record.engram.uncertain_grant_id.is_none());
            assert!(record.engram.retained_checkpoint.is_none());
            assert!(!record.engram.rebind_required);
        });
        let operations: Vec<_> = turn
            .transport
            .requests()
            .into_iter()
            .skip(before)
            .map(|request| request.request["operation"].as_str().unwrap().to_owned())
            .filter(|operation| operation != "named_root_read")
            .collect();
        assert_eq!(
            operations,
            ["turn_checkpoint", "session_status", "session_bind"]
        );
        eprintln!("expired binding: {code} preserves existing reconciliation");
    }
}
