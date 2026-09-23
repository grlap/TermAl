//! Post-receipt ownership and nonterminal recovery regressions. Scripted control
//! and provider channels only; the live Engram fixture remains unchanged.
use super::*;
use crate::tests::delegation_support::finish_delegation_child_with_assistant_text;

#[test]
fn ambiguous_dispatch_card_commit_is_an_error_and_never_reaches_provider() {
    let (mut state, session, receiver, _) = root_fixture([
        bind_reply("persistence-unknown-token"),
        grant_reply("persistence-unknown-grant"),
        begin_reply("persistence-unknown-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    state.shutdown_persist_blocking();
    let failing_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("dispatch-card-caller-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());

    let error = deliver_turn_dispatch_now(&state, dispatch)
        .expect_err("ambiguous dispatch-card persistence must reach the caller");
    assert!(error.message.contains("persistence is unknown"));
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.dispatch_generation, generation);
    assert!(record.engram.pending_dispatch.is_none());
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.session.live_activity.is_none());
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(!record.queued_prompts[0].engram_waiting);
    assert_eq!(
        record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .and_then(|prepared| prepared.begun_grant_id.as_deref()),
        Some("persistence-unknown-grant")
    );
    assert!(record.session.preview.contains("persistence unknown"));
    drop(inner);

    state
        .resume_session_queue(&session)
        .expect("public Resume should safely preserve a reconciliation-only owner");
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.queued_prompts[0].engram_interrupted);
    drop(inner);

    fs::remove_dir_all(failing_path).unwrap();
}

#[test]
fn failed_grant_fence_and_interruption_commit_publish_one_fail_closed_owner() {
    let (state, session, receiver, _) = root_fixture([
        bind_reply("fence-unknown-token"),
        grant_reply("fence-unknown-grant"),
        begin_reply("fence-unknown-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    let (persist_tx, persist_rx) = mpsc::channel();
    let mut state = AppState {
        persist_tx,
        ..state
    };
    let failing_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("grant-fence-interrupt-is-directory");
    fs::create_dir_all(&failing_path).unwrap();
    state.persistence_path = Arc::new(failing_path.clone());
    let mut state_events = state.subscribe_events();

    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch_now(&state, dispatch));
        assert!(matches!(
            persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap(),
            PersistRequest::Delta
        ));
        let PersistRequest::Fence(fence) =
            persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap()
        else {
            panic!("grant durability must request its exact-content fence")
        };
        fence.finish(Err(PersistFenceError::WriteFailed(
            "injected grant fence failure".to_owned(),
        )));
        drop(persist_rx);
        worker.join().unwrap()
    });
    let error = result.expect_err("fence uncertainty must reach the caller");
    assert!(error.message.contains("persistence is unknown"));
    assert!(receiver.try_recv().is_err());
    let published_payload = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, state_events.recv())
                .await
                .expect("interruption failure publication timed out")
                .unwrap()
        });
    let published: Value = serde_json::from_str(&published_payload).unwrap();
    let published_session = published["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["id"] == session)
        .unwrap();
    assert_eq!(published_session["queuePaused"], true);
    assert_eq!(published_session["status"], "idle");
    assert!(published_session["liveActivity"].is_null());
    assert!(
        published_session["preview"]
            .as_str()
            .unwrap()
            .contains("interrupted/unknown")
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.dispatch_generation, generation);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.session.live_activity.is_none());
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(!record.queued_prompts[0].engram_waiting);
    assert_eq!(
        record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .and_then(|prepared| prepared.begun_grant_id.as_deref()),
        Some("fence-unknown-grant")
    );
    drop(inner);

    fs::remove_dir_all(failing_path).unwrap();
}

