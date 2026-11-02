//! Ordinary provider handoff, receipt and Stop ownership regressions.
//! No control grant or checkpoint authorizes these turns. Historical queue
//! metadata stays passive; real receipt WAL, runtime/turn and Stop owners remain.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::delegation_support::finish_delegation_child_with_assistant_text;

// A known-unsent original carrying historical bytes is promoted by the real
// ordinary queue owner. The product retains that row after promotion, so the
// Stop tests exercise exact retirement rather than a synthetic admission.
fn prepare_ordinary_owned_head(turn: &OrdinaryRoot) -> TurnDispatch {
    turn.enqueue("Prepared ordinary input", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts.back_mut().unwrap().engram_evaluate =
            Some(json!({"historical":"known-unsent","future":[null,"żółw"]}));
    });
    turn.state.start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap().unwrap().dispatch
}

// A fresh receipted promotion keeps custody in delivery.entry, not a queued
// history row. DeliveryWriter acknowledges without writing SQLite; only the
// WAL ACK latch is set here to capture a prepared dispatch before automatic
// enqueue. Production code creates HandingOff and the promoted identities.
fn prepare_receipted_ordinary_promotion(
    turn: &mut OrdinaryRoot,
    source: QueuedPromptSource,
) -> (TurnDispatch, String, crate::tests::prompt_delivery::DeliveryWriter, Box<PersistFence>) {
    use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
    let writer = DeliveryWriter::attach(&mut turn.state);
    let id = turn.enqueue_durable("Prepared ordinary input", true);
    // Select the queue-source variant before its WAL intent is captured.
    // Neither variant is a Human prompt or a historical admission alias.
    assert!(matches!(source, QueuedPromptSource::User | QueuedPromptSource::Orchestrator));
    turn.edit(|record| record.queued_prompts.back_mut().unwrap().source = source);
    assert!(turn.state.request_queued_prompt_durability(&turn.session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    if source == QueuedPromptSource::User {
        // The producer initially held Orchestrator content. Its real
        // settlement refreshes that hold after this source choice, so only
        // an acknowledgement of the changed entry can release it.
        writer.take(HeldFence::Durability).finish(Ok(()));
    }
    let write_ahead = writer.take(HeldFence::WriteAhead);
    turn.record(|record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert!(!delivery.write_ahead_acknowledged);
        assert!(delivery.turn_generation.is_none() && delivery.entry.is_none());
    });
    turn.edit(|record| record.prompt_deliveries.get_mut(&id).unwrap().write_ahead_acknowledged = true);
    let started = turn.state.start_next_queued_turn_off_lock(&turn.session, false, false)
        .unwrap().expect("acknowledged receipted queue entry promotes");
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert_eq!(delivery.turn_generation, Some(started.dispatch.active_turn_generation()));
        let entry = delivery.entry.as_ref().unwrap();
        assert_eq!(entry.pending_prompt.id, id);
        assert_eq!(entry.pending_prompt.text, "Prepared ordinary input");
        assert_eq!(entry.source, source);
        assert!(entry.engram_evaluate.is_none() && entry.engram_bind.is_none());
        assert!(delivery.message_id.is_some() && delivery.attempt_key.is_some());
    });
    turn.assert_no_prompt();
    (started.dispatch, id, writer, write_ahead)
}

#[test]
fn ordinary_handoff_waits_for_real_stop_failure_or_success() {
    for failed_stop in [true, false] {
        let turn = OrdinaryRoot::new("ordinary-stop-handoff");
    let state = turn.state.clone();
    let session = turn.session.clone();
    let root_rx = &turn.receiver;
        let dispatch = prepare_ordinary_owned_head(&turn);
        let active_turn_generation = dispatch.active_turn_generation();
        // The real ordinary promotion is complete; provider handoff has not run.
        let (runtime, receiver) = test_acp_runtime_handle(AcpAgent::OpenCode, "post-receipt-stop");
        let token = RuntimeToken::Acp(runtime.runtime_id.clone());
        let process = runtime.process.clone();
        let sender = runtime.input_tx.clone();
        let lifecycle = runtime.turn_lifecycle.clone();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            assert!(record.queued_prompts[0].engram_ordinary_handoff);
            record.runtime = SessionRuntime::Acp(runtime);
        }
        let failure =
            failed_stop.then(|| force_test_kill_child_process_failure(&process, "OpenCode"));
        if !failed_stop {
            turn.enqueue("successor prompt", QueuedPromptSource::User);
        }
        let stop_gate = install_test_stop_fence_gate(&state, &session);
        std::thread::scope(|scope| {
            let stop = scope.spawn(|| state.stop_session(&session));
            stop_gate.wait_until_claimed();
            handoff_prepared_turn_dispatch(
                &state,
                TurnDispatch::PersistentAcp {
                    human_prompt_id: None,
                    federation_handoff: None,
                    opening_prompt: None,
                    active_turn_generation,
                    runtime_token: token.clone(),
                    session_id: session.clone(),
                    mailbox_notification: None,
                    sender,
                    turn_lifecycle: lifecycle.clone(),
                    command: AcpPromptCommand {
                        cwd: String::new(),
                        cursor_mode: None,
                        model: String::new(),
                        opencode_effort: None,
                        opencode_mode: None,
                        prompt: "exact ordinary prompt".into(),
                        resume_session_id: None,
                    },
                },
            )
            .unwrap();
            assert!(
                receiver.try_recv().is_err(),
                "handoff must defer while Stop owns runtime"
            );
            let callbacks = {
                let inner = state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .deferred_stop_callbacks
                    .clone()
            };
            assert!(matches!(
                callbacks.as_slice(),
                [DeferredStopCallback::TurnHandoff { .. }]
            ));
            stop_gate.release();
            let result = stop.join().unwrap();
            if failed_stop {
                let Err(error) = result else {
                    panic!("forced Stop failure must surface");
                };
                assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
            } else {
                result.unwrap();
            }
            // Replay a cloned callback list too: delivery is one-shot and still
            // runtime/turn fenced after a successful Stop.
            state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
        });
        drop(failure);
        let prompts = receiver
            .try_iter()
            .filter_map(|command| match command {
                AcpRuntimeCommand::Prompt(command) => Some(command.prompt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            prompts,
            if failed_stop {
                vec!["exact ordinary prompt".to_owned()]
            } else {
                vec![]
            }
        );
        assert!(root_rx.try_recv().is_err());
        if !failed_stop {
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.queued_prompts.len(), 1);
                assert_eq!(
                    record.queued_prompts[0].pending_prompt.text,
                    "successor prompt"
                );
                assert!(record.queued_prompts[0].engram_bind.is_none());
                assert!(record.queued_prompts[0].engram_evaluate.is_none());
            }
            let durable: PersistedSessionRecord =
                serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
            assert_eq!(durable.queued_prompts.len(), 1);
            assert_eq!(
                durable.queued_prompts[0].pending_prompt.text,
                "successor prompt"
            );
            state.resume_session_queue(&session).unwrap();
            let CodexRuntimeCommand::Prompt { command, .. } = root_rx.try_recv().unwrap() else {
                panic!("successor should reach provider");
            };
            assert!(command.prompt.contains("successor prompt"));
            assert!(!command.prompt.contains("Prepared ordinary input"));
            assert!(root_rx.try_recv().is_err());
        }
        turn.assert_no_control();
    }
}


