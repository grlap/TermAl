//! Policy healing and real Stop arbitration after an ordinary marker fence.
//! Mounted by engram_admission_availability_contract.rs.
use super::*;

#[test]
fn ordinary_admission_regression_refusal_healing_never_falls_open() {
    for code in ["stale_fence", "control_assurance_insufficient"] {
        for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
            let refusal = ScriptedEngramControlResponse::Reply(Ok(json!({
                "decision": "refuse",
                "directive": {
                    "directive_id": "healing-refusal",
                    "code": code,
                    "target": "host",
                    "satisfaction": "rebind",
                    "effect": "mutate_local",
                    "declared_mediated_effects": ["observe"],
                    "required_assurance": "turn_gated"
                }
            })));
            let (state, session, receiver, transport) = root_fixture([
                bind_reply("refused-token"),
                refusal,
                status_reply("ready"),
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::spawn_failed(
                    "healing sidecar did not start",
                ))),
            ]);
            queue_test_engram_prompt(&state, &session, "policy must be settled", source, None);
            let started = state
                .start_next_queued_turn_off_lock(&session, false, false)
                .unwrap()
                .unwrap();
            let _ = deliver_turn_dispatch(&state, started.dispatch);
            assert_eq!(
                operations(&transport),
                [
                    "session_bind",
                    "turn_evaluate",
                    "session_status",
                    "session_bind"
                ]
            );
            assert!(
                receiver.try_recv().is_err(),
                "a prior refusal excludes ungranted fallback: {code}"
            );
            let held = inspect(&state, &session, |record| record.clone());
            assert_eq!(held.queued_prompts.len(), 1);
            assert!(!held.queued_prompts[0].engram_ordinary_handoff);
            assert!(held.engram.ordinary_handoff.is_none());
            assert!(held.engram.active_grant_id.is_none());
        }
    }
}

async fn ordinary_stop_case(failed_stop: bool, callback_first: bool) {
    let (state, session, root_rx, transport) =
        root_fixture([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::spawn_failed("optional control unavailable"),
        ))]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation();
    let active_turn_generation = dispatch.active_turn_generation();
    assert_eq!(
        state.prepare_engram_turn_delivery_off_lock(&session, generation.unwrap()),
        EngramTurnDeliveryPreparation::Ready
    );
    let prepared = inspect(&state, &session, |record| record.clone());
    assert!(prepared.queued_prompts[0].engram_ordinary_handoff);
    assert!(prepared.engram.ordinary_handoff.is_some());
    assert!(prepared.engram.active_grant_id.is_none());

    // Use the same real kill-failure and Stop fence as post-receipt tests.
    // Only the provider channel is adapted to an ACP fixture at this seam.
    let (runtime, receiver) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "ordinary-stop-arbitration");
    let token = RuntimeToken::Acp(runtime.runtime_id.clone());
    let process = runtime.process.clone();
    let sender = runtime.input_tx.clone();
    let lifecycle = runtime.turn_lifecycle.clone();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }
    let _failure = failed_stop.then(|| force_test_kill_child_process_failure(&process, "OpenCode"));
    let gate = install_test_stop_fence_gate(&state, &session);
    let mut events = state.subscribe_events();
    state.request_stop_session(&session).unwrap();
    gate.wait_until_claimed();
    let mut dispatch = Some(TurnDispatch::PersistentAcp {
        federation_handoff: None,
        opening_prompt: None,
        active_turn_generation,
        engram_dispatch_generation: generation,
        runtime_token: token.clone(),
        session_id: session.clone(),
        mailbox_notification: None,
        sender,
        turn_lifecycle: lifecycle,
        command: AcpPromptCommand {
            cwd: String::new(),
            cursor_mode: None,
            model: String::new(),
            opencode_effort: None,
            opencode_mode: None,
            prompt: "exact ordinary prompt".into(),
            resume_session_id: None,
        },
    });
    let early =
        callback_first.then(|| handoff_prepared_turn_dispatch(&state, dispatch.take().unwrap()));
    let callbacks = inspect(&state, &session, |record| {
        record.deferred_stop_callbacks.clone()
    });
    let delivered_during_stop = receiver
        .try_iter()
        .any(|command| matches!(command, AcpRuntimeCommand::Prompt(_)));
    let replay_carrier = callbacks.iter().find_map(|callback| match callback {
        DeferredStopCallback::EngramHandoff { dispatch, .. } => Some(dispatch.clone()),
        _ => None,
    });
    // Force the exact observation gap: Stop clears its runtime fence before
    // the background replay takes this one-shot carrier and enqueues it.
    let replay_blocker = (failed_stop && callback_first).then(|| {
        replay_carrier
            .as_ref()
            .expect("deferred handoff carrier")
            .0
            .lock()
            .unwrap()
    });
    // Release and observe Stop completion before any assertion, including RED.
    gate.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            if inspect(&state, &session, |record| !record.runtime_stop_in_progress) {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("real Stop must settle its runtime fence");
    let empty_before_replay = replay_blocker.as_ref().map(|_| {
        !receiver
            .try_iter()
            .any(|command| matches!(command, AcpRuntimeCommand::Prompt(_)))
    });
    drop(replay_blocker);
    let outcome = early
        .unwrap_or_else(|| handoff_prepared_turn_dispatch(&state, dispatch.take().unwrap()))
        .unwrap();
    let mut prompts = Vec::new();
    if failed_stop {
        loop {
            let command = phase_sync::receive(
                &receiver,
                "failed Stop finishes its ordinary handoff replay",
            );
            if let AcpRuntimeCommand::Prompt(command) = command {
                prompts.push(command.prompt);
                break;
            }
        }
    }
    // A copied callback still owns the same one-shot take; it cannot resend.
    state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks.clone());
    state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
    prompts.extend(
        receiver
            .try_iter()
            .filter_map(|command| match command {
                AcpRuntimeCommand::Prompt(command) => Some(command.prompt),
                _ => None,
            })
            .collect::<Vec<_>>(),
    );
    let after = inspect(&state, &session, |record| record.clone());
    assert!(
        !delivered_during_stop,
        "Stop arbitration forbids provider enqueue"
    );
    if let Some(empty) = empty_before_replay {
        assert!(
            empty,
            "Stop fence completion alone does not imply callback delivery"
        );
    }
    if callback_first {
        assert!(
            matches!(outcome, HandoffPreparedTurnDispatchOutcome::Scheduled),
            "a prepared ordinary handoff must be retained by Stop arbitration"
        );
    } else if failed_stop {
        assert!(
            matches!(outcome, HandoffPreparedTurnDispatchOutcome::Delivered),
            "failed Stop must restore the still-owned ordinary dispatch"
        );
    } else {
        assert!(matches!(
            outcome,
            HandoffPreparedTurnDispatchOutcome::Superseded
        ));
    }
    assert_eq!(
        prompts,
        if failed_stop {
            vec!["exact ordinary prompt".to_owned()]
        } else {
            vec![]
        }
    );
    assert!(root_rx.try_recv().is_err());
    assert!(after.engram.ordinary_handoff.is_none());
    assert!(
        !operations(&transport)
            .iter()
            .any(|op| op == "turn_begin" || op == "turn_checkpoint")
    );
}

#[tokio::test]
async fn ordinary_admission_regression_handoff_waits_for_public_stop_outcome() {
    for failed_stop in [true, false] {
        ordinary_stop_case(failed_stop, true).await;
    }
}

#[tokio::test]
async fn ordinary_admission_regression_failed_stop_restores_late_handoff() {
    for failed_stop in [true, false] {
        ordinary_stop_case(failed_stop, false).await;
    }
}