#[test]
fn public_root_send_surfaces_post_begin_fence_uncertainty_and_resume_requires_reconcile() {
    let (mut state, session, receiver, _) = root_fixture([]);
    state.shutdown_persist_blocking();
    let (persist_tx, persist_rx) = mpsc::channel();
    state.persist_tx = persist_tx;
    let stop_persister = Arc::new(AtomicBool::new(false));
    let fail_next_fence = Arc::new(AtomicBool::new(false));
    let persist_stop = stop_persister.clone();
    let persist_failure = fail_next_fence.clone();
    let persister = std::thread::spawn(move || {
        while let Ok(request) = persist_rx.recv() {
            if persist_stop.load(Ordering::SeqCst) {
                break;
            }
            if let PersistRequest::Fence(fence) = request {
                if persist_failure.swap(false, Ordering::SeqCst) {
                    fence.finish(Err(PersistFenceError::Deadline));
                } else {
                    fence.finish(Ok(()));
                }
            }
        }
    });
    let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply("public-root-grant"));
    state.install_control_test_transport(GatedEngramControlTransport::new([
        immediate_engram_step("session_bind", bind_reply("public-root-token")),
        immediate_engram_step("turn_evaluate", grant_reply("public-root-grant")),
        begin,
        immediate_engram_step("turn_checkpoint", checkpoint_reply("public-root-grant")),
    ]));

    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            dispatch_turn_and_snapshot(
                &state,
                &session,
                SendMessageRequest {
                    text: "Public root send retained across fence uncertainty".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
        });
        begin_gate.wait();
        fail_next_fence.store(true, Ordering::SeqCst);
        begin_gate.release();
        worker.join().unwrap()
    });
    let error = match result {
        Ok(_) => panic!("public Send must surface post-begin persistence uncertainty"),
        Err(error) => error,
    };
    assert!(error.message.contains("persistence is unknown"));
    stop_persister.store(true, Ordering::SeqCst);
    state.persist_tx.send(PersistRequest::Delta).unwrap();
    persister.join().unwrap();
    assert!(receiver.try_recv().is_err());

    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.session.live_activity.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(record.queued_prompts[0].engram_interrupted);
    }
    state
        .resume_session_queue(&session)
        .expect("public Resume should no-op on a reconciliation-only owner");
    assert!(receiver.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.queued_prompts[0].engram_interrupted);
}

#[test]
fn orphan_recovery_retains_exact_ids_on_persistence_uncertainty_without_requeue() {
    for mailbox_before_orchestrator in [false, true] {
        let (mut state, session, receiver, _) = root_fixture([]);
        if mailbox_before_orchestrator {
            queue_test_engram_prompt(
                &state,
                &session,
                "durable mailbox wake before workflow",
                QueuedPromptSource::Mailbox,
                None,
            );
        }
        queue_test_engram_prompt(
            &state,
            &session,
            "durable orphaned orchestrator workflow",
            QueuedPromptSource::Orchestrator,
            None,
        );
        let original_ids = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .queued_prompts
                .iter()
                .map(|queued| queued.pending_prompt.id.clone())
                .collect::<Vec<_>>()
        };

        state.shutdown_persist_blocking();
        let (persist_tx, persist_rx) = mpsc::channel();
        state.persist_tx = persist_tx;
        let stop_persister = Arc::new(AtomicBool::new(false));
        let fail_next_fence = Arc::new(AtomicBool::new(false));
        let persist_stop = stop_persister.clone();
        let persist_failure = fail_next_fence.clone();
        let persister = std::thread::spawn(move || {
            while let Ok(request) = persist_rx.recv() {
                if persist_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let PersistRequest::Fence(fence) = request {
                    if persist_failure.swap(false, Ordering::SeqCst) {
                        fence.finish(Err(PersistFenceError::Deadline));
                    } else {
                        fence.finish(Ok(()));
                    }
                }
            }
        });
        let grant_id = if mailbox_before_orchestrator {
            "mailbox-orchestrator-orphan-grant"
        } else {
            "orchestrator-orphan-grant"
        };
        let (begin, begin_gate) = gated_engram_step("turn_begin", begin_reply(grant_id));
        state.install_control_test_transport(GatedEngramControlTransport::new([
            immediate_engram_step("session_bind", bind_reply("orphan-token")),
            immediate_engram_step("turn_evaluate", grant_reply(grant_id)),
            begin,
            immediate_engram_step("turn_checkpoint", checkpoint_reply(grant_id)),
        ]));

        std::thread::scope(|scope| {
            let worker = scope.spawn(|| state.dispatch_orphaned_workflow_prompts());
            begin_gate.wait();
            fail_next_fence.store(true, Ordering::SeqCst);
            begin_gate.release();
            worker.join().unwrap();
        });
        stop_persister.store(true, Ordering::SeqCst);
        state.persist_tx.send(PersistRequest::Delta).unwrap();
        persister.join().unwrap();
        assert!(receiver.try_recv().is_err());

        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Idle);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record
                .queued_prompts
                .iter()
                .map(|queued| queued.pending_prompt.id.clone())
                .collect::<Vec<_>>(),
            original_ids,
            "retained uncertainty must not create a replacement ID"
        );
        assert!(record.queued_prompts[0].engram_interrupted);
        if mailbox_before_orchestrator {
            assert_eq!(record.queued_prompts.len(), 2);
            assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Mailbox);
            assert_eq!(
                record.queued_prompts[1].source,
                QueuedPromptSource::Orchestrator
            );
        } else {
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(
                record.queued_prompts[0].source,
                QueuedPromptSource::Orchestrator
            );
        }
    }
}