#[tokio::test]
async fn unresolved_fast_ordinary_handoff_obeys_public_stop_outcome() {
    for failed_stop in [true, false] {
        let mut turn = OrdinaryRoot::new("ordinary-stop-handoff");
        let (runtime, receiver) = test_codex_runtime_handle("fast-stop-runtime");
        let process = runtime.process.clone();
        let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
        {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.session.model = "catalog-model".into();
            record.session.codex_fast_mode = true;
            record.session.model_options.clear();
            record.runtime = SessionRuntime::Codex(runtime);
        }
        let (dispatch, prompt_id, _writer, _write_ahead) =
            prepare_receipted_ordinary_promotion(&mut turn, QueuedPromptSource::Orchestrator);
        let state = turn.state.clone();
        let session = turn.session.clone();
        turn.enqueue("ordinary orchestrator successor", QueuedPromptSource::Orchestrator);
        let failure = failed_stop.then(|| force_test_kill_child_process_failure(&process, "Codex"));
        let stop_gate = install_test_stop_fence_gate(&state, &session);
        let mut events = state.subscribe_events();
        let response = state.request_stop_session(&session).unwrap();
        assert_eq!(
            response
                .sessions
                .iter()
                .find(|candidate| candidate.id == session)
                .unwrap()
                .status,
            SessionStatus::Stopping
        );
        stop_gate.wait_until_claimed();

        handoff_prepared_turn_dispatch(&state, dispatch).unwrap();
        let callbacks = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.queued_prompts.is_empty(), "Stop drops the Orchestrator successor");
            let receipt = &record.prompt_deliveries[&prompt_id];
            assert_eq!(receipt.disposition, PromptDeliveryDisposition::HandingOff);
            assert_eq!(receipt.turn_generation, Some(record.active_turn_generation));
            assert_eq!(receipt.entry.as_ref().unwrap().pending_prompt.text, "Prepared ordinary input");
            assert_eq!(receipt.entry.as_ref().unwrap().source, QueuedPromptSource::Orchestrator);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            assert!(matches!(
                record.deferred_stop_callbacks.as_slice(),
                [DeferredStopCallback::TurnHandoff { .. }]
            ));
            record.deferred_stop_callbacks.clone()
        };
        stop_gate.release();

        if failed_stop {
            let CodexRuntimeCommand::RefreshModelList { response_tx } =
                phase_sync::receive(&receiver, "failed Stop replays unresolved Fast discovery")
            else {
                panic!("discovery must precede the provider prompt");
            };
            response_tx
                .send(Ok(codex_model_options(&json!({ "data": [{
                    "model": "catalog-model",
                    "serviceTiers": [{ "id": "Turbo-EXACT", "name": "Fast" }]
                }] }))))
                .unwrap();
            let CodexRuntimeCommand::Prompt { command, .. } =
                phase_sync::receive(&receiver, "failed Stop replays one Fast provider handoff")
            else {
                panic!("resolved Fast dispatch should reach the provider");
            };
            assert_eq!(command.prompt, "Prepared ordinary input");
            assert_eq!(command.model, "catalog-model");
            assert_eq!(command.service_tier.as_deref(), Some("Turbo-EXACT"));
            state.replay_deferred_runtime_stop_callbacks(&session, &runtime_token, callbacks);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(record.queued_prompts.is_empty());
            assert_eq!(record.prompt_deliveries[&prompt_id].entry.as_ref().unwrap().pending_prompt.id, prompt_id);
            assert!(ordinary_queue_position(record).is_none());
        } else {
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
                loop {
                    {
                        let inner = state.inner.lock().unwrap();
                        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                        if !record.runtime_stop_in_progress {
                            assert_eq!(record.session.status, SessionStatus::Idle);
                            assert!(record.queued_prompts.is_empty());
                            break;
                        }
                    }
                    events.recv().await.unwrap();
                }
            })
            .await
            .expect("successful public Stop should settle");
            state.replay_deferred_runtime_stop_callbacks(&session, &runtime_token, callbacks);
            assert!(receiver.try_recv().is_err());
        }
        drop(failure);
        turn.assert_no_control();
    }
}


