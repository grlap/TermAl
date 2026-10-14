//! Cancellation after a capacity Resume's real off-lock Engram evaluation.
use super::*;

fn late_head_after_empty_capacity_capture(receipted: bool, cancel: bool) {
    use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
    let (evaluate, gate) = gated_engram_step("turn_evaluate", grant_reply("late-head-grant"));
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("late-head-token")),
        evaluate,
        immediate_engram_step("turn_begin", begin_reply("late-head-grant")),
    ]);
    let (mut state, session, _) = receipted_admission::gated_root_fixture([]);
    state.install_control_test_transport(transport.clone());
    let (runtime, receiver, process) = test_shared_codex_runtime("capacity-empty-owner");
    let generation = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        assert!(record.queued_prompts.is_empty());
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session.clone(),
            }),
        });
        record.active_turn_generation
    };
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    let nonce = state
        .prepare_provider_capacity_failure(
            &session,
            &token,
            Some(generation),
            true,
            &json!({"message":"capacity", "codexErrorInfo":"serverOverloaded"}),
            false,
        )
        .unwrap();
    state
        .fail_turn_if_runtime_matches(&session, &token, "capacity")
        .unwrap();
    state
        .finish_provider_capacity_failure(&session, &nonce)
        .unwrap();
    let empty_owner = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.recovering = true;
        retry.dispatched = true;
        retry.attempts = 1;
        retry.due_at = None;
        record.session.status = SessionStatus::Error;
        record.session.provider_capacity_waiting = false;
        // The exact empty observation the tick passes to Resume after unlocking.
        let owner = EngramQueuedAdmissionOwner::capture(record);
        assert!(owner.is_none());
        owner
    };
    for (position, text) in ["late peer head", "late peer tail"].into_iter().enumerate() {
        let peer = test_session_id(&state, Agent::Claude);
        queue_test_engram_prompt(
            &state,
            &session,
            text,
            QueuedPromptSource::User,
            Some(MessageSource::peer(peer, "Peer".to_owned())),
        );
        if position == 0 {
            // A producer's acknowledged durability marker keeps its exact
            // peer entry standalone; ordinary peer batches merge across peers.
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            let head = record.queued_prompts.front().unwrap();
            let id = head.pending_prompt.id.clone();
            let content = queued_prompt_durability_content(head);
            record.queued_prompt_durability_holds.insert(
                id,
                QueuedPromptDurabilityHold {
                    nonce: Uuid::new_v4().to_string(),
                    content,
                    failure: None,
                    acknowledged: true,
                },
            );
        }
    }
    let (head, tail, dispatch_generation) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.provider_capacity_retry.as_ref().unwrap().nonce,
            nonce
        );
        (
            record.queued_prompts[0].pending_prompt.id.clone(),
            record.queued_prompts[1].pending_prompt.id.clone(),
            record.engram.dispatch_generation,
        )
    };
    let writer = receipted.then(|| {
        assert_eq!(
            receipted_admission::receipt_the_parked_head(&state, &session),
            head
        );
        DeliveryWriter::attach(&mut state)
    });
    let resuming_state = state.clone();
    let resuming_session = session.clone();
    let resuming_nonce = nonce.clone();
    let resuming = std::thread::spawn(move || {
        resuming_state.resume_local_session_queue_inner(
            &resuming_session,
            empty_owner,
            Some(&resuming_nonce),
        )
    });
    let mut held_fence = None;
    let finishing = if let Some(writer) = &writer {
        assert!(matches!(
            resuming.join().unwrap().unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        held_fence = Some(writer.take(HeldFence::WriteAhead));
        let wal = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].prompt_deliveries[&head]
                .write_ahead
                .clone()
        };
        let acknowledging = state.clone();
        let acknowledging_session = session.clone();
        let acknowledging_head = head.clone();
        std::thread::spawn(move || {
            acknowledging.settle_prompt_delivery_write_ahead(
                &acknowledging_session,
                &acknowledging_head,
                &wal,
                Ok(()),
            );
        })
    } else {
        std::thread::spawn(move || {
            resuming.join().unwrap().unwrap();
        })
    };
    // The actual late head's off-lock evaluation is held, not a simulated grant.
    gate.wait();
    if cancel {
        state.cancel_queued_prompt(&session, &tail).unwrap();
    }
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            head
        );
        assert_eq!(record.engram.dispatch_generation, dispatch_generation);
        assert_eq!(record.active_turn_generation, generation);
        if cancel {
            assert!(record.provider_capacity_retry.is_none());
        }
    }
    gate.release();
    finishing.join().unwrap();
    if cancel {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.active_turn_generation, generation,
            "a canceled empty-capture Resume cannot promote its late head"
        );
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            head
        );
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.rebind_required);
        if receipted {
            assert_eq!(
                record.prompt_deliveries[&head].disposition,
                PromptDeliveryDisposition::Pending
            );
        }
        drop(inner);
        assert!(
            receiver.try_recv().is_err(),
            "no provider prompt after tail Cancel"
        );
        assert_eq!(
            transport.requests().len(),
            2,
            "bind/evaluate only, no begin"
        );
    } else {
        let command = match recv_within_guard(&receiver, "uncanceled late head") {
            Ok(CodexRuntimeCommand::Prompt { command, .. }) => command,
            _ => panic!("the uncanceled late head promotes exactly once"),
        };
        assert_eq!(
            command.active_turn_generation,
            generation.wrapping_add(1).max(1)
        );
        assert!(receiver.try_recv().is_err());
        assert_eq!(transport.requests().len(), 3, "bind/evaluate/one begin");
    }
    drop(held_fence);
    drop(writer);
}