#[test]
fn initial_child_post_begin_persistence_unknown_retains_running_delegation_and_exact_owner() {
    for fail_card_commit in [true, false] {
        let (mut state, parent, receiver, _) = root_fixture([]);
        state.shutdown_persist_blocking();
        let failing_path =
            state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join(if fail_card_commit {
                    "child-card-commit-is-directory"
                } else {
                    "child-fence-unused-directory"
                });
        fs::create_dir_all(&failing_path).unwrap();
        if fail_card_commit {
            state.persistence_path = Arc::new(failing_path.clone());
        }

        let (persist_tx, persist_rx) = mpsc::channel();
        state.persist_tx = persist_tx;
        let stop_persister = Arc::new(AtomicBool::new(false));
        let fail_next_fence = Arc::new(AtomicBool::new(false));
        let persist_stop = stop_persister.clone();
        let persist_failure = fail_next_fence.clone();
        let mut persister = Some(std::thread::spawn(move || {
            while let Ok(request) = persist_rx.recv() {
                if persist_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let PersistRequest::Fence(fence) = request {
                    if persist_failure.swap(false, Ordering::SeqCst) {
                        fence.finish(Err(PersistFenceError::Deadline));
                    } else {
                        fence.finish(Ok(()));
                    }
                }
            }
        }));

        let (begin, begin_gate) = gated_engram_step(
            "turn_begin",
            begin_reply(if fail_card_commit {
                "child-card-grant"
            } else {
                "child-fence-grant"
            }),
        );
        state.install_control_test_transport(GatedEngramControlTransport::new([
            immediate_engram_step("session_bind", bind_reply("parent-child-test")),
            immediate_engram_step("session_bind", bind_reply("child-test")),
            immediate_engram_step(
                "turn_evaluate",
                grant_reply(if fail_card_commit {
                    "child-card-grant"
                } else {
                    "child-fence-grant"
                }),
            ),
            begin,
            immediate_engram_step(
                "turn_checkpoint",
                checkpoint_reply(if fail_card_commit {
                    "child-card-grant"
                } else {
                    "child-fence-grant"
                }),
            ),
        ]));

        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                state.create_read_only_delegation(
                    &parent,
                    CreateDelegationRequest {
                        prompt: "retain the exact initial child owner".to_owned(),
                        title: Some("Post-begin persistence recovery".to_owned()),
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Explorer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
            });
            begin_gate.wait();
            if fail_card_commit {
                stop_persister.store(true, Ordering::SeqCst);
                state.persist_tx.send(PersistRequest::Delta).unwrap();
                persister.take().unwrap().join().unwrap();
            } else {
                fail_next_fence.store(true, Ordering::SeqCst);
            }
            begin_gate.release();
            worker.join().unwrap()
        });

        let error = match result {
            Ok(_) => panic!("persistence uncertainty must reach child creation"),
            Err(error) => error,
        };
        assert!(error.message.contains("persistence is unknown"));
        if !fail_card_commit {
            stop_persister.store(true, Ordering::SeqCst);
            state.persist_tx.send(PersistRequest::Delta).unwrap();
            persister.take().unwrap().join().unwrap();
        }
        assert!(receiver.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.delegations.len(), 1);
        assert_eq!(inner.delegations[0].status, DelegationStatus::Running);
        let child_id = &inner.delegations[0].child_session_id;
        let child = &inner.sessions[inner.find_session_index(child_id).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Idle);
        assert!(child.session.live_activity.is_none());
        assert!(child.orchestrator_auto_dispatch_blocked);
        assert_eq!(child.queued_prompts.len(), 1);
        assert!(child.queued_prompts[0].engram_interrupted);
        assert_eq!(
            child.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .and_then(|prepared| prepared.begun_grant_id.as_deref()),
            Some(if fail_card_commit {
                "child-card-grant"
            } else {
                "child-fence-grant"
            })
        );
        drop(inner);
        fs::remove_dir_all(failing_path).unwrap();
    }
}

