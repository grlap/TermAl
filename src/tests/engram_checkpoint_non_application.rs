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
fn project_containment_checkpoint_review_healthy_turn_history_is_bounded_through_reload() {
    const TURNS: usize = 24;
    let grants: Vec<String> = (0..TURNS).map(|index| {
        if index == 0 { CHECK_GRANT.into() } else { format!("history-grant-{index}") }
    }).collect();
    let mut replies = Vec::new();
    for index in 0..TURNS {
        replies.push(checkpoint_reply(&grants[index]));
        if index + 1 < TURNS {
            replies.push(grant_reply(&grants[index + 1]));
            replies.push(begin_reply(&grants[index + 1]));
        }
    }
    let turn = CheckedTurn::start_with_turns("bounded-checkpoint-history", true, None, replies, TURNS);
    turn.record_mut(|record| record.active_codex_sandbox_mode = Some(CodexSandboxMode::WorkspaceWrite));
    for index in 0..TURNS {
        turn.finish();
        assert!(turn.record(|record| record.engram.active_grant_id.is_none()));
        if index + 1 < TURNS { begin_check_outcome_turn(&turn); }
    }
    assert_eq!(checkpoints(&turn).len(), TURNS);
    {
        let inner = turn.state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &turn.session_id).is_none());
        assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
        assert!(inner.engram_session_obligations.checkpoint_operations.len() <= 1,
            "healthy ACKed checkpoints must not accumulate full history every turn: {}",
            inner.engram_session_obligations.checkpoint_operations.len());
        assert!(inner.engram_confirmed_containment_ids.len() <= 1);
    }
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.engram_session_obligations.checkpoint_operations.len() <= 1);
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_none());
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
fn project_containment_checkpoint_on_receipt_ack_drains_queued_prompt_once() {
    fn hold_ack_after(
        remaining: usize,
        entered: mpsc::SyncSender<()>,
        release: mpsc::Receiver<()>,
    ) {
        TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                if remaining == 0 {
                    entered.send(()).unwrap();
                    let _ = release.recv();
                } else {
                    hold_ack_after(remaining - 1, entered, release);
                }
            }));
        });
    }
    const NEXT: &str = "after-checkpoint-ack";
    let turn = CheckedTurn::start_with_turns(
        "ordinary-checkpoint-ack",
        true,
        None,
        vec![
            checkpoint_reply(CHECK_GRANT),
            grant_reply(NEXT),
            begin_reply(NEXT),
        ],
        3,
    );
    let token = turn.record(|record| record.runtime.runtime_token().unwrap());
    let closing_state = turn.state.clone();
    let closing_session = turn.session_id.clone();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let closing = std::thread::spawn(move || {
        // Two pre-send confirmations precede the receipt/effects ACK.
        hold_ack_after(2, entered_tx, release_rx);
        closing_state.finish_turn_ok_if_runtime_matches(&closing_session, &token)
    });
    receive(&entered_rx, "checkpoint receipt ACK");
    let before = turn.transport.requests().len();
    let queued = turn.state.dispatch_turn(
        &turn.session_id,
        SendMessageRequest {
            text: "Continue after the acknowledged checkpoint.".into(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    let no_control = turn.transport.requests().len() == before;
    let no_provider = turn._runtime_rx.try_recv().is_err();
    drop(release_tx);
    closing.join().unwrap().unwrap();
    assert!(matches!(queued, Ok(DispatchTurnResult::Queued)));
    assert!(
        no_control && no_provider,
        "queueing during ACK cannot start admission or provider work"
    );
    assert!(matches!(
        receive(&turn._runtime_rx, "automatically drained prompt"),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(
        turn._runtime_rx.try_recv().is_err(),
        "healthy completion drains exactly once"
    );
    assert_eq!(
        turn.record(|record| record.engram.active_grant_id.clone())
            .as_deref(),
        Some(NEXT)
    );
    let operations = turn
        .transport
        .requests()
        .into_iter()
        .skip(before)
        .map(|request| request.request["operation"].as_str().unwrap().to_owned())
        .filter(|operation| operation != "named_root_read")
        .collect::<Vec<_>>();
    assert_eq!(operations, ["turn_evaluate", "turn_begin"]);
}

#[test]
fn control_non_application_expired_binding_still_requires_reconciliation() {
    for (code, action) in [
        ("control_request_expired", "admission"),
        ("control_request_expired", "status"),
        ("control_store_busy", "admission"),
        ("control_store_busy", "status"),
    ] {
        let turn = CheckedTurn::start_with_turns(
            &format!("expired-binding-{code}-{action}"),
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
        // Admission must first recover the immutable original. The existing
        // diagnostic action can instead read status, without settling debt.
        let error = if action == "admission" {
            turn.state.ensure_engram_session_bound_off_lock(&turn.session_id).err()
                .expect("unresolved original recovery cannot authorize a replacement binding")
        } else {
            turn.state.diagnose_original_checkpoint_status(&turn.session_id, None, None)
                .expect("an invalid status token cannot release the protected checkpoint")
        };
        assert_eq!(
            error.to_string(),
            if action == "admission" {
                "Original checkpoint result remains unresolved: scripted Engram error: invalid_routing_token"
            } else {
                "Status cannot settle the protected original checkpoint or discard its report"
            }
        );
        // Keep the literal two-invalid-token sequence covered. Neither
        // refusal says whether the original report was accounted, and the
        // queued replacement Bind reply must remain an unused trap.
        turn.record(|record| {
            assert_eq!(record.engram.active_grant_id.as_deref(), Some(CHECK_GRANT));
            assert!(record.engram.uncertain_grant_id.is_none());
            assert!(record.engram.retained_checkpoint.is_some());
            assert!(record.engram.rebind_required);
            let operation = record.engram.checkpoint_operation.as_ref().unwrap();
            assert_eq!(
                serde_json::to_value(&operation.original.request).unwrap(),
                first
            );
            assert!(matches!(
                operation.attempts.last().unwrap().result,
                Some(EngramCheckpointAttemptResult::Unknown { .. })
            ));
        });
        let inner = turn.state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &turn.session_id).is_some());
        let operation = inner.sessions[inner.find_session_index(&turn.session_id).unwrap()]
            .engram.checkpoint_operation.as_ref().unwrap();
        let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|entry| entry.original.id == operation.original.id).unwrap();
        assert_eq!(serde_json::to_value(operation).unwrap(), serde_json::to_value(canonical).unwrap());
        assert!(!inner.engram_confirmed_containment_ids.contains(&operation.original.id));
        drop(inner);
        let operations: Vec<_> = turn
            .transport
            .requests()
            .into_iter()
            .skip(before)
            .map(|request| request.request["operation"].as_str().unwrap().to_owned())
            .filter(|operation| operation != "named_root_read")
            .collect();
        assert_eq!(operations, if action == "admission" {
            ["turn_checkpoint", "turn_checkpoint"]
        } else { ["turn_checkpoint", "session_status"] });
        let sent = checkpoints(&turn);
        assert_eq!(sent.len(), if action == "admission" { 3 } else { 2 });
        assert!(sent.iter().all(|request| request == &first), "every replay keeps the exact original report and key");
        assert!(turn._runtime_rx.try_recv().is_err());
        eprintln!(
            "expired binding: {code}/{action} preserves the unresolved checkpoint without a fresh Bind"
        );
    }
}