#[tokio::test]
async fn public_failed_stop_restores_ordinary_handoff_before_replay() {
    for orchestrator_source in [false, true] {
        assert_public_failed_stop_restores_ordinary_handoff(true, orchestrator_source).await;
    }
}

#[tokio::test]
async fn public_failed_stop_restores_ordinary_owner_before_handoff_arrives() {
    for orchestrator_source in [false, true] {
        assert_public_failed_stop_restores_ordinary_handoff(false, orchestrator_source).await;
    }
}

async fn assert_public_failed_stop_restores_ordinary_handoff(
    callback_first: bool,
    orchestrator_source: bool,
) {
    let mut turn = OrdinaryRoot::new("ordinary-stop-handoff");
    let source = if orchestrator_source { QueuedPromptSource::Orchestrator } else { QueuedPromptSource::User };
    let (dispatch, prompt_id, _writer, _write_ahead) = prepare_receipted_ordinary_promotion(&mut turn, source);
    let state = turn.state.clone();
    let session = turn.session.clone();
    let root_rx = &turn.receiver;
    let active_turn_generation = dispatch.active_turn_generation();
    let receipt_before_stop = turn.record(|record| serde_json::to_value(&record.prompt_deliveries[&prompt_id]).unwrap());
    let (runtime, receiver) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "public-post-receipt-stop");
    let token = RuntimeToken::Acp(runtime.runtime_id.clone());
    let process = runtime.process.clone();
    let sender = runtime.input_tx.clone();
    let lifecycle = runtime.turn_lifecycle.clone();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }
    let _failure = force_test_kill_child_process_failure(&process, "OpenCode");
    let gate = install_test_stop_fence_gate(&state, &session);
    let mut events = state.subscribe_events();
    let response = state.request_stop_session(&session).unwrap();
    assert_eq!(
        response
            .sessions
            .iter()
            .find(|s| s.id == session)
            .unwrap()
            .status,
        SessionStatus::Stopping
    );
    gate.wait_until_claimed();
    if !callback_first {
        gate.release();
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            loop {
                {
                    let inner = state.inner.lock().unwrap();
                    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    if !record.runtime_stop_in_progress {
                        assert_eq!(record.session.status, SessionStatus::Active);
                        assert!(record.queued_prompts.is_empty());
                        assert_eq!(serde_json::to_value(&record.prompt_deliveries[&prompt_id]).unwrap(), receipt_before_stop);
                        assert!(record.deferred_stop_callbacks.is_empty());
                        break;
                    }
                }
                events.recv().await.unwrap();
            }
        })
        .await
        .expect("public Stop must restore owner before handoff arrival");
    }
    handoff_prepared_turn_dispatch(
        &state,
        TurnDispatch::PersistentAcp {
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
                prompt: "public ordinary prompt".into(),
                resume_session_id: None,
            },
        },
    )
    .unwrap();
    let callbacks = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .deferred_stop_callbacks
            .clone()
    };
    if callback_first {
        assert!(receiver.try_recv().is_err());
        gate.release();
    }
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                if !record.runtime_stop_in_progress {
                    assert_eq!(record.session.status, SessionStatus::Active);
                    assert!(record.runtime.matches_runtime_token(&token));
                    assert!(record.session.messages.iter().any(|message| matches!(message,
                        Message::Text { text, .. } if text.contains("Stop failed in the background"))));
                    break;
                }
                assert_ne!(record.session.status, SessionStatus::Error, "public Stop must not discard its ordinary handoff");
            }
            events.recv().await.unwrap();
        }
    }).await.expect("public Stop rollback did not release handoff");
    let prompt = loop {
        if let AcpRuntimeCommand::Prompt(command) = phase_sync::receive(&receiver, "failed Stop releases the original ordinary handoff") {
            break command.prompt;
        }
    };
    assert_eq!(prompt, "public ordinary prompt");
    state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
    assert!(!receiver.try_iter().any(|command| matches!(command, AcpRuntimeCommand::Prompt(_))), "cloned callbacks cannot repeat the handoff");
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert_eq!(serde_json::to_value(&record.prompt_deliveries[&prompt_id]).unwrap(), receipt_before_stop);
        assert_eq!(record.prompt_deliveries[&prompt_id].entry.as_ref().unwrap().source, source);
        assert!(ordinary_queue_position(record).is_none());
    });
    assert!(root_rx.try_recv().is_err());
    turn.assert_no_control();
}