#[test]
fn admitted_channel_rejection_checkpoints_and_retires_only_its_exact_owner() {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = StatefulEngramControlTransport::new();
    state.install_control_test_transport(transport.clone());
    let dispatch = root_dispatch(&state, &session, false);
    let failed_generation = dispatch.engram_dispatch_generation().unwrap();
    queue_test_engram_prompt(
        &state,
        &session,
        "successor after rejected provider channel",
        QueuedPromptSource::User,
        None,
    );
    drop(receiver);

    let error = deliver_turn_dispatch_now(&state, dispatch)
        .expect_err("closed provider channel must reject the admitted turn");
    assert!(error.message.contains("failed to queue prompt for Codex"));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.pending_dispatch.is_none());
        assert!(record.engram.dispatch_generation > failed_generation);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "successor after rejected provider channel"
        );
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(inner.delegations.is_empty());
    }
    let durable: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert_eq!(durable.queued_prompts.len(), 1);
    assert_eq!(
        durable.queued_prompts[0].pending_prompt.text,
        "successor after rejected provider channel"
    );
    assert!(durable.queued_prompts[0].engram_evaluate.is_none());

    let reloaded = {
        let inner = state.inner.lock().unwrap();
        let encoded = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
        serde_json::from_slice::<PersistedState>(&encoded)
            .unwrap()
            .into_inner()
            .unwrap()
    };
    let reloaded_record = &reloaded.sessions[reloaded.find_session_index(&session).unwrap()];
    assert!(reloaded_record.engram.active_grant_id.is_none());
    assert_eq!(reloaded_record.queued_prompts.len(), 1);
    assert_eq!(
        reloaded_record.queued_prompts[0].pending_prompt.text,
        "successor after rejected provider channel"
    );
    assert!(!reloaded_record.queued_prompts[0].has_engram_intent());

    let (retry_runtime, retry_receiver) = test_codex_runtime_handle("channel-rejection-successor");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(retry_runtime);
        record.session.status = SessionStatus::Idle;
    }
    state.resume_session_queue(&session).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = retry_receiver
        .try_recv()
        .expect("freshly authorized channel-rejection successor")
    else {
        panic!("successor must reach the provider only after fresh authorization")
    };
    assert_eq!(command.prompt, "successor after rejected provider channel");
    assert!(retry_receiver.try_recv().is_err());

    let requests = transport.requests();
    let evaluate_keys = requests
        .iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .map(|request| request.request["idempotency_key"].clone())
        .collect::<Vec<_>>();
    let begin_keys = requests
        .iter()
        .filter(|request| request.request["operation"] == "turn_begin")
        .map(|request| request.request["idempotency_key"].clone())
        .collect::<Vec<_>>();
    assert_eq!(evaluate_keys.len(), 2);
    assert_eq!(begin_keys.len(), 2);
    assert_ne!(evaluate_keys[0], evaluate_keys[1]);
    assert_ne!(begin_keys[0], begin_keys[1]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
}

#[test]
fn admitted_channel_rejection_stays_interrupted_when_persistence_is_unavailable() {
    let (mut state, session, receiver, _) = root_fixture([]);
    let transport = StatefulEngramControlTransport::new();
    state.install_control_test_transport(transport.clone());
    let dispatch = root_dispatch(&state, &session, false);
    let failed_generation = dispatch.engram_dispatch_generation().unwrap();
    assert!(matches!(
        state.prepare_engram_turn_delivery_off_lock(&session, failed_generation),
        EngramTurnDeliveryPreparation::Ready
    ));
    drop(receiver);

    let failing_persistence_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("rejected-delivery-persistence-is-directory");
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.persistence_path = Arc::new(failing_persistence_path);

    let error = handoff_prepared_turn_dispatch(&state, dispatch)
        .expect_err("closed provider channel must reject the admitted turn");
    assert!(error.message.contains("failed to queue prompt for Codex"));
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert_eq!(record.engram.dispatch_generation, failed_generation);
        assert!(record.engram.active_grant_id.is_some());
        assert!(record.engram.pending_dispatch.is_none());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(!record.queued_prompts[0].engram_waiting);
        assert!(record.queued_prompts[0].has_engram_intent());
    }

    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        0,
        "a grant must not be checkpointed unless its interrupted owner was durable"
    );
}

#[test]
fn admitted_fast_discovery_failure_cannot_replay_checkpointed_authority() {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = StatefulEngramControlTransport::new();
    state.install_control_test_transport(transport.clone());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.model = "catalog-model".into();
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
    }
    let dispatch = root_dispatch(&state, &session, false);
    let failed_generation = dispatch.engram_dispatch_generation().unwrap();
    queue_test_engram_prompt(
        &state,
        &session,
        "successor after Fast discovery failure",
        QueuedPromptSource::User,
        None,
    );
    let worker_state = state.clone();
    let worker = std::thread::spawn(move || deliver_turn_dispatch_now(&worker_state, dispatch));
    let CodexRuntimeCommand::RefreshModelList { response_tx } =
        phase_sync::receive(&receiver, "admitted Fast discovery request")
    else {
        panic!("Fast discovery must precede provider delivery")
    };
    response_tx
        .send(Err("forced catalog failure".to_owned()))
        .unwrap();
    let error = worker
        .join()
        .unwrap()
        .expect_err("Fast discovery failure must reject the admitted turn");
    assert!(error.message.contains("failed to resolve Codex Fast"));
    assert!(receiver.try_recv().is_err());

    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.pending_dispatch.is_none());
        assert!(record.engram.dispatch_generation > failed_generation);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "successor after Fast discovery failure"
        );
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(inner.delegations.is_empty());
    }
    let durable: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert_eq!(durable.queued_prompts.len(), 1);
    assert_eq!(
        durable.queued_prompts[0].pending_prompt.text,
        "successor after Fast discovery failure"
    );
    assert!(durable.queued_prompts[0].engram_evaluate.is_none());

    let reloaded = {
        let inner = state.inner.lock().unwrap();
        let encoded = serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap();
        serde_json::from_slice::<PersistedState>(&encoded)
            .unwrap()
            .into_inner()
            .unwrap()
    };
    let reloaded_record = &reloaded.sessions[reloaded.find_session_index(&session).unwrap()];
    assert!(reloaded_record.engram.active_grant_id.is_none());
    assert_eq!(reloaded_record.queued_prompts.len(), 1);
    assert_eq!(
        reloaded_record.queued_prompts[0].pending_prompt.text,
        "successor after Fast discovery failure"
    );
    assert!(!reloaded_record.queued_prompts[0].has_engram_intent());

    let (retry_runtime, retry_receiver) = test_codex_runtime_handle("fast-rejection-successor");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(retry_runtime);
        record.session.status = SessionStatus::Idle;
        record.session.codex_fast_mode = false;
    }
    state.resume_session_queue(&session).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = phase_sync::receive(
        &retry_receiver,
        "freshly authorized successor provider send",
    ) else {
        panic!("successor must reach the provider only after fresh authorization")
    };
    assert_eq!(command.prompt, "successor after Fast discovery failure");
    assert!(retry_receiver.try_recv().is_err());

    let requests = transport.requests();
    let evaluate_keys = requests
        .iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .map(|request| request.request["idempotency_key"].clone())
        .collect::<Vec<_>>();
    let begin_keys = requests
        .iter()
        .filter(|request| request.request["operation"] == "turn_begin")
        .map(|request| request.request["idempotency_key"].clone())
        .collect::<Vec<_>>();
    assert_eq!(evaluate_keys.len(), 2);
    assert_eq!(begin_keys.len(), 2);
    assert_ne!(evaluate_keys[0], evaluate_keys[1]);
    assert_ne!(begin_keys[0], begin_keys[1]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
}