#[test]
fn late_plain_head_keeps_capacity_ownership_after_an_empty_capture() {
    late_head_after_empty_capacity_capture(false, false);
    late_head_after_empty_capacity_capture(false, true);
}

#[test]
fn late_receipted_head_keeps_capacity_ownership_after_an_empty_capture() {
    late_head_after_empty_capacity_capture(true, false);
    late_head_after_empty_capacity_capture(true, true);
}

#[test]
fn a_tick_preserves_its_own_ack_admission_but_keeps_settings_and_cancel_fences() {
    use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};
    for change in [
        "none",
        "settings",
        "tail_cancel",
        "pause",
        "independent_hold",
        "head_content",
    ] {
        let (evaluate, gate) =
            gated_engram_step("turn_evaluate", grant_reply("owned-capacity-grant"));
        let transport = GatedEngramControlTransport::new([
            immediate_engram_step("session_bind", bind_reply("owned-capacity-token")),
            evaluate,
            immediate_engram_step("turn_begin", begin_reply("owned-capacity-grant")),
        ]);
        let (mut state, session, _unused) = receipted_admission::gated_root_fixture([]);
        state.install_control_test_transport(transport.clone());
        let (runtime, receiver, process) = test_shared_codex_runtime("capacity-owned-ack");
        queue_test_engram_prompt(
            &state,
            &session,
            "owned receipted head",
            QueuedPromptSource::User,
            None,
        );
        queue_test_engram_prompt(&state, &session, "tail", QueuedPromptSource::User, None);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process,
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: session.clone(),
                }),
            });
        }
        let writer = DeliveryWriter::attach(&mut state);
        let generation = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()].active_turn_generation
        };
        let token = RuntimeToken::Codex(runtime.runtime_id.clone());
        let nonce = state
            .prepare_provider_capacity_failure(
                &session,
                &token,
                Some(generation),
                true,
                &json!({"message":"capacity", "codexErrorInfo":"serverOverloaded"}),
                false,
            )
            .unwrap();
        state
            .fail_turn_if_runtime_matches(&session, &token, "capacity")
            .unwrap();
        state
            .finish_provider_capacity_failure(&session, &nonce)
            .unwrap();
        let head_id = receipted_admission::receipt_the_parked_head(&state, &session);
        let (owner, tail_id) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            let retry = record.provider_capacity_retry.as_mut().unwrap();
            retry.recovering = true;
            retry.dispatched = true;
            retry.attempts = 1;
            retry.due_at = None;
            record.session.status = SessionStatus::Error;
            record.session.provider_capacity_waiting = false;
            let mut owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
            owner.capacity_nonce = Some(nonce.clone());
            (owner, record.queued_prompts[1].pending_prompt.id.clone())
        };
        assert!(matches!(
            state
                .resume_local_session_queue_inner(&session, Some(owner), Some(&nonce))
                .unwrap(),
            Some(TurnDispatchDeliveryOutcome::Scheduled)
        ));
        let fence = writer.take(HeldFence::WriteAhead);
        let write_ahead = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            record.prompt_deliveries[&head_id].write_ahead.clone()
        };
        let acknowledging_state = state.clone();
        let acknowledging_session = session.clone();
        let acknowledging_head = head_id.clone();
        let acknowledging = std::thread::spawn(move || {
            acknowledging_state.settle_prompt_delivery_write_ahead(
                &acknowledging_session,
                &acknowledging_head,
                &write_ahead,
                Ok(()),
            );
        });
        // The real ACK settlement's continuation is now inside its real
        // off-lock evaluation, before the head can be promoted.
        gate.wait();
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.admission_in_progress.is_some());
            assert_eq!(
                record.engram.admission_prompt_id.as_deref(),
                Some(head_id.as_str())
            );
            assert!(prompt_delivery_resume_holds(record, &head_id));
            assert!(
                provider_capacity_owns_ack_admission(record, &nonce),
                "the continuation is bound to its actual guard before off-lock evaluation"
            );
            assert_eq!(
                record.engram.dispatch_generation,
                record
                    .provider_capacity_retry
                    .as_ref()
                    .unwrap()
                    .dispatch_generation
            );
            assert!(!provider_capacity_manual_hold_except_owned_admission(
                record, true
            ));
        }
        match change {
            "settings" => {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].session.model.push_str("-changed");
            }
            "tail_cancel" => {
                state.cancel_queued_prompt(&session, &tail_id).unwrap();
            }
            "pause" | "independent_hold" | "head_content" => {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                match change {
                    "pause" => record.set_auto_dispatch_blocked(true),
                    "independent_hold" => {
                        record.engram.uncertain_grant_id = Some("unrelated-grant".to_owned())
                    }
                    _ => record
                        .queued_prompts
                        .front_mut()
                        .unwrap()
                        .pending_prompt
                        .text
                        .push_str(" edited"),
                }
            }
            _ => {}
        }
        state.provider_capacity_retry_tick(chrono::Utc::now());
        state.provider_capacity_retry_tick(chrono::Utc::now());
        let retained = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .provider_capacity_retry
                .as_ref()
                .map(|retry| retry.nonce.clone())
        };
        if change == "none" {
            assert_eq!(
                retained.as_deref(),
                Some(nonce.as_str()),
                "a tick cannot cancel its own admission"
            );
        } else {
            assert!(retained.is_none(), "the {change} fence still ends recovery");
        }
        gate.release();
        acknowledging
            .join()
            .expect("the exact ACK settlement completes");
        if change == "none" {
            let command = match recv_within_guard(&receiver, "owned ACK promotion after a tick") {
                Ok(CodexRuntimeCommand::Prompt { command, .. }) => command,
                _ => panic!("expected exactly one recovered provider prompt"),
            };
            assert_eq!(
                command.active_turn_generation,
                generation.wrapping_add(1).max(1)
            );
            state.provider_capacity_retry_tick(chrono::Utc::now());
            assert!(receiver.try_recv().is_err());
            assert_eq!(
                transport.requests().len(),
                3,
                "bind, evaluate and one begin"
            );
        } else {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert!(record.engram.admission_in_progress.is_none());
            assert_eq!(
                record.prompt_deliveries[&head_id].disposition,
                PromptDeliveryDisposition::Pending
            );
            drop(inner);
            assert!(receiver.try_recv().is_err());
            assert_eq!(transport.requests().len(), 2, "no begin after {change}");
        }
        drop(fence);
        drop(writer);
    }
}