#[tokio::test]
async fn successful_public_stop_never_retires_a_replacement_for_its_orchestrator_owner() {
    let turn = OrdinaryRoot::new("ordinary-stop-handoff");
    let state = turn.state.clone();
    let session = turn.session.clone();
    let root_rx = &turn.receiver;
    let dispatch = prepare_ordinary_owned_head(&turn);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].queued_prompts[0].source = QueuedPromptSource::Orchestrator;
        sync_pending_prompts(&mut inner.sessions[index]);
    }
    turn.enqueue("replacement user successor", QueuedPromptSource::User);
    let (runtime, _receiver) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "public-stop-owner-fence");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }
    let gate = install_test_stop_fence_gate(&state, &session);
    let mut events = state.subscribe_events();
    state.request_stop_session(&session).unwrap();
    gate.wait_until_claimed();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            record.queued_prompts[0].source,
            QueuedPromptSource::Orchestrator
        );
        assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::User);
        record.queued_prompts.pop_front();
        sync_pending_prompts(record);
    }
    gate.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                if !record.runtime_stop_in_progress {
                    assert_eq!(record.session.status, SessionStatus::Idle);
                    assert_eq!(record.queued_prompts.len(), 1);
                    assert_eq!(
                        record.queued_prompts[0].pending_prompt.text,
                        "replacement user successor"
                    );
                    break;
                }
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("successful Stop should settle without consuming a replacement owner");
    assert!(root_rx.try_recv().is_err());
    turn.assert_no_control();
}

/// A successful ordinary Stop whose drain meets a pending receipted
/// successor does not begin its write-ahead inside the Stop's transaction;
/// the Stop publishes its own message once, and the drain it wakes after its
/// commit begins the write-ahead, whose acknowledgement hands the
/// successor off exactly once, with no further input.

#[test]
fn an_ordinary_stop_wakes_its_receipted_successor_after_its_own_commit() {
    use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
    // The ordinary Stop must commit before its separately woken drain starts WAL.
    let turn = OrdinaryRoot::new("ordinary-stop-handoff");
    let mut state = turn.state.clone();
    let session = turn.session.clone();
    let receiver = &turn.receiver;
    // Hands the test the receipt's durability and write-ahead fences.
    let writer = DeliveryWriter::attach(&mut state);
    let prompt_id = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: "the receipted ordinary successor".to_owned(),
            expanded_text: None,
            source: None,
        };
        let id = prompt.id.clone();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram.historical_control.insert("engramRoutingToken".into(), json!("old-token"));
        // Exercise Stop's missing-runtime recovery of a running session; a
        // busy session's durability acknowledgement dispatches nothing.
        record.session.status = SessionStatus::Active;
        queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
        state.commit_locked(&mut inner).unwrap();
        id
    };
    assert!(state.request_queued_prompt_durability(&session, &prompt_id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let acknowledged = phase_sync::PollGuard::new();
    loop {
        let done = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index]
                .queued_prompt_durability_holds
                .get(&prompt_id)
                .is_some_and(|hold| hold.acknowledged)
        };
        if done {
            break;
        }
        acknowledged.wait("the successor's durability acknowledgement");
    }
    let mut delta_events = state.subscribe_delta_events();
    state
        .stop_session_with_options(
            &session,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: true,
                pause_automatic_resumes_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .unwrap();
    // The last commit so far is the write-ahead's; its acknowledgement is
    // still held, so nothing commits after it yet.
    let write_ahead_revision = state.snapshot().revision;
    let mut stop_revisions = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        if let DeltaEvent::MessageCreated {
            revision,
            message: Message::Text { text, .. },
            ..
        } = event
            && text == "Turn stopped by user."
        {
            stop_revisions.push(revision);
        }
    }
    assert_eq!(stop_revisions.len(), 1, "the Stop's message is published once");
    assert!(
        stop_revisions[0] < write_ahead_revision,
        "the Stop committed and published before the write-ahead began: {stop_revisions:?} vs {write_ahead_revision}"
    );
    let write_ahead = writer.take(HeldFence::WriteAhead);
    assert!(receiver.try_recv().is_err(), "nothing before the acknowledgement");
    write_ahead.finish(Ok(()));
    match recv_within_guard(&receiver, "the woken successor's hand-off") {
        Ok(CodexRuntimeCommand::Prompt { .. }) => {}
        Ok(_) => panic!("the runtime got another command first"),
        Err(error) => panic!(
            "the successor was not handed off: {error:?}; Engram saw {:?}",
            turn.transport.requests()
        ),
    }
    let promoted = phase_sync::PollGuard::new();
    loop {
        let done = {
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].prompt_deliveries[&prompt_id].turn_generation.is_some()
        };
        if done {
            break;
        }
        promoted.wait("the successor's promotion");
    }
    assert!(
        !receiver.try_iter().any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. })),
        "handed off once"
    );
    turn.assert_no_control();
}

/// A Stop of a session whose receipted head was promoted for ordinary dispatch and
/// never handed off retires that head with its receipt rejected, in the
/// Stop's own retirement write (prompt_delivery.rs), and a restart reads that
/// terminal receipt back: none is left handing off for a prompt that provably
/// never reached the runtime.