#[test]
fn post_receipt_handoff_waits_for_real_stop_failure_or_success() {
    for failed_stop in [true, false] {
        let (state, session, root_rx, _) = root_fixture([
            bind_reply("token"),
            grant_reply("grant"),
            begin_reply("grant"),
            checkpoint_reply("grant"),
            grant_reply("successor"),
            begin_reply("successor"),
        ]);
        let dispatch = root_dispatch(&state, &session, false);
        let generation = dispatch.engram_dispatch_generation();
        let active_turn_generation = dispatch.active_turn_generation();
        assert!(matches!(
            state.prepare_engram_turn_delivery_off_lock(&session, generation.unwrap()),
            EngramTurnDeliveryPreparation::Ready
        ));
        // This is the deterministic boundary AFTER the actual durable receipt,
        // not the older gated turn_begin window.
        let (runtime, receiver) = test_acp_runtime_handle(AcpAgent::OpenCode, "post-receipt-stop");
        let token = RuntimeToken::Acp(runtime.runtime_id.clone());
        let process = runtime.process.clone();
        let sender = runtime.input_tx.clone();
        let lifecycle = runtime.turn_lifecycle.clone();
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            assert!(record.engram.pending_dispatch.is_none());
            assert!(
                record.queued_prompts[0]
                    .engram_evaluate
                    .as_ref()
                    .unwrap()
                    .begun_grant_id
                    .is_some()
            );
            record.runtime = SessionRuntime::Acp(runtime);
        }
        let failure =
            failed_stop.then(|| force_test_kill_child_process_failure(&process, "OpenCode"));
        if !failed_stop {
            queue_test_engram_prompt(
                &state,
                &session,
                "successor prompt",
                QueuedPromptSource::User,
                None,
            );
        }
        let stop_gate = install_test_stop_fence_gate(&state, &session);
        std::thread::scope(|scope| {
            let stop = scope.spawn(|| state.stop_session(&session));
            stop_gate.wait_until_claimed();
            handoff_prepared_turn_dispatch(
                &state,
                TurnDispatch::PersistentAcp {
                    active_turn_generation,
                    engram_dispatch_generation: generation,
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
                        prompt: "exact admitted prompt".into(),
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
                [DeferredStopCallback::EngramHandoff { .. }]
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
                vec!["exact admitted prompt".to_owned()]
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
                assert!(!record.queued_prompts[0].has_engram_intent());
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
            assert!(!command.prompt.contains("Root controlled turn"));
            assert!(root_rx.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn unresolved_fast_post_receipt_handoff_obeys_public_stop_outcome() {
    for failed_stop in [true, false] {
        let (state, session, _root_receiver, _) = root_fixture([
            bind_reply("token"),
            grant_reply("grant"),
            begin_reply("grant"),
            checkpoint_reply("grant"),
        ]);
        let (runtime, receiver) = test_codex_runtime_handle("fast-stop-runtime");
        let process = runtime.process.clone();
        let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.session.model = "catalog-model".into();
            record.session.codex_fast_mode = true;
            record.session.model_options.clear();
            record.runtime = SessionRuntime::Codex(runtime);
        }
        let dispatch = root_dispatch(&state, &session, false);
        let generation = dispatch.engram_dispatch_generation().unwrap();
        assert!(matches!(
            state.prepare_engram_turn_delivery_off_lock(&session, generation),
            EngramTurnDeliveryPreparation::Ready
        ));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].queued_prompts[0].source = QueuedPromptSource::Orchestrator;
            sync_pending_prompts(&mut inner.sessions[index]);
        }
        queue_test_engram_prompt(
            &state,
            &session,
            "ordinary orchestrator successor",
            QueuedPromptSource::Orchestrator,
            None,
        );
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
            assert_eq!(
                record.queued_prompts.len(),
                1,
                "preliminary Stop cleanup keeps only its admitted Orchestrator owner"
            );
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "Root controlled turn"
            );
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            assert!(matches!(
                record.deferred_stop_callbacks.as_slice(),
                [DeferredStopCallback::EngramHandoff { .. }]
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
            assert_eq!(command.prompt, "Root controlled turn");
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
    }
}

#[tokio::test]
async fn public_failed_stop_restores_admitted_handoff_before_replay() {
    for orchestrator_source in [false, true] {
        assert_public_failed_stop_restores_admitted_handoff(true, orchestrator_source).await;
    }
}

#[tokio::test]
async fn public_failed_stop_restores_admitted_owner_before_handoff_arrives() {
    for orchestrator_source in [false, true] {
        assert_public_failed_stop_restores_admitted_handoff(false, orchestrator_source).await;
    }
}

async fn assert_public_failed_stop_restores_admitted_handoff(
    callback_first: bool,
    orchestrator_source: bool,
) {
    let (state, session, root_rx, _) = root_fixture([
        bind_reply("token"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation();
    let active_turn_generation = dispatch.active_turn_generation();
    assert!(matches!(
        state.prepare_engram_turn_delivery_off_lock(&session, generation.unwrap()),
        EngramTurnDeliveryPreparation::Ready
    ));
    if orchestrator_source {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].queued_prompts[0].source = QueuedPromptSource::Orchestrator;
        sync_pending_prompts(&mut inner.sessions[index]);
    }
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
                        assert_eq!(record.queued_prompts.len(), 1);
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
                prompt: "public admitted prompt".into(),
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
                if !record.runtime_stop_in_progress && record.queued_prompts.is_empty() {
                    assert_eq!(record.session.status, SessionStatus::Active);
                    assert!(record.runtime.matches_runtime_token(&token));
                    assert!(record.session.messages.iter().any(|message| matches!(message,
                        Message::Text { text, .. } if text.contains("Stop failed in the background"))));
                    break;
                }
                assert_ne!(record.session.status, SessionStatus::Error, "public Stop must not discard admitted handoff");
            }
            events.recv().await.unwrap();
        }
    }).await.expect("public Stop rollback did not release handoff");
    state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
    let prompts = receiver
        .try_iter()
        .filter_map(|command| match command {
            AcpRuntimeCommand::Prompt(command) => Some(command.prompt),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(prompts, ["public admitted prompt"]);
    assert!(root_rx.try_recv().is_err());
}

#[tokio::test]
async fn successful_public_stop_never_retires_a_replacement_for_its_orchestrator_owner() {
    let (state, session, root_rx, _) = root_fixture([
        bind_reply("token"),
        grant_reply("grant"),
        begin_reply("grant"),
        checkpoint_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let generation = dispatch.engram_dispatch_generation().unwrap();
    assert!(matches!(
        state.prepare_engram_turn_delivery_off_lock(&session, generation),
        EngramTurnDeliveryPreparation::Ready
    ));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].queued_prompts[0].source = QueuedPromptSource::Orchestrator;
        sync_pending_prompts(&mut inner.sessions[index]);
    }
    queue_test_engram_prompt(
        &state,
        &session,
        "replacement user successor",
        QueuedPromptSource::User,
        None,
    );
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
}

#[test]
fn retained_authority_mismatch_stays_paused_cancelable_for_root_and_child() {
    for child in [false, true] {
        for lost_bind in [false, true] {
            let mut replies = Vec::new();
            if child {
                replies.push(bind_reply("parent"));
            }
            if !lost_bind {
                replies.push(bind_reply("token"));
            }
            replies.push(ScriptedEngramControlResponse::Reply(Err(
                EngramTransportError::deadline("lost authorization"),
            )));
            if child && lost_bind {
                // The first child bind is eager setup, before a queue record
                // exists. Lose the queued retry too so this case owns durable
                // retained bind input rather than only a backoff/wait flag.
                replies.push(ScriptedEngramControlResponse::Reply(Err(
                    EngramTransportError::deadline("lost queued bind"),
                )));
            }
            let (state, parent, receiver, transport) = root_fixture(replies);
            let (session, delegation) = if child {
                let created = state
                    .create_read_only_delegation(
                        &parent,
                        CreateDelegationRequest {
                            prompt: "held child".into(),
                            title: None,
                            cwd: None,
                            agent: Some(Agent::Codex),
                            model: None,
                            mode: Some(DelegationMode::Explorer),
                            write_policy: Some(DelegationWritePolicy::ReadOnly),
                        },
                    )
                    .unwrap();
                (
                    created.delegation.child_session_id,
                    Some(created.delegation.id),
                )
            } else {
                let dispatch = root_dispatch(&state, &parent, false);
                deliver_turn_dispatch(&state, dispatch).unwrap();
                (parent.clone(), None)
            };
            if child && lost_bind {
                {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&session).unwrap();
                    inner.sessions[index].engram.next_bind_retry_at = None;
                }
                state.resume_session_queue(&session).unwrap();
            }
            let (prompt_id, retained) = {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let project_id = inner.sessions[index].session.project_id.clone().unwrap();
                let project = inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project_id)
                    .unwrap();
                project.engram.as_mut().unwrap().binary_path =
                    Some("changed-engram-authority-binary".into());
                let record = &mut inner.sessions[index];
                record.engram.next_bind_retry_at = None;
                let queued = &record.queued_prompts[0];
                assert!(
                    queued.has_engram_intent(),
                    "fixture must retain authorization before settings change"
                );
                (
                    queued.pending_prompt.id.clone(),
                    serde_json::to_value((&queued.engram_bind, &queued.engram_evaluate)).unwrap(),
                )
            };
            let requests_before = transport.requests().len();
            state.resume_session_queue(&session).unwrap();
            if child {
                state
                    .refresh_delegation_for_child_session(&session)
                    .unwrap();
            }
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Idle);
                assert!(record.orchestrator_auto_dispatch_blocked);
                assert_eq!(record.queued_prompts.len(), 1);
                assert!(record.queued_prompts[0].engram_interrupted);
                assert_eq!(
                    serde_json::to_value((
                        &record.queued_prompts[0].engram_bind,
                        &record.queued_prompts[0].engram_evaluate
                    ))
                    .unwrap(),
                    retained
                );
                assert!(record.session.queue_paused);
                assert!(record.session.pending_prompts[0].engram_interrupted);
                if let Some(id) = delegation.as_ref() {
                    assert_eq!(
                        inner.delegations[inner.find_delegation_index(id).unwrap()].status,
                        DelegationStatus::Running
                    );
                }
            }
            assert_eq!(
                transport.requests().len(),
                requests_before,
                "mismatched authority must never transmit"
            );
            assert!(receiver.try_recv().is_err());
            state.cancel_queued_prompt(&session, &prompt_id).unwrap();
            let inner = state.inner.lock().unwrap();
            assert!(
                inner.sessions[inner.find_session_index(&session).unwrap()]
                    .queued_prompts
                    .is_empty()
            );
        }
    }
}

