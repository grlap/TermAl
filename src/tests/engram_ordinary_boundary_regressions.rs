//! Local preparation failures and Stop arbitration before an ordinary ACK.
use super::*;

/// Hands one exact admission fence to the test; serves all other ACKs.
/// These tests inspect local control flow, not physical SQLite durability.
struct BoundaryWriter {
    tx: mpsc::Sender<PersistRequest>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl BoundaryWriter {
    fn install(
        state: &mut AppState,
        select: fn(&Value) -> bool,
    ) -> (Self, mpsc::Receiver<Box<PersistFence>>) {
        state.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        let (selected_tx, selected_rx) = mpsc::channel();
        state.persist_tx = tx.clone();
        let worker = std::thread::spawn(move || {
            let mut selected = false;
            while let Ok(request) = rx.recv() {
                match request {
                    PersistRequest::Fence(fence) => {
                        let matches = !selected
                            && !fence.retry_acknowledgement
                            && matches!(&fence.target,
                                PersistFenceTarget::EngramAdmission { content, .. } if select(content));
                        if matches {
                            selected = true;
                            selected_tx.send(fence).unwrap();
                        } else {
                            fence.finish(Ok(()));
                        }
                    }
                    PersistRequest::Shutdown => break,
                    _ => {}
                }
            }
        });
        (
            Self {
                tx,
                worker: Some(worker),
            },
            selected_rx,
        )
    }
}

impl Drop for BoundaryWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(PersistRequest::Shutdown);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[test]
fn admission_review_cancel_during_marker_ack_allows_replacement_under_same_outage() {
    let unavailable = || {
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::spawn_failed(
            "optional control remains unavailable",
        )))
    };
    let (mut state, session, receiver, transport) = root_fixture([unavailable(), unavailable()]);
    let (_writer, selected) = BoundaryWriter::install(&mut state, |content| {
        content["queue"]["engram_ordinary_handoff"] == true
    });
    let canceled = root_dispatch(&state, &session, false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch(&state, canceled));
        let fence = selected.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        let prompt_id = inspect(&state, &session, |record| {
            record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone()
        });
        state.cancel_queued_prompt(&session, &prompt_id).unwrap();
        fence.finish(Ok(()));
        let _ = worker.join().unwrap();
    });
    assert!(
        receiver.try_recv().is_err(),
        "the canceled prompt is never delivered"
    );
    // Exercise a second actual spawn failure, rather than a locally delayed
    // bind whose separate retry policy could mask the stale-slot defect.
    elapse_bind_backoff(&state, &session);
    let replacement = state
        .dispatch_turn(
            &session,
            SendMessageRequest {
                text: "replacement after cancellation".into(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
    let dispatch = match replacement {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the canceled head must release admission"),
    };
    let _ = deliver_turn_dispatch(&state, dispatch);
    let prompts = receiver
        .try_iter()
        .filter_map(|command| match command {
            CodexRuntimeCommand::Prompt { command, .. } => Some(command.prompt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        prompts.len(),
        1,
        "a stale canceled slot cannot block its replacement"
    );
    assert!(prompts[0].contains("replacement after cancellation"));
    assert_eq!(operations(&transport), ["session_bind", "session_bind"]);
    assert!(inspect(&state, &session, |record| record
        .queued_prompts
        .is_empty()));
}

#[test]
fn admission_review_preparation_cleanup_retires_only_its_stored_owner() {
    for (successor, durable) in [(false, false), (true, false), (false, true), (true, true)] {
        let (state, session, _receiver, _transport) = root_fixture([]);
        let _dispatch = root_dispatch(&state, &session, false);
        let owner = inspect(&state, &session, |record| {
            EngramQueuedAdmissionOwner::capture(record).unwrap()
        });
        {
            let mut inner = state.inner.lock().unwrap();
            let record = inner.session_mut(&session).unwrap();
            record.engram.dispatch_generation += 1;
            let stored_owner = if successor {
                EngramQueuedAdmissionOwner::capture(record).unwrap()
            } else {
                owner.clone()
            };
            record.engram.ordinary_handoff = Some(EngramOrdinaryHandoff {
                owner: stored_owner,
                durable,
            });
        }
        drop(EngramOrdinaryPreparation {
            state: &state,
            session_id: &session,
            owner: &owner,
        });
        assert_eq!(
            inspect(&state, &session, |record| {
                record.engram.ordinary_handoff.is_some()
            }),
            successor || durable,
            "an old guard retires its stale slot but preserves its successor"
        );
    }
}

#[test]
fn admission_review_cancel_after_marker_ack_retires_ready_permission() {
    let unavailable = || {
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::spawn_failed(
            "optional control remains unavailable",
        )))
    };
    let (state, session, receiver, transport) = root_fixture([unavailable(), unavailable()]);
    let canceled = root_dispatch(&state, &session, false);
    assert_eq!(
        state.prepare_engram_turn_delivery_off_lock(
            &session,
            canceled.engram_dispatch_generation().unwrap(),
        ),
        EngramTurnDeliveryPreparation::Ready
    );
    let prompt_id = inspect(&state, &session, |record| {
        assert!(engram_ordinary_handoff_allowed(record));
        record
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .id
            .clone()
    });
    state.cancel_queued_prompt(&session, &prompt_id).unwrap();
    assert!(inspect(&state, &session, |record| record
        .engram
        .ordinary_handoff
        .is_none()));
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, canceled).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Superseded
    ));
    assert!(receiver.try_recv().is_err());
    elapse_bind_backoff(&state, &session);
    let replacement = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, replacement).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind", "session_bind"]);
}