#[test]
fn tail_cancel_while_capacity_resume_is_evaluating_never_promotes_the_head() {
    let (evaluate, gate) = gated_engram_step("turn_evaluate", grant_reply("capacity-grant"));
    let transport = GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("capacity-token")),
        evaluate,
        immediate_engram_step("turn_begin", begin_reply("capacity-grant")),
    ]);
    let (state, session, _unused_runtime_receiver) = receipted_admission::gated_root_fixture([]);
    state.install_control_test_transport(transport.clone());
    let (runtime, receiver, process) = test_shared_codex_runtime("capacity-off-lock");
    queue_test_engram_prompt(
        &state,
        &session,
        "capacity queued head",
        QueuedPromptSource::User,
        None,
    );
    queue_test_engram_prompt(
        &state,
        &session,
        "cancel this tail",
        QueuedPromptSource::User,
        None,
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session.clone(),
            }),
        });
    }
    let generation = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].active_turn_generation
    };
    let token = RuntimeToken::Codex(runtime.runtime_id.clone());
    let nonce = state
        .prepare_provider_capacity_failure(
            &session,
            &token,
            Some(generation),
            true,
            &json!({"message":"capacity", "codexErrorInfo":"serverOverloaded"}),
            false,
        )
        .unwrap();
    state
        .fail_turn_if_runtime_matches(&session, &token, "capacity")
        .unwrap();
    state
        .finish_provider_capacity_failure(&session, &nonce)
        .unwrap();
    let (owner, tail_id, head_id, dispatch_generation) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        let retry = record.provider_capacity_retry.as_mut().unwrap();
        retry.recovering = true;
        retry.dispatched = true;
        retry.attempts = 1;
        retry.due_at = None;
        record.session.status = SessionStatus::Error;
        let mut owner = EngramQueuedAdmissionOwner::capture(record).unwrap();
        owner.capacity_nonce = Some(nonce.clone());
        (
            owner,
            record.queued_prompts[1].pending_prompt.id.clone(),
            record.queued_prompts[0].pending_prompt.id.clone(),
            record.engram.dispatch_generation,
        )
    };
    let resuming_state = state.clone();
    let resuming_session = session.clone();
    let resuming = std::thread::spawn(move || {
        resuming_state.resume_local_session_queue_inner(
            &resuming_session,
            Some(owner),
            Some(&nonce),
        )
    });
    gate.wait();
    state.cancel_queued_prompt(&session, &tail_id).unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            head_id
        );
        assert_eq!(record.engram.dispatch_generation, dispatch_generation);
        assert!(record.provider_capacity_retry.is_none());
    }
    gate.release();
    let resumed = resuming.join().unwrap().unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.active_turn_generation, generation,
            "no turn was promoted"
        );
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            head_id
        );
    }
    // The existing Resume contract keeps an unstarted superseded drain as
    // None (finish_resume_drain / UnstartedResumeStep::Keep).
    assert!(resumed.is_none());
    assert!(
        receiver.try_recv().is_err(),
        "no prompt crosses the canceled capacity owner"
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.queued_prompts.front().unwrap().pending_prompt.id,
        head_id
    );
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.engram.active_grant_id.is_none());
    assert!(
        record.engram.rebind_required,
        "the late grant is repaired before another admission"
    );
    assert_eq!(
        record.active_turn_generation, generation,
        "no turn was promoted"
    );
    assert_eq!(
        transport.requests().len(),
        2,
        "only bind and evaluate; no turn_begin"
    );
    assert_eq!(
        transport.requests()[1].request["operation"],
        "turn_evaluate"
    );
}