#[test]
fn held_child_initial_and_followup_authorization_survive_polling_then_resume() {
    for followup in [false, true] {
        for failure in ["defer", "bind", "evaluate", "begin"] {
            let mut responses = vec![bind_reply("parent")];
            if followup {
                responses.extend([
                    bind_reply("child"),
                    grant_reply("first"),
                    begin_reply("first"),
                    checkpoint_reply("first"),
                ]);
            }
            if failure != "bind" && !followup {
                responses.push(bind_reply("child"));
            }
            if failure == "begin" {
                responses.push(grant_reply("retry"));
            }
            responses.push(if failure == "defer" {
                defer_reply("busy")
            } else {
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                    "lost reply",
                )))
            });
            if failure == "bind" {
                responses.push(rebind_reply("child"));
            }
            responses.push(grant_reply("retry"));
            responses.push(begin_reply("retry"));
            let (state, parent, receiver, transport) = root_fixture(responses);
            let created = state
                .create_read_only_delegation(
                    &parent,
                    CreateDelegationRequest {
                        prompt: "initial child prompt".into(),
                        title: Some("held child".into()),
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Explorer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
                .unwrap();
            let child = created.delegation.child_session_id;
            let delegation = created.delegation.id;
            if followup {
                assert!(matches!(
                    receive_synchronous_engram_prompt(&state, &receiver, "initial prompt").unwrap(),
                    CodexRuntimeCommand::Prompt { .. }
                ));
                let token = {
                    let inner = state.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&child).unwrap()]
                        .runtime
                        .runtime_token()
                        .unwrap()
                };
                finish_delegation_child_with_assistant_text(&state, &child, "initial completed");
                {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&child).unwrap();
                    inner.sessions[index].session.status = SessionStatus::Active;
                }
                state
                    .finish_turn_ok_if_runtime_matches(&child, &token)
                    .unwrap();
                state.refresh_delegation_for_child_session(&child).unwrap();
                {
                    let inner = state.inner.lock().unwrap();
                    assert_eq!(
                        inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
                        DelegationStatus::Completed
                    );
                }
                if failure == "bind" {
                    let mut inner = state.inner.lock().unwrap();
                    let index = inner.find_session_index(&child).unwrap();
                    inner.sessions[index].engram.rebind_required = true;
                    inner.sessions[index].engram.routing_token = None;
                }
                state
                    .followup_delegation(&parent, &delegation, "follow-up child prompt".into())
                    .unwrap();
            }
            state.refresh_delegation_for_child_session(&child).unwrap();
            let saved = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
                assert_eq!(
                    record.session.status,
                    SessionStatus::Idle,
                    "{failure}, followup={followup}"
                );
                assert!(record.orchestrator_auto_dispatch_blocked);
                assert_eq!(record.queued_prompts.len(), 1);
                assert!(!record.queued_prompts[0].engram_interrupted);
                assert_eq!(
                    inner.delegations[inner.find_delegation_index(&delegation).unwrap()].status,
                    DelegationStatus::Running
                );
                serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap()
            };
            assert!(receiver.try_recv().is_err());
            if failure == "bind" {
                // Initial child setup also makes a best-effort eager bind
                // before its queued admission. Advance that existing retry
                // backoff deterministically; do not synchronize with sleeps.
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&child).unwrap();
                inner.sessions[index].engram.next_bind_retry_at = None;
            }
            state.resume_session_queue(&child).unwrap();
            assert!(matches!(
                receiver.try_recv().unwrap(),
                CodexRuntimeCommand::Prompt { .. }
            ));
            assert!(receiver.try_recv().is_err());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&child).unwrap()];
            assert!(record.queued_prompts.is_empty());
            assert_eq!(
                record
                    .session
                    .messages
                    .iter()
                    .filter(|message| Some(message.id()) == saved["id"].as_str())
                    .count(),
                1
            );
            let requests = transport.requests();
            if failure == "evaluate" || failure == "begin" {
                let operation = if failure == "evaluate" {
                    "turn_evaluate"
                } else {
                    "turn_begin"
                };
                let matching = requests
                    .iter()
                    .filter(|r| r.request["operation"] == operation)
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>();
                assert_eq!(matching[0].request, matching[1].request, "exact replay");
            }
        }
    }
}