#[test]
fn admission_review_local_evaluate_fence_never_opens_ordinary_delivery() {
    for source in [QueuedPromptSource::User, QueuedPromptSource::Mailbox] {
        for fault in ["deadline", "late", "write_failed"] {
            let (mut state, session, receiver, transport) = root_fixture([
                bind_reply("local-fence-token"),
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::spawn_failed(
                    "must not reach the remote evaluate after a local fence failure",
                ))),
            ]);
            let (_writer, selected) = BoundaryWriter::install(&mut state, |content| {
                content["queue"]["engram_evaluate"].is_object()
                    && content["queue"]["engram_evaluate"]["prepared_begin"].is_null()
                    && content["queue"]["engram_ordinary_handoff"] != true
            });
            queue_test_engram_prompt(
                &state,
                &session,
                "local write must remain strict",
                source,
                None,
            );
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    if let Some(started) = state
                        .start_next_queued_turn_off_lock(&session, false, false)
                        .unwrap()
                    {
                        let _ = deliver_turn_dispatch(&state, started.dispatch);
                    }
                });
                let fence = selected.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
                if fault == "late" {
                    let clock = state.engram_budget_clock();
                    clock.wait_for_scripted_waiter();
                    assert!(fence.completion.resolve_at(Ok(()), clock.now()));
                    clock.advance(
                        fence
                            .completion
                            .deadline
                            .saturating_duration_since(clock.now()),
                    );
                } else if fault == "deadline" {
                    fence.finish(Err(PersistFenceError::Deadline));
                } else {
                    fence.finish(Err(PersistFenceError::WriteFailed(
                        "local evaluation write rejected".into(),
                    )));
                }
                worker.join().unwrap();
            });
            assert!(
                receiver.try_recv().is_err(),
                "local evaluate fence failure cannot permit an ungranted prompt"
            );
            assert_eq!(operations(&transport), ["session_bind"]);
            let held = inspect(&state, &session, |record| record.clone());
            assert_eq!(held.queued_prompts.len(), 1);
            assert!(!held.queued_prompts[0].engram_ordinary_handoff);
            assert!(held.engram.ordinary_handoff.is_none());
            assert!(held.engram.active_grant_id.is_none());
        }
    }
}

#[tokio::test]
async fn admission_review_stop_during_marker_fence_keeps_unsent_owner() {
    for failed_stop in [true, false] {
        let (mut state, session, root_rx, transport) =
            root_fixture([ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::spawn_failed("optional control unavailable"),
            ))]);
        let dispatch = root_dispatch(&state, &session, false);
        let generation = dispatch.engram_dispatch_generation().unwrap();
        let active_turn_generation = dispatch.active_turn_generation();
        let (runtime, receiver) =
            test_acp_runtime_handle(AcpAgent::OpenCode, "ordinary-pending-fence-stop");
        let token = RuntimeToken::Acp(runtime.runtime_id.clone());
        let process = runtime.process.clone();
        let sender = runtime.input_tx.clone();
        let lifecycle = runtime.turn_lifecycle.clone();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        }
        drop(dispatch);
        let (_writer, selected) = BoundaryWriter::install(&mut state, |content| {
            content["queue"]["engram_ordinary_handoff"] == true
        });
        let _failure =
            failed_stop.then(|| force_test_kill_child_process_failure(&process, "OpenCode"));
        let stop_gate = install_test_stop_fence_gate(&state, &session);
        let worker_state = state.clone();
        let worker_session = session.clone();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || {
            let preparation =
                worker_state.prepare_engram_turn_delivery_off_lock(&worker_session, generation);
            if preparation == EngramTurnDeliveryPreparation::Ready {
                handoff_prepared_turn_dispatch(
                    &worker_state,
                    TurnDispatch::PersistentAcp {
                        federation_handoff: None,
                        opening_prompt: None,
                        active_turn_generation,
                        engram_dispatch_generation: Some(generation),
                        runtime_token: worker_token,
                        session_id: worker_session,
                        mailbox_notification: None,
                        sender,
                        turn_lifecycle: lifecycle,
                        command: AcpPromptCommand {
                            cwd: String::new(),
                            cursor_mode: None,
                            model: String::new(),
                            opencode_effort: None,
                            opencode_mode: None,
                            prompt: "exact pending ordinary prompt".into(),
                            resume_session_id: None,
                        },
                    },
                )
                .unwrap();
            }
            preparation
        });
        let fence = selected.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        let no_permission_before_ack = inspect(&state, &session, |record| {
            !engram_provider_handoff_allowed(record)
        });
        let mut events = state.subscribe_events();
        state.request_stop_session(&session).unwrap();
        stop_gate.wait_until_claimed();
        let no_prompt_before_ack = !receiver
            .try_iter()
            .any(|command| matches!(command, AcpRuntimeCommand::Prompt(_)));
        fence.finish(Ok(()));
        stop_gate.release();
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while inspect(&state, &session, |record| record.runtime_stop_in_progress) {
                events.recv().await.unwrap();
            }
        })
        .await
        .expect("public Stop must settle");
        let preparation = tokio::task::spawn_blocking(move || worker.join().unwrap())
            .await
            .unwrap();
        let prompts = receiver
            .try_iter()
            .filter_map(|command| match command {
                AcpRuntimeCommand::Prompt(command) => Some(command.prompt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(no_permission_before_ack && no_prompt_before_ack);
        assert_eq!(
            prompts,
            if failed_stop {
                vec!["exact pending ordinary prompt".to_owned()]
            } else {
                vec![]
            },
            "a failed Stop must preserve the unsent owner through its pending marker acknowledgement: {preparation:?}"
        );
        assert!(root_rx.try_recv().is_err());
        assert!(
            !operations(&transport)
                .iter()
                .any(|op| op == "turn_begin" || op == "turn_checkpoint")
        );
    }
}