#[tokio::test]
async fn a_stop_retiring_a_promoted_receipted_head_rejects_its_receipt_durably() {
    let turn = OrdinaryRoot::new("ordinary-stop-handoff");
    let state = turn.state.clone();
    let session = turn.session.clone();
    let root_rx = &turn.receiver;
    let dispatch = prepare_ordinary_owned_head(&turn);
    let turn_generation = dispatch.active_turn_generation();
    // The ordinary head as a receipted one: handing off under this turn, its
    // write-ahead acknowledged, the prompt not yet sent.
    let prompt_id = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        let head = record.queued_prompts[0].pending_prompt.id.clone();
        record.prompt_deliveries.insert(
            head.clone(),
            PromptDelivery {
                federation_operation: None,
                federation_handoff_started: false,
                disposition: PromptDeliveryDisposition::HandingOff,
                attempt: 1,
                write_ahead: "ordinary-write-ahead".to_owned(),
                sequence: 1,
                message_id: None,
                turn_generation: Some(turn_generation),
                attempt_key: None,
                reason: None,
                entry: None,
                write_ahead_acknowledged: true,
                resume: None,
                runtime_write: PromptRuntimeWrite::Untracked,
            },
        );
        head
    };
    let (runtime, _receiver) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "receipted-stop-retirement");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }
    let mut events = state.subscribe_events();
    state.request_stop_session(&session).unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                if !record.runtime_stop_in_progress
                    && record.session.status != SessionStatus::Stopping
                {
                    break;
                }
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("the Stop settles");
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty(), "the Stop retired its promoted head");
        let delivery = &record.prompt_deliveries[&prompt_id];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::Rejected);
        assert_eq!(delivery.turn_generation, Some(turn_generation));
        let reloaded = PersistedSessionRecord::from_record(record)
            .into_record()
            .expect("the row loads");
        assert_eq!(
            reloaded.prompt_deliveries[&prompt_id].disposition,
            PromptDeliveryDisposition::Rejected,
            "the terminal receipt survives a restart"
        );
    }
    drop(dispatch);
    assert!(root_rx.try_recv().is_err(), "never handed off");
    let durable: PersistedSessionRecord = serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(!durable.queued_prompts.iter().any(|row| row.pending_prompt.id == prompt_id));
    assert_eq!(durable.prompt_deliveries[&prompt_id].disposition, PromptDeliveryDisposition::Rejected);
    turn.assert_no_control();

}