#[test]
fn retained_mailbox_coverage_deduplicates_equal_boundary_and_keeps_newer_separate() {
    for newer in [false, true] {
        assert_retained_mailbox_coverage(newer);
    }
}

fn assert_retained_mailbox_coverage(newer: bool) {
    let (state, session, receiver, _) = root_fixture([
        bind_reply("token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("lost evaluate"))),
        grant_reply("retry"),
        begin_reply("retry"),
    ]);
    let coordination_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&coordination_path).unwrap()),
        ..state
    };
    let sender = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state
        .append_mailbox_message_and_notify(
            &sender,
            SendMailboxMessageRequest {
                target_session_id: session.clone(),
                message: "one inbound".into(),
                idempotency_key: "retained-one".into(),
                topic: None,
                state_stamp: None,
                class: Some("routine".into()),
            },
        )
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let dispatch = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap()
        .unwrap()
        .dispatch;
    let notification = dispatch.mailbox_notification().unwrap().clone();
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let before = {
        let inner = state.inner.lock().unwrap();
        serde_json::to_value(
            &inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0],
        )
        .unwrap()
    };
    for _ in 0..2 {
        state
            .requeue_rejected_mailbox_notification(&notification)
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
    }
    assert!(receiver.try_recv().is_err());
    if newer {
        state
            .append_mailbox_message_and_notify(
                &sender,
                SendMailboxMessageRequest {
                    target_session_id: session.clone(),
                    message: "newer inbound".into(),
                    idempotency_key: "retained-two".into(),
                    topic: None,
                    state_stamp: None,
                    class: Some("routine".into()),
                },
            )
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0]).unwrap(),
            before
        );
    }
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .queued_prompts
            .len(),
        usize::from(newer)
    );
}

#[test]
fn fresh_prompt_preserves_existing_message_index_allocation() {
    let (state, session, _, _) = root_fixture([bind_reply("token"), grant_reply("grant")]);
    let capacity = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].message_positions.reserve(4096);
        assert!(
            cached_message_index_on_record(&inner.sessions[index], "fresh-missing-id").is_none()
        );
        inner.sessions[index].message_positions.capacity()
    };
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(cached_message_index_on_record(record, "another-fresh-id").is_none());
        assert_eq!(record.message_positions.capacity(), capacity);
    }
    // Append has its own pre-existing index maintenance; only the preliminary
    // absent-ID membership check must avoid an additional rebuilding pass.
    let _dispatch = root_dispatch(&state, &session, false);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    for (id, index) in &record.message_positions {
        assert_eq!(record.session.messages[*index].id(), id);
    }
}
