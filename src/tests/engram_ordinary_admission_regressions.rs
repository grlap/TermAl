//! Real Stop arbitration and ordinary delivery without an admission marker.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

#[test]
fn ordinary_user_and_mailbox_delivery_never_consults_retired_healing_responses() {
    for code in ["stale_fence", "control_assurance_insufficient"] {
        for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
            let turn = OrdinaryRoot::new("ordinary-unused-healing");
            turn.transport.responses.lock().unwrap().push_back(
                ScriptedEngramControlResponse::Reply(Ok(json!({"decision":"refuse","code":code}))),
            );
            turn.enqueue("ordinary payload with original source", source);
            let started = turn
                .state
                .start_next_queued_turn_off_lock(&turn.session, false, false)
                .unwrap()
                .unwrap();
            deliver_turn_dispatch(&turn.state, started.dispatch)
                .into_public_result()
                .unwrap();
            assert!(
                turn.prompt()
                    .prompt
                    .contains("ordinary payload with original source")
            );
            turn.record(|record| {
                assert!(record.queued_prompts.is_empty());
                assert!(record.prompt_deliveries.is_empty());
            });
            assert_eq!(turn.transport.responses.lock().unwrap().len(), 1);
            turn.assert_no_prompt();
            turn.assert_no_control();
        }
    }
}

pub(super) async fn ordinary_stop_case(failed_stop: bool, callback_first: bool) {
    ordinary_stop_receipt_case(failed_stop, callback_first, true, None).await;
}

async fn ordinary_stop_receipt_case(
    failed_stop: bool,
    callback_first: bool,
    receipted: bool,
    changed_receipt: Option<&str>,
) {
    let mut turn = OrdinaryRoot::new("ordinary-stop-arbitration");
    let writer = DeliveryWriter::attach(&mut turn.state);
    let (prepared, receipt_id, _write_ahead_fence) = if receipted {
        let id = turn.enqueue_durable("exact ordinary prompt", true);
        assert!(
            turn.state
                .request_queued_prompt_durability(&turn.session, &id)
        );
        writer.take(HeldFence::Durability).finish(Ok(()));
        let fence = writer.take(HeldFence::WriteAhead);
        turn.record(|record| {
            let receipt = &record.prompt_deliveries[&id];
            assert_eq!(receipt.disposition, PromptDeliveryDisposition::HandingOff);
            assert!(!receipt.write_ahead_acknowledged);
            assert!(receipt.turn_generation.is_none() && receipt.entry.is_none());
        });
        // DeliveryWriter is a no-write acknowledgement double. Set only its
        // ACK latch here so production queue promotion returns the prepared
        // dispatch before settlement's automatic provider enqueue. Promotion,
        // receipt identity and removal of the queue row remain production code.
        turn.edit(|record| {
            record
                .prompt_deliveries
                .get_mut(&id)
                .unwrap()
                .write_ahead_acknowledged = true;
        });
        let started = turn
            .state
            .start_next_queued_turn_off_lock(&turn.session, false, false)
            .unwrap()
            .expect("acknowledged ordinary receipt promotes");
        turn.record(|record| {
            assert!(
                record.queued_prompts.is_empty(),
                "ordinary promotion removes its queue row"
            );
            let receipt = &record.prompt_deliveries[&id];
            assert_eq!(receipt.disposition, PromptDeliveryDisposition::HandingOff);
            assert_eq!(
                receipt.turn_generation,
                Some(started.dispatch.active_turn_generation())
            );
            assert_eq!(receipt.entry.as_ref().unwrap().pending_prompt.id, id);
            assert!(receipt.message_id.is_some() && receipt.attempt_key.is_some());
        });
        (started.dispatch, Some(id), Some(fence))
    } else {
        (turn.prepare("exact ordinary prompt"), None, None)
    };
    turn.assert_no_prompt();
    let promoted_receipt = receipt_id.as_ref().map(|id| {
        turn.record(|record| serde_json::to_value(&record.prompt_deliveries[id]).unwrap())
    });
    let state = &turn.state;
    let session = &turn.session;
    let root_rx = &turn.receiver;
    let active_turn_generation = prepared.active_turn_generation();
    drop(prepared);

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
    if let Some(change) = changed_receipt {
        turn.edit(|record| {
            let receipt = record
                .prompt_deliveries
                .get_mut(receipt_id.as_ref().unwrap())
                .unwrap();
            match change {
                "nonce" => receipt.write_ahead = Uuid::new_v4().to_string(),
                "attempt" => receipt.attempt += 1,
                "settled" => receipt.disposition = PromptDeliveryDisposition::Accepted,
                _ => panic!("unknown receipt mutation"),
            }
        });
    }
    let restores_handoff = failed_stop && receipted && changed_receipt.is_none();
    let mut dispatch = Some(TurnDispatch::PersistentAcp {
        human_prompt_id: None,
        federation_handoff: None,
        opening_prompt: None,
        active_turn_generation,
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
    let callbacks = turn.record(|record| record.deferred_stop_callbacks.clone());
    let delivered_during_stop = receiver
        .try_iter()
        .any(|command| matches!(command, AcpRuntimeCommand::Prompt(_)));
    let replay_carrier = callbacks.iter().find_map(|callback| match callback {
        DeferredStopCallback::TurnHandoff { dispatch, .. } => Some(dispatch.clone()),
        _ => None,
    });
    // Force the exact observation gap: Stop clears its runtime fence before
    // the background replay takes this one-shot carrier and enqueues it.
    let replay_blocker = (restores_handoff && callback_first).then(|| {
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
            if turn.record(|record| !record.runtime_stop_in_progress) {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("real Stop must settle its runtime fence");
    if failed_stop {
        turn.record(|record| {
            assert_eq!(
                record.session.status,
                if restores_handoff {
                    SessionStatus::Active
                } else {
                    SessionStatus::Error
                }
            );
            assert!(
                record.queued_prompts.is_empty(),
                "Stop cannot recreate a queue owner"
            );
            if restores_handoff {
                let receipt = &record.prompt_deliveries[receipt_id.as_ref().unwrap()];
                assert!(receipt.write_ahead_acknowledged);
                assert_eq!(
                    Some(serde_json::to_value(receipt).unwrap()),
                    promoted_receipt,
                    "failed Stop restores the same receipt without replacing it"
                );
            } else if !receipted {
                assert!(
                    record.prompt_deliveries.is_empty(),
                    "direct dispatch has no receipt owner"
                );
            }
        });
    }
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
    if restores_handoff {
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
    turn.record(|record| assert!(!record.runtime_stop_in_progress));
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
    } else if restores_handoff {
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
        if restores_handoff {
            vec!["exact ordinary prompt".to_owned()]
        } else {
            vec![]
        }
    );
    assert!(root_rx.try_recv().is_err());
    turn.assert_no_control();
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

#[tokio::test]
async fn ordinary_failed_stop_does_not_restore_ownerless_direct_dispatch() {
    for callback_first in [true, false] {
        ordinary_stop_receipt_case(true, callback_first, false, None).await;
    }
}

#[tokio::test]
async fn ordinary_failed_stop_does_not_restore_a_changed_receipt() {
    for change in ["nonce", "attempt", "settled"] {
        for callback_first in [true, false] {
            ordinary_stop_receipt_case(true, callback_first, true, Some(change)).await;
        }
    }
}