#[test]
fn ordinary_send_commit_failure_never_reaches_the_provider() {
    let mut turn = OrdinaryRoot::new("ordinary-send-commit-refusal");
    turn.state.shutdown_persist_blocking();
    let failing = turn.root.join("persistence-is-directory");
    fs::create_dir_all(&failing).unwrap();
    turn.state.persistence_path = Arc::new(failing);
    let error = dispatch_turn_and_snapshot(&turn.state, &turn.session, SendMessageRequest {
        text: "Uncommitted ordinary input.".into(), expanded_text: None,
        attachments: Vec::new(), source_session_id: None, source_mailbox: None,
    }).err().expect("the real dispatch commit must refuse the unusable store");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_orphan_recovery_keeps_original_ids_and_source_order_on_commit_failure() {
    for mailbox_first in [false, true] {
        let mut turn = OrdinaryRoot::new("ordinary-orphan-commit-refusal");
        if mailbox_first { turn.enqueue("Mailbox before workflow.", QueuedPromptSource::Mailbox); }
        turn.enqueue("Original workflow.", QueuedPromptSource::Orchestrator);
        let before = turn.record(|record| record.queued_prompts.iter()
            .map(|row| (row.pending_prompt.id.clone(), row.pending_prompt.text.clone(), row.source))
            .collect::<Vec<_>>());
        turn.state.shutdown_persist_blocking();
        let failing = turn.root.join("orphan-store-is-directory");
        fs::create_dir_all(&failing).unwrap();
        turn.state.persistence_path = Arc::new(failing);
        turn.state.dispatch_orphaned_workflow_prompts();
        turn.record(|record| assert_eq!(record.queued_prompts.iter()
            .map(|row| (row.pending_prompt.id.clone(), row.pending_prompt.text.clone(), row.source))
            .collect::<Vec<_>>(), before));
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn stale_ordinary_rejection_cannot_terminalize_a_replacement_runtime_turn_or_stop_owner() {
    for change in ["runtime", "turn", "stop"] {
        let turn = OrdinaryRoot::new("ordinary-rejected-owner-drift");
        let dispatch = prepare_ordinary_owned_head(&turn);
        let token = dispatch.runtime_token().clone();
        let generation = dispatch.active_turn_generation();
        let (runtime, replacement_rx) = test_codex_runtime_handle("ordinary-rejection-replacement");
        turn.edit(|record| match change {
            "runtime" => record.runtime = SessionRuntime::Codex(runtime),
            "turn" => record.active_turn_generation += 1,
            _ => { record.claim_runtime_stop(RuntimeStopOwnerKind::LostRuntimeTerminalization, token.clone()); },
        });
        let before = turn.saved();
        assert!(!record_rejected_turn_dispatch(&turn.state, &turn.session,
            "stale provider channel rejection", None, &token, generation));
        assert_eq!(turn.saved(), before, "{change}: the newer owner stays untouched");
        drop(dispatch);
        turn.assert_no_prompt();
        assert!(replacement_rx.try_recv().is_err());
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_channel_rejection_preserves_history_and_only_the_fresh_successor_can_run() {
    let turn = OrdinaryRoot::new("ordinary-closed-provider-channel");
    let (runtime, closed) = test_codex_runtime_handle("ordinary-channel-rejection");
    turn.edit(|record| record.runtime = SessionRuntime::Codex(runtime));
    let dispatch = prepare_ordinary_owned_head(&turn);
    let next = turn.enqueue("Successor after channel rejection.", QueuedPromptSource::User);
    drop(closed);
    let error = deliver_turn_dispatch_now(&turn.state, dispatch).expect_err("the actual provider channel is closed");
    assert!(error.message.contains("failed to queue prompt for Codex"), "{}", error.message);
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert!(record.queued_prompts.iter().any(|row| row.pending_prompt.id == next && row.pending_prompt.text == "Successor after channel rejection."));
        let old = record.queued_prompts.iter().find(|row| row.pending_prompt.text == "Prepared ordinary input").unwrap();
        assert!(old.engram_ordinary_handoff, "the saved original is not a runnable successor");
    });
    let durable: PersistedSessionRecord = serde_json::from_str(&persisted_session_json(&turn.state, &turn.session)).unwrap();
    assert!(durable.queued_prompts.iter().any(|row| row.pending_prompt.id == next));
    let (runtime, receiver) = test_codex_runtime_handle("ordinary-channel-successor");
    turn.edit(|record| {
        record.runtime = SessionRuntime::Codex(runtime);
        record.session.status = SessionStatus::Idle;
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = phase_sync::receive(&receiver, "the ordinary successor") else {
        panic!("only the successor may reach the replacement runtime");
    };
    assert_eq!(command.prompt, "Successor after channel rejection.");
    assert!(!receiver.try_iter().any(|command| matches!(command, CodexRuntimeCommand::Prompt { .. })));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_channel_rejection_keeps_the_terminal_owner_visible_when_its_commit_fails() {
    let mut turn = OrdinaryRoot::new("ordinary-rejection-failed-commit");
    let (runtime, closed) = test_codex_runtime_handle("ordinary-rejection-failed-store");
    turn.edit(|record| record.runtime = SessionRuntime::Codex(runtime));
    let dispatch = prepare_ordinary_owned_head(&turn);
    let next = turn.enqueue("Original successor id.", QueuedPromptSource::User);
    drop(closed);
    turn.state.shutdown_persist_blocking();
    let failing = turn.root.join("rejected-owner-store-is-directory");
    fs::create_dir_all(&failing).unwrap();
    turn.state.persistence_path = Arc::new(failing);
    let error = handoff_prepared_turn_dispatch(&turn.state, dispatch).unwrap_err();
    assert!(error.message.contains("failed to queue prompt for Codex"));
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(matches!(record.runtime, SessionRuntime::None), "a dead runtime cannot be restored after a failed commit");
        assert!(record.queued_prompts.iter().any(|row| row.pending_prompt.id == next));
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_fast_discovery_failure_keeps_the_successor_and_cannot_replay_the_failed_turn() {
    let turn = OrdinaryRoot::new("ordinary-fast-discovery-rejection");
    turn.edit(|record| {
        record.session.model = "catalog-model".into();
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
    });
    let dispatch = prepare_ordinary_owned_head(&turn);
    let next = turn.enqueue("Successor after discovery failure.", QueuedPromptSource::User);
    let state = turn.state.clone();
    let worker = std::thread::spawn(move || deliver_turn_dispatch_now(&state, dispatch));
    let CodexRuntimeCommand::RefreshModelList { response_tx } = phase_sync::receive(&turn.receiver, "actual Fast discovery") else {
        panic!("discovery precedes the provider prompt");
    };
    response_tx.send(Err("forced catalog failure".into())).unwrap();
    let error = worker.join().unwrap().expect_err("discovery failure must surface");
    assert!(error.message.contains("failed to resolve Codex Fast"));
    turn.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.queued_prompts.iter().any(|row| row.pending_prompt.id == next));
        assert!(record.queued_prompts[0].engram_ordinary_handoff);
    });
    let durable: PersistedSessionRecord = serde_json::from_str(&persisted_session_json(&turn.state, &turn.session)).unwrap();
    assert!(durable.queued_prompts.iter().any(|row| row.pending_prompt.id == next));
    turn.edit(|record| {
        record.session.codex_fast_mode = false;
        record.session.status = SessionStatus::Idle;
    });
    turn.state.resume_session_queue(&turn.session).unwrap();
    assert_eq!(turn.prompt().prompt, "Successor after discovery failure.");
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_child_initial_and_followup_turns_survive_polling_without_control_authorization() {
    let turn = OrdinaryRoot::new("ordinary-child-initial-followup");
    let response = turn.state.create_read_only_delegation(&turn.session, CreateDelegationRequest {
        prompt: "Initial child input.".into(), title: None, cwd: None, agent: Some(Agent::Codex),
        model: None, mode: Some(DelegationMode::Explorer), write_policy: Some(DelegationWritePolicy::ReadOnly),
    }).unwrap();
    let child = response.delegation.child_session_id;
    let delegation = response.delegation.id;
    let CodexRuntimeCommand::Prompt { session_id, command } = phase_sync::receive(&turn.receiver, "initial ordinary child handoff") else { panic!("child prompt"); };
    assert_eq!(session_id, child);
    assert!(command.prompt.contains("Initial child input."));
    let token = {
        let inner = turn.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&child).unwrap()].runtime.runtime_token().unwrap()
    };
    finish_delegation_child_with_assistant_text(&turn.state, &child, "Initial result.");
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.session_mut(&child).unwrap().session.status = SessionStatus::Active;
    }
    turn.state.finish_turn_ok_if_runtime_matches(&child, &token).unwrap();
    turn.state.refresh_delegation_for_child_session(&child).unwrap();
    turn.state.followup_delegation(&turn.session, &delegation, "Follow-up child input.".into()).unwrap();
    for _ in 0..3 { turn.state.refresh_delegation_for_child_session(&child).unwrap(); }
    let CodexRuntimeCommand::Prompt { session_id, command } = phase_sync::receive(&turn.receiver, "ordinary child followup") else { panic!("child followup"); };
    assert_eq!(session_id, child);
    assert!(command.prompt.contains("Follow-up child input."));
    turn.assert_no_prompt();
    let inner = turn.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(record.queued_prompts.is_empty());
    assert_eq!(record.session.messages.iter().filter(|message| matches!(message,
        Message::Text { text, .. } if text == "Follow-up child input.")).count(), 1);
    drop(inner);
    turn.assert_no_control();
}

#[test]
fn passive_unknown_authorization_keeps_exact_root_and_child_bytes_cancelable_without_replay() {
    for child in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-passive-authority-cancel");
        let session = if child {
            let response = turn.state.create_read_only_delegation(&turn.session, CreateDelegationRequest {
                prompt: "Ordinary child setup.".into(), title: None, cwd: None, agent: Some(Agent::Codex),
                model: None, mode: Some(DelegationMode::Explorer), write_policy: Some(DelegationWritePolicy::ReadOnly),
            }).unwrap();
            let child = response.delegation.child_session_id;
            let CodexRuntimeCommand::Prompt { session_id, .. } = phase_sync::receive(&turn.receiver, "ordinary child setup") else { panic!("child prompt"); };
            assert_eq!(session_id, child);
            let token = {
                let inner = turn.state.inner.lock().unwrap();
                inner.sessions[inner.find_session_index(&child).unwrap()].runtime.runtime_token().unwrap()
            };
            turn.state.finish_turn_ok_if_runtime_matches(&child, &token).unwrap();
            child
        } else { turn.session.clone() };
        let (id, expected) = {
            let mut inner = turn.state.inner.lock().unwrap();
            let id = inner.next_message_id();
            let record = inner.session_mut(&session).unwrap();
            record.set_auto_dispatch_blocked(true);
            queue_prompt_on_record_with_source(record, PendingPrompt {
                engram_interrupted: false, is_engram_retained: false, attachments: Vec::new(),
                id: id.clone(), timestamp: stamp_now(), text: "Original unknown provider attempt.".into(),
                expanded_text: None, source: None,
            }, Vec::new(), QueuedPromptSource::User);
            let row = record.queued_prompts.back_mut().unwrap();
            row.engram_bind = Some(json!({"request":{"idempotency_key":"old-key"},"connection":{"session":"original"}}));
            row.engram_ordinary_handoff = true;
            row.promotion_disposition_known = false;
            let expected = serde_json::to_value(&*row).unwrap();
            turn.state.commit_locked(&mut inner).unwrap();
            (id, expected)
        };
        {
            let mut inner = turn.state.inner.lock().unwrap();
            inner.projects.iter_mut().find(|project| project.id == turn.project).unwrap()
                .engram.as_mut().unwrap().binary_path = Some("changed-work-service".into());
            turn.state.commit_locked(&mut inner).unwrap();
        }
        if child { turn.state.refresh_delegation_for_child_session(&session).unwrap(); }
        turn.state.resume_session_queue(&session).unwrap();
        let inner = turn.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), expected);
        assert!(ordinary_queue_position(record).is_none(), "no old grant may become an executor");
        drop(inner);
        turn.assert_no_prompt();
        turn.assert_no_control();
        turn.state.cancel_queued_prompt(&session, &id).unwrap();
        let inner = turn.state.inner.lock().unwrap();
        assert!(!inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts.iter().any(|row| row.pending_prompt.id == id));
    }
}

#[test]
fn prepared_mailbox_history_deduplicates_equal_boundary_and_keeps_a_newer_wake_separate() {
    for newer in [false, true] {
        let mut turn = OrdinaryRoot::new("ordinary-mailbox-retained-boundary");
        let coordination = resolve_coordination_persistence_path(turn.state.persistence_path.as_ref());
        turn.state.mailbox_store = Arc::new(MailboxStore::open(&coordination).unwrap());
        let sender = test_session_id(&turn.state, Agent::Claude);
        turn.edit(|record| record.session.status = SessionStatus::Active);
        let send = |key: &str| turn.state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
            target_session_id: turn.session.clone(), message: key.into(), idempotency_key: key.into(),
            topic: None, state_stamp: None, class: Some("routine".into()),
        }).unwrap();
        send("ordinary-retained-one");
        turn.edit(|record| {
            record.session.status = SessionStatus::Idle;
            record.queued_prompts[0].engram_evaluate = Some(json!({"historical":"original-mailbox-boundary"}));
        });
        let dispatch = turn.state.start_next_queued_turn_off_lock(&turn.session, false, false).unwrap().unwrap().dispatch;
        let notification = dispatch.mailbox_notification().unwrap().clone();
        let before = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
        for _ in 0..2 {
            turn.state.requeue_rejected_mailbox_notification(&notification).unwrap();
            turn.record(|record| {
                assert_eq!(record.queued_prompts.len(), 1);
                assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), before);
            });
        }
        if newer {
            send("ordinary-retained-two");
            turn.record(|record| {
                assert_eq!(record.queued_prompts.len(), 2);
                assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), before);
                assert!(record.queued_prompts[1].pending_prompt.source.as_ref().unwrap().mailbox.as_ref().unwrap().sequence > notification.through_sequence);
            });
        }
        // Sending the original dispatch completes its original boundary only.
        deliver_turn_dispatch(&turn.state, dispatch).unwrap();
        turn.prompt();
        turn.assert_no_prompt();
        turn.record(|record| {
            assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), before);
            assert_eq!(record.queued_prompts.len(), 1 + usize::from(newer));
        });
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        if newer {
            let command = turn.prompt();
            assert!(command.prompt.contains(&notification.mailbox_id));
            turn.record(|record| {
                assert_eq!(record.queued_prompts.len(), 1, "only the passive original row remains");
                assert_eq!(serde_json::to_value(&record.queued_prompts[0]).unwrap(), before);
                assert!(record.session.messages.iter().any(|message| matches!(message,
                    Message::Text { source: Some(source), .. }
                        if source.mailbox.as_ref().is_some_and(|mailbox|
                            mailbox.mailbox_id == notification.mailbox_id && mailbox.sequence > notification.through_sequence))));
            });
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

#[test]
fn fresh_ordinary_prompt_preserves_existing_message_index_allocation() {
    let turn = OrdinaryRoot::new("ordinary-fresh-message-index");
    let capacity = turn.edit(|record| {
        record.message_positions.reserve(4096);
        assert!(cached_message_index_on_record(record, "fresh-missing-id").is_none());
        record.message_positions.capacity()
    });
    turn.record(|record| {
        assert!(cached_message_index_on_record(record, "another-fresh-id").is_none());
        assert_eq!(record.message_positions.capacity(), capacity);
    });
    let _dispatch = turn.prepare("New ordinary prompt.");
    turn.record(|record| {
        for (id, index) in &record.message_positions {
            assert_eq!(record.session.messages[*index].id(), id);
        }
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

// Reading-backed port of the upstream Stop-retirement delta witness. The
// checkpoint callback no longer exists. This inspects the ordinary Stop's
// post-shutdown delta; a later Stop commit also stamps, so it does not isolate
// the retirement stamp or prove physical SQLite fencing.
#[tokio::test]
async fn a_stop_retirement_is_in_the_ordinary_post_shutdown_delta() {
    let mut turn = OrdinaryRoot::new("ordinary-stop-retirement-delta");
    let session = turn.session.clone();
    let (tx, persist_rx) = mpsc::channel();
    turn.state.persist_tx = tx.clone();
    let state = turn.state.clone();
    let writer = std::thread::spawn(move || {
        while let Ok(request) = persist_rx.recv() {
            match request {
                PersistRequest::Fence(fence) => fence.finish(Ok(())),
                PersistRequest::Shutdown => break,
                _ => {},
            }
        }
    });
    let dispatch = prepare_ordinary_owned_head(&turn);
    let turn_generation = dispatch.active_turn_generation();
    let prompt_id = {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&session).unwrap();
        let id = record.queued_prompts[0].pending_prompt.id.clone();
        record.prompt_deliveries.insert(id.clone(), PromptDelivery {
            federation_operation: None, federation_handoff_started: false,
            disposition: PromptDeliveryDisposition::HandingOff, attempt: 1,
            write_ahead: "ordinary-write-ahead".into(), sequence: 1, message_id: None,
            turn_generation: Some(turn_generation), attempt_key: None, reason: None,
            entry: None, write_ahead_acknowledged: true, resume: None,
            runtime_write: PromptRuntimeWrite::Untracked,
        });
        id
    };
    let (runtime, receiver) = test_acp_runtime_handle(AcpAgent::OpenCode, "ordinary-stop-delta");
    let lifecycle = runtime.turn_lifecycle.clone();
    *lifecycle.0.lock().unwrap() = true;
    let watermark = Arc::new(Mutex::new(None::<u64>));
    let (held_tx, held_rx) = mpsc::channel();
    let watcher = {
        let observed = state.clone();
        let watermark = watermark.clone();
        std::thread::spawn(move || {
            let (active_lock, settled) = &*lifecycle;
            let mut active = active_lock.lock().unwrap();
            held_tx.send(()).unwrap();
            assert!(matches!(receiver.recv_timeout(phase_sync::DEADLOCK_GUARD), Ok(AcpRuntimeCommand::Cancel)),
                "the real Stop cancels its runtime before retirement");
            *watermark.lock().unwrap() = Some(observed.inner.lock().unwrap().last_mutation_stamp);
            *active = false;
            settled.notify_all();
        })
    };
    phase_sync::receive(&held_rx, "the runtime shutdown window is held");
    {
        let mut inner = state.inner.lock().unwrap();
        inner.session_mut(&session).unwrap().runtime = SessionRuntime::Acp(runtime);
    }
    let options = StopSessionOptions::default();
    let (_, claim) = state.begin_requested_stop_session(&session, &options).unwrap();
    let claim = claim.expect("Stop owns the actual runtime");
    state.stop_local_session_with_options(&session, options, Some(claim)).unwrap();
    watcher.join().unwrap();
    let watermark = watermark.lock().unwrap().expect("shutdown recorded the delta watermark");
    let delta = collect_persist_delta_from_shared_state(&state.inner, watermark);
    let selected = delta.changed_sessions.into_iter().find(|record| record.session.id == session)
        .expect("the retirement image is selected after shutdown");
    assert!(!selected.queued_prompts.iter().any(|row| row.pending_prompt.id == prompt_id));
    assert_eq!(selected.prompt_deliveries[&prompt_id].disposition, PromptDeliveryDisposition::Rejected);
    assert_eq!(selected.prompt_deliveries[&prompt_id].turn_generation, Some(turn_generation));
    drop(dispatch);
    turn.assert_no_prompt();
    turn.assert_no_control();
    tx.send(PersistRequest::Shutdown).unwrap();
    writer.join().unwrap();
}
