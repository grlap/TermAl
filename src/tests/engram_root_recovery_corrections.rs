//! Regression boundaries for retained authorization ownership and exact replay.
//! Uses the parent's scripted transports; no live store or provider process.
use super::*;

struct WorkFocusGate {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl EngramControlTransport for WorkFocusGate {
    fn shutdown_session(&self, _: &str) {}

    fn request(
        &self,
        _: &EngramConnectionConfig,
        _: &EngramControlRequest,
        _: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        panic!("Stop during work focus must prevent wire admission");
    }

    fn read_work_binding(
        &self,
        _: &EngramConnectionConfig,
        _: EngramBindingPreference<'_>,
        _: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
        Ok(None)
    }
}

fn retained_mailbox_request(
    target_session_id: &str,
    idempotency_key: &str,
    topic: &str,
) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target_session_id.to_owned(),
        message: format!("durable body for {topic}"),
        idempotency_key: idempotency_key.to_owned(),
        topic: Some(topic.to_owned()),
        state_stamp: None,
        class: Some("routine".to_owned()),
    }
}

#[test]
fn early_stop_keeps_interrupted_head_ahead_of_mailbox_and_new_user_prompts() {
    let (state, session, receiver, _) = root_fixture([]);
    let coordination_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&coordination_path).unwrap()),
        ..state
    };
    queue_test_engram_prompt(
        &state,
        &session,
        "original",
        QueuedPromptSource::Orchestrator,
        None,
    );
    let original = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .clone()
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    state.install_control_test_transport(Arc::new(WorkFocusGate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    }));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| state.start_next_queued_turn_off_lock(&session, false, false));
        entered_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        state.request_stop_session(&session).unwrap();
        // Both a fresh mailbox insertion and promotion of an existing wake
        // must respect the interrupted head, despite there being no wire intent.
        for sequence in [1, 2] {
            state
                .queue_mailbox_wakeups_for_session_outcome(
                    &session,
                    vec![MailboxUnreadWakeup {
                        mailbox_id: "later-mailbox".into(),
                        message_id: format!("mail-{sequence}"),
                        sequence,
                        unread_count: sequence,
                        sender_session_id: "sender".into(),
                        sender_name: "Sender".into(),
                        topic: None,
                    }],
                    MailboxWakeupRecovery::NeverWoken,
                    true,
                )
                .unwrap();
        }
        queue_test_engram_prompt(&state, &session, "new user", QueuedPromptSource::User, None);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            prioritize_user_queued_prompts(record);
            let head = &record.queued_prompts[0];
            assert!(!head.has_engram_intent());
            assert!(head.engram_interrupted);
            assert_eq!(
                serde_json::to_value(&head.pending_prompt).unwrap(),
                serde_json::to_value(&original).unwrap()
            );
            assert_eq!(record.queued_prompts.len(), 3);
        }
        release_tx.send(()).unwrap();
        assert!(worker.join().unwrap().unwrap().is_none());
    });
    assert!(
        state
            .start_next_queued_turn_off_lock(&session, true, false)
            .unwrap()
            .is_none()
    );
    assert!(receiver.try_recv().is_err());
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.session.pending_prompts[0].engram_interrupted);
    }
    state.cancel_queued_prompt(&session, &original.id).unwrap();
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.session.preview, "Engram authorization canceled.");
    assert!(
        record.orchestrator_auto_dispatch_blocked,
        "cancel must preserve intentional pause"
    );
    assert!(
        !record
            .session
            .pending_prompts
            .iter()
            .any(|prompt| prompt.engram_interrupted)
    );
}

#[test]
fn deferred_mailbox_wake_is_immutable_while_a_newer_wake_queues_separately() {
    let (base, target, receiver, _) = root_fixture([
        bind_reply("token"),
        defer_reply("wait"),
        grant_reply("retry"),
        begin_reply("retry"),
    ]);
    let coordination_path = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&coordination_path).unwrap()),
        ..base
    };
    let sender = test_session_id(&state, Agent::Claude);

    let first = state
        .append_mailbox_message_and_notify(
            &sender,
            retained_mailbox_request(&target, "defer-first", "first"),
        )
        .unwrap();
    let (original_prompt, original_promotion) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        let queued = record.queued_prompts.front().unwrap();
        assert!(queued.engram_waiting);
        assert!(!queued.has_engram_intent());
        assert!(queued.is_engram_retained());
        assert!(record.session.pending_prompts[0].is_engram_retained);
        (
            serde_json::to_value(&queued.pending_prompt).unwrap(),
            queued.promoted_message_index,
        )
    };

    let second = state
        .append_mailbox_message_and_notify(
            &sender,
            retained_mailbox_request(&target, "defer-second", "second"),
        )
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(record.queued_prompts.len(), 2);
        let retained = &record.queued_prompts[0];
        assert_eq!(
            serde_json::to_value(&retained.pending_prompt).unwrap(),
            original_prompt
        );
        assert_eq!(retained.promoted_message_index, original_promotion);
        let sequences = record
            .queued_prompts
            .iter()
            .map(|queued| {
                queued
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|source| source.mailbox.as_ref())
                    .map(|mailbox| mailbox.sequence)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(sequences, [first.sequence, second.sequence]);
        assert!(retained.is_engram_retained());
        assert!(!record.queued_prompts[1].is_engram_retained());
    }

    state.resume_session_queue(&target).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0]
            .pending_prompt
            .source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .map(|mailbox| mailbox.sequence),
        Some(second.sequence)
    );
    assert_eq!(
        record
            .session
            .messages
            .iter()
            .filter(|message| matches!(message,
                Message::Text { source: Some(source), .. }
                    if source.mailbox.as_ref().is_some_and(|mailbox|
                        mailbox.mailbox_id == first.mailbox_id
                            && mailbox.sequence == first.sequence)))
            .count(),
        1,
        "resuming the deferred wake must promote its exact correspondence once"
    );
}

#[test]
fn pre_bind_stop_keeps_the_old_mailbox_wake_immutable_and_queues_its_successor() {
    let (base, target, receiver, _) = root_fixture([]);
    let coordination_path = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&coordination_path).unwrap()),
        ..base
    };
    let sender = test_session_id(&state, Agent::Claude);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    state.install_control_test_transport(Arc::new(WorkFocusGate {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    }));

    let first = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            state.append_mailbox_message_and_notify(
                &sender,
                retained_mailbox_request(&target, "pre-bind-first", "first"),
            )
        });
        entered_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        state.request_stop_session(&target).unwrap();
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap()
    });
    let (original_prompt, original_promotion) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        let queued = record.queued_prompts.front().unwrap();
        assert!(queued.engram_interrupted);
        assert!(!queued.has_engram_intent());
        assert!(queued.is_engram_retained());
        (
            serde_json::to_value(&queued.pending_prompt).unwrap(),
            queued.promoted_message_index,
        )
    };

    let second = state
        .append_mailbox_message_and_notify(
            &sender,
            retained_mailbox_request(&target, "pre-bind-second", "second"),
        )
        .unwrap();
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
    assert_eq!(record.queued_prompts.len(), 2);
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0].pending_prompt).unwrap(),
        original_prompt
    );
    assert_eq!(
        record.queued_prompts[0].promoted_message_index,
        original_promotion
    );
    let sequences = record
        .queued_prompts
        .iter()
        .map(|queued| {
            queued
                .pending_prompt
                .source
                .as_ref()
                .and_then(|source| source.mailbox.as_ref())
                .map(|mailbox| mailbox.sequence)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(sequences, [first.sequence, second.sequence]);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn claude_and_acp_handoff_supersession_is_success_without_provider_or_rejection() {
    for acp in [false, true] {
        for supersession in ["generation", "stopping", "idle", "grant"] {
            let (state, session, receiver, _) = root_fixture([
                bind_reply("token"),
                grant_reply("grant"),
                begin_reply("grant"),
            ]);
            let dispatch = root_dispatch(&state, &session, false);
            let generation = dispatch.engram_dispatch_generation();
            let active_turn_generation = dispatch.active_turn_generation();
            let runtime_token = dispatch.runtime_token().clone();
            assert!(matches!(
                state.prepare_engram_turn_delivery_off_lock(&session, generation.unwrap()),
                EngramTurnDeliveryPreparation::Ready
            ));
            let before = {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                match supersession {
                    "generation" => record.engram.dispatch_generation += 1,
                    "stopping" => record.runtime_stop_in_progress = true,
                    "idle" => record.session.status = SessionStatus::Idle,
                    "grant" => record.engram.active_grant_id = None,
                    _ => unreachable!(),
                }
                serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
            };
            let (claude_tx, claude_rx) = mpsc::channel();
            let (acp_tx, acp_rx) = mpsc::channel();
            let successor_active = supersession == "generation";
            let turn_lifecycle = Arc::new((Mutex::new(successor_active), Condvar::new()));
            let dispatch = if acp {
                TurnDispatch::PersistentAcp {
                    federation_handoff: None,
                    opening_prompt: None,
                    active_turn_generation,
                    engram_dispatch_generation: generation,
                    runtime_token,
                    session_id: session.clone(),
                    mailbox_notification: None,
                    sender: acp_tx,
                    turn_lifecycle: turn_lifecycle.clone(),
                    command: AcpPromptCommand {
                        cwd: String::new(),
                        cursor_mode: None,
                        model: String::new(),
                        opencode_effort: None,
                        opencode_mode: None,
                        prompt: "must not deliver".into(),
                        resume_session_id: None,
                    },
                }
            } else {
                TurnDispatch::PersistentClaude {
                    federation_handoff: None,
                    opening_prompt: None,
                    active_turn_generation,
                    engram_dispatch_generation: generation,
                    runtime_token,
                    session_id: session.clone(),
                    mailbox_notification: None,
                    sender: claude_tx,
                    command: ClaudePromptCommand {
                        attachments: vec![],
                        replay_generation: "test".into(),
                        text: "must not deliver".into(),
                        turn_generation: 1,
                    },
                }
            };
            handoff_prepared_turn_dispatch(&state, dispatch).unwrap();
            assert!(claude_rx.try_recv().is_err());
            assert!(acp_rx.try_recv().is_err());
            assert!(receiver.try_recv().is_err());
            assert_eq!(*turn_lifecycle.0.lock().unwrap(), successor_active);
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(
                before,
                serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap(),
                "supersession must not terminalize or rewrite the current owner"
            );
        }
    }
}

fn change_evaluator_defaults(state: &AppState, session: &str) {
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(session).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap()
    };
    state
        .update_acceptance_defaults(
            &project,
            AcceptanceEvaluatorDefaults {
                default_mode: Some(AcceptanceEvaluationMode::IndependentSession),
                evaluator_agent: Some(Agent::Claude),
                evaluator_model: Some("changed-default".into()),
            },
        )
        .unwrap();
}

/// A head whose begin is durably recorded begun: cold recovery persists the
/// interruption before its restart checkpoint closes the remote begun grant.
#[test]
fn cold_recovery_persists_interruption_before_closing_remote_begun_grant() {
    let (state, session, receiver, _) = root_fixture([
        bind_reply("token"),
        grant_reply("grant"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("lost begin"))),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    mark_retained_begin_begun(&state, &session, "grant");
    let transport = ScriptedEngramControlTransport::new([
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "phase": "turn_open", "open_grant_id": "grant", "open_grant_state": "begun"
        }))),
        checkpoint_reply("grant"),
    ]);
    state.install_control_test_transport(transport.clone());
    let (mut target, owner) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index] = PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .unwrap();
        (
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .unwrap(),
            EngramQueuedAdmissionOwner::capture(&inner.sessions[index]).unwrap(),
        )
    };
    target.admission_started_at = Some(std::time::Instant::now());
    let (persist_tx, persist_rx) = mpsc::channel();
    let state = AppState {
        persist_tx,
        ..state
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| state.restore_queued_engram_target(&mut target, &owner));
        let mut batch = PersistFenceBatch::default();
        loop {
            let request = persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            let is_fence = matches!(&request, PersistRequest::Fence(_));
            batch.accept(request);
            if is_fence {
                break;
            }
        }
        assert_eq!(
            operations(&transport),
            ["session_status"],
            "must not checkpoint before durable interruption"
        );
        let durable: PersistedSessionRecord =
            serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
        assert!(!durable.queued_prompts[0].engram_interrupted);
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .message
                .contains("Interrupted/unknown")
        );
    });
    assert_eq!(
        operations(&transport),
        ["session_status", "turn_checkpoint"]
    );
    let durable: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(durable.queued_prompts[0].engram_interrupted);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn evaluator_defaults_do_not_invalidate_unknown_bind_or_evaluate_replay() {
    for lost_bind in [true, false] {
        let lost =
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("lost reply")));
        let responses = if lost_bind {
            vec![
                lost,
                bind_reply("token"),
                grant_reply("grant"),
                begin_reply("grant"),
            ]
        } else {
            vec![
                bind_reply("token"),
                lost,
                grant_reply("grant"),
                begin_reply("grant"),
            ]
        };
        let (state, session, receiver, transport) = root_fixture(responses);
        let dispatch = root_dispatch(&state, &session, false);
        deliver_turn_dispatch(&state, dispatch).unwrap();
        change_evaluator_defaults(&state, &session);
        state.resume_session_queue(&session).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(receiver.try_recv().is_err());
        let requests = transport.requests();
        let original = if lost_bind { 0 } else { 1 };
        assert_eq!(requests[original].request, requests[original + 1].request);
    }
}

#[test]
fn evaluator_defaults_saved_during_bind_preserve_its_owner() {
    let (state, session, receiver, _) = root_fixture([]);
    let (step, gate) = gated_engram_step("session_bind", bind_reply("token"));
    let transport = GatedEngramControlTransport::new([
        step,
        immediate_engram_step("turn_evaluate", grant_reply("grant")),
        immediate_engram_step("turn_begin", begin_reply("grant")),
    ]);
    state.install_control_test_transport(transport);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let dispatch = root_dispatch(&state, &session, false);
            deliver_turn_dispatch(&state, dispatch).unwrap();
        });
        gate.wait();
        change_evaluator_defaults(&state, &session);
        gate.release();
        worker.join().unwrap();
    });
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
}

#[test]
fn admission_guard_retries_competing_drain_and_releases_on_unwind() {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(&state, &session, "queued", QueuedPromptSource::User, None);
    let mut calls = 0;
    let result = state
        .with_queued_engram_admission(&session, || {
            calls += 1;
            if calls == 1 {
                // This drain sampled Active, then completion made the session Idle.
                // The completion's competing drain arrives before guard release.
                assert!(
                    state
                        .with_queued_engram_admission(&session, || panic!("single flight"))?
                        .is_none()
                );
            }
            Ok(None)
        })
        .unwrap();
    assert!(result.is_none());
    assert_eq!(calls, 2, "competing wake must be consumed, not discarded");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = state.with_queued_engram_admission(&session, || panic!("injected off-lock unwind"));
    }));
    assert!(panic.is_err());
    let inner = state
        .inner
        .lock()
        .expect("state mutex not poisoned by off-lock unwind");
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .admission_in_progress
            .is_none()
    );
}

#[test]
fn ordinary_drain_is_not_serialized_and_waiting_admission_is_not_auto_retried() {
    let (state, session, _, _) = root_fixture([]);
    queue_test_engram_prompt(&state, &session, "queued", QueuedPromptSource::User, None);
    let mut calls = 0;
    state
        .with_queued_engram_admission(&session, || {
            calls += 1;
            state.with_queued_engram_admission(&session, || panic!("single flight"))?;
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
            Ok(None)
        })
        .unwrap();
    assert_eq!(
        calls, 1,
        "explicit Waiting/Unknown pause wins over automatic wake"
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.engram.is_some())
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
    }
    let mut nested = false;
    state
        .with_queued_engram_admission(&session, || {
            state.with_queued_engram_admission(&session, || {
                nested = true;
                Ok(None)
            })?;
            Ok(None)
        })
        .unwrap();
    assert!(
        nested,
        "ordinary sessions must not lose a concurrent drain to the Engram latch"
    );
}

#[test]
fn stale_begin_retirement_cannot_clear_replacement_evaluation() {
    let (state, session, receiver, _) =
        root_fixture([bind_reply("token"), grant_reply("old-grant")]);
    let dispatch = root_dispatch(&state, &session, false);
    let old_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    let (step, gate) = gated_engram_step("turn_begin", checkpoint_refusal_reply("stale_fence"));
    state.install_control_test_transport(GatedEngramControlTransport::new([step]));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        gate.wait();
        let (runtime_token, turn_generation, changed_path) = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            assert_eq!(record.session.status, SessionStatus::Active);
            assert!(record.active_turn_start_message_count.is_some());
            record.active_turn_mailbox_notification = Some(MailboxNotificationDelivery {
                mailbox_id: "canceled-mailbox".into(),
                session_id: session.clone(),
                through_sequence: 17,
            });
            let path = PathBuf::from(&record.session.workdir)
                .join("canceled.rs")
                .to_string_lossy()
                .into_owned();
            record
                .active_turn_file_changes
                .insert(path.clone(), WorkspaceFileChangeKind::Created);
            record.active_turn_file_change_grace_deadline =
                Some(std::time::Instant::now() + Duration::from_secs(30));
            (
                record.runtime.runtime_token(),
                record.active_turn_generation,
                path,
            )
        };
        state.cancel_queued_prompt(&session, &old_id).unwrap();
        // Cancel returns before the gated begin is released, without tearing
        // down the runtime or allowing its late response to own a successor.
        state.record_active_turn_file_changes(&[WorkspaceFileChangeEvent {
            path: changed_path,
            kind: WorkspaceFileChangeKind::Created,
            root_path: None,
            session_id: Some(session.clone()),
            mtime_ms: None,
            size_bytes: None,
        }]);
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert_eq!(record.runtime.runtime_token(), runtime_token);
            assert_eq!(record.active_turn_generation, turn_generation);
            assert!(record.active_turn_start_message_count.is_none());
            assert!(record.active_turn_mailbox_notification.is_none());
            assert!(record.active_turn_file_changes.is_empty());
            assert!(record.active_turn_file_change_grace_deadline.is_none());
        }
        queue_test_engram_prompt(
            &state,
            &session,
            "replacement",
            QueuedPromptSource::User,
            None,
        );
        let expected = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let target =
                AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                    .unwrap()
                    .unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let prepared = EngramQueuedEvaluate {
                connection: target.connection,
                settings: target.settings,
                operation_generation: None,
                begun_grant_id: None,
                prepared_begin: None,
                request: EngramControlRequest::TurnEvaluate {
                    routing_token: "replacement-token".into(),
                    intent_fingerprint: "replacement".into(),
                    requested_effects: vec![],
                    resource_intents: vec![],
                    purpose: "ordinary".into(),
                    idempotency_key: "replacement-key".into(),
                },
            };
            let expected = serde_json::to_value(&prepared).unwrap();
            inner.sessions[index].queued_prompts[0].engram_evaluate = Some(prepared);
            // Model successor-owned bookkeeping before the old reply arrives.
            let record = &mut inner.sessions[index];
            record.active_turn_generation = turn_generation + 1;
            record.active_turn_start_message_count = Some(999);
            record.active_turn_mailbox_notification = Some(MailboxNotificationDelivery {
                mailbox_id: "successor-mailbox".into(),
                session_id: session.clone(),
                through_sequence: 18,
            });
            record
                .active_turn_file_changes
                .insert("successor.rs".into(), WorkspaceFileChangeKind::Created);
            expected
        };
        gate.release();
        worker.join().unwrap().unwrap();
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0].engram_evaluate).unwrap(),
            expected
        );
        assert_eq!(record.active_turn_generation, turn_generation + 1);
        assert_eq!(record.active_turn_start_message_count, Some(999));
        assert_eq!(
            record
                .active_turn_mailbox_notification
                .as_ref()
                .unwrap()
                .through_sequence,
            18
        );
        assert!(record.active_turn_file_changes.contains_key("successor.rs"));
    });
    assert!(receiver.try_recv().is_err());
}

#[test]
fn disabled_control_after_restart_surfaces_retained_authorization_until_cancel() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("lost evaluate"))),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let prompt_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.engram.is_some())
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
        let index = inner.find_session_index(&session).unwrap();
        // Settings reset clears ephemeral state, but not the durable intent.
        inner.sessions[index].engram = EngramSessionState::default();
        inner.sessions[index] = PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .unwrap();
        assert!(inner.sessions[index].engram.recovered_admission);
        inner.sessions[index].queued_prompts[0]
            .pending_prompt
            .id
            .clone()
    };
    let error = state
        .resume_session_queue(&session)
        .err()
        .expect("control off must explain its hold");
    assert_eq!(error.status, StatusCode::CONFLICT);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.session.preview.contains("Engram control is off"));
        assert!(record.queued_prompts[0].is_engram_retained());
    }
    assert_eq!(operations(&transport), ["session_bind", "turn_evaluate"]);
    assert!(receiver.try_recv().is_err());
    state.cancel_queued_prompt(&session, &prompt_id).unwrap();
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
}

struct LostAdmissionReplyTransport {
    inner: Arc<StatefulEngramControlTransport>,
    operation: &'static str,
    lose_next: AtomicBool,
}

impl EngramControlTransport for LostAdmissionReplyTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let reply = self.inner.request(connection, request, timeout)?;
        if serde_json::to_value(request).unwrap()["operation"] == self.operation
            && self.lose_next.swap(false, Ordering::SeqCst)
        {
            return Err(EngramTransportError::deadline(
                "applied admission reply lost",
            ));
        }
        Ok(reply)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn reenabled_control_resume_recovers_retained_evaluate_without_cancel() {
    for lost_operation in ["session_bind", "turn_evaluate"] {
        let (state, session, receiver, _) = root_fixture([]);
        let (project_id, root, settings) = {
            let mut inner = state.inner.lock().unwrap();
            let project = inner
                .projects
                .iter_mut()
                .find(|p| p.engram.is_some())
                .unwrap();
            let root = PathBuf::from(&project.root_path);
            fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
            let settings = real_fixture_engram_settings(&root);
            project.engram = Some(settings.clone());
            (project.id.clone(), root, settings)
        };
        let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
        // Start with the same validated store identity that the public update
        // writes, so the test changes only the control tier, not authority.
        let mut settings = settings;
        let project = state
            .inner
            .lock()
            .unwrap()
            .find_project(&project_id)
            .unwrap()
            .clone();
        settings.authority_store_key = Some(
            validate_engram_project_enablement(
                &project,
                &settings,
                &state.engram_host_launch_workdir(),
            )
            .unwrap(),
        );
        state
            .inner
            .lock()
            .unwrap()
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .unwrap()
            .engram = Some(settings.clone());
        let transport = Arc::new(LostAdmissionReplyTransport {
            inner: StatefulEngramControlTransport::new(),
            operation: lost_operation,
            lose_next: AtomicBool::new(true),
        });
        state.install_control_test_transport(transport.clone());
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert!(receiver.try_recv().is_err());
        let (prompt_id, original_request) = {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            (
                record.queued_prompts[0].pending_prompt.id.clone(),
                transport.inner.requests().last().unwrap().request.clone(),
            )
        };
        let mut disabled = settings.clone();
        disabled.turn_gated_control = false;
        state
            .update_project_engram_settings(&project_id, disabled)
            .unwrap();
        let error = state
            .resume_session_queue(&session)
            .err()
            .expect("control off holds the retained intent");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(receiver.try_recv().is_err());
        let count_before_enable = transport.inner.requests().len();
        state
            .update_project_engram_settings(&project_id, settings)
            .unwrap();
        assert_eq!(
            transport.inner.requests().len(),
            count_before_enable,
            "settings must not rotate the retained binding before Resume"
        );
        {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
            assert!(record.queued_prompts[0].engram_interrupted);
        }
        state
            .resume_session_queue(&session)
            .expect("retained prompt can resume");
        assert!(matches!(
            receiver
                .try_recv()
                .expect("Resume sends the retained prompt"),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(
            receiver.try_recv().is_err(),
            "no duplicate provider handoff"
        );
        state.resume_session_queue(&session).unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "repeated Resume cannot resend an active turn"
        );
        let requests = transport.inner.requests();
        let replays: Vec<_> = requests
            .iter()
            .filter(|r| r.request["operation"] == lost_operation)
            .collect();
        assert_eq!(replays.len(), 2, "only the exact lost operation is retried");
        assert_eq!(replays[1].request, original_request);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.request["operation"] == "turn_begin")
                .count(),
            1
        );
        assert!(transport.inner.grant_state(&session).1.is_some());
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .queued_prompts
                .is_empty()
        );
    }
}

#[test]
fn interrupted_head_without_wire_intent_requires_recorded_begin_format() {
    for records_begins in [false, true] {
        let (state, session, _, _) = root_fixture([]);
        queue_test_engram_prompt(
            &state,
            &session,
            "Stopped before admission",
            QueuedPromptSource::User,
            None,
        );
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.begins_recorded = records_begins;
        record.queued_prompts[0].engram_interrupted = true;
        record.queued_prompts[0].promotion_disposition_known = true;
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(
            matches!(
                engram_begin_preparation_resume(record),
                EngramBeginPreparationResume::Releasable
            ) == records_begins
        );
    }
}

#[test]
fn interrupted_retained_resume_preserves_unresolved_grants_and_authority() {
    for problem in ["begun", "uncertain", "different_settings"] {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("token"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "lost evaluate",
            ))),
        ]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        let before = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.engram = EngramSessionState::default();
            let head = record.queued_prompts.front_mut().unwrap();
            head.engram_interrupted = true;
            match problem {
                "begun" => {
                    head.engram_evaluate.as_mut().unwrap().begun_grant_id =
                        Some("possibly-delivered".into())
                }
                "uncertain" => record.engram.uncertain_grant_id = Some("uncertain".into()),
                "different_settings" => {
                    head.engram_evaluate.as_mut().unwrap().connection.home = "different-home".into()
                }
                _ => unreachable!(),
            }
            serde_json::to_value(&record.queued_prompts[0].engram_evaluate).unwrap()
        };
        let error = state
            .resume_session_queue(&session)
            .err()
            .expect("unresolved authorization must be refused explicitly");
        assert_eq!(error.status, StatusCode::CONFLICT, "{problem}");
        assert!(
            receiver.try_recv().is_err(),
            "{problem}: no provider handoff"
        );
        assert_eq!(
            operations(&transport),
            ["session_bind", "turn_evaluate"],
            "{problem}"
        );
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts[0].engram_interrupted);
        assert_eq!(
            serde_json::to_value(&record.queued_prompts[0].engram_evaluate).unwrap(),
            before
        );
    }
}

#[test]
fn resumed_cold_recovery_reports_a_remotely_begun_turn_without_delivery() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("lost evaluate"))),
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "phase": "turn_open", "open_grant_id": "remote-grant", "open_grant_state": "begun"
        }))),
        checkpoint_reply("remote-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram = EngramSessionState::default();
        inner.sessions[index].queued_prompts[0].engram_interrupted = true;
    }
    let error = state
        .resume_session_queue(&session)
        .err()
        .expect("Resume must expose the recovered hold");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "session_status",
            "turn_checkpoint"
        ]
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(record.orchestrator_auto_dispatch_blocked);
}

/// Commits, through the real writer, the prepared-Begin acknowledgement that
/// precedes every begin send, so a test can then drive the writer through the
/// pre-handoff receipt alone.
fn commit_prepared_begin_fence(
    state: &AppState,
    persist_rx: &mpsc::Receiver<PersistRequest>,
    cache: &mut SqlitePersistConnectionCache,
) {
    let mut batch = PersistFenceBatch::default();
    loop {
        let request = persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        let prepared = matches!(&request, PersistRequest::Fence(fence)
            if matches!(&fence.target, PersistFenceTarget::EngramAdmission { content, .. }
                if content["queue"]["engram_evaluate"]["prepared_begin"].is_object()));
        let is_fence = matches!(&request, PersistRequest::Fence(_));
        batch.accept(request);
        if is_fence {
            assert!(prepared, "the first fence acknowledges the prepared Begin");
            break;
        }
    }
    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
    persist_delta_with_fences(cache, state.persistence_path.as_path(), &delta, &mut batch)
        .unwrap();
}

fn writer_backed_checkpoint_fixture(
    responses: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> (RootFixture, mpsc::Receiver<PersistRequest>) {
    let (state, session, receiver, transport) = root_fixture(responses);
    let dispatch = root_dispatch(&state, &session, false);
    let (persist_tx, persist_rx) = mpsc::channel();
    let state = AppState {
        persist_tx,
        ..state
    };
    let mut cache = SqlitePersistConnectionCache::new();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        commit_prepared_begin_fence(&state, &persist_rx, &mut cache);
        let mut batch = PersistFenceBatch::default();
        // Drive the real persistence writer through the pre-handoff receipt,
        // then deliberately stop it before the queue-removal delta can commit.
        loop {
            let request = persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            let is_fence = matches!(&request, PersistRequest::Fence(_));
            batch.accept(request);
            if is_fence {
                break;
            }
        }
        let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
        worker.join().unwrap().unwrap();
    });
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    drop(cache);
    ((state, session, receiver, transport), persist_rx)
}

// The manual receiver stays connected until the finishing worker is joined:
// queued deltas cannot fall back to synchronous writes. Shutdown is only this
// fixture driver's completion marker, sent after finish has returned.
fn drive_writer_backed_checkpoint_finish(
    state: &AppState,
    session: &str,
    persist_rx: &mpsc::Receiver<PersistRequest>,
    transport: &ScriptedEngramControlTransport,
    commit: bool,
) -> Vec<(EngramExistingCheckpointProgress, usize)> {
    let token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        record.runtime.runtime_token().unwrap()
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = state.finish_turn_ok_if_runtime_matches(session, &token);
            state.persist_tx.send(PersistRequest::Shutdown).unwrap();
            result
        });
        let mut cuts = Vec::new();
        let mut batch = PersistFenceBatch::default();
        let mut cache = SqlitePersistConnectionCache::new();
        loop {
            let request = persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            let finished = matches!(&request, PersistRequest::Shutdown);
            let is_fence = matches!(&request, PersistRequest::Fence(_));
            if is_fence {
                let progress = {
                    let inner = state.inner.lock().unwrap();
                    inner.engram_session_obligations.checkpoint_operations.iter()
                        .find(|operation| operation.protection.local_session_id == session)
                        .map(|operation| operation.progress.clone())
                };
                let sent = transport.requests().iter()
                    .filter(|request| request.request["operation"] == "turn_checkpoint").count();
                cuts.push((progress, sent));
            }
            batch.accept(request);
            if is_fence || finished {
                if commit {
                    let delta = collect_persist_delta_from_shared_state(&state.inner, 0);
                    persist_delta_with_fences(&mut cache, state.persistence_path.as_path(),
                        &delta, &mut batch).unwrap();
                } else {
                    // No collection or write occurs after the genuine begun
                    // cut. Resolve the owned fence explicitly, not by a timer.
                    batch.fail(PersistFenceError::WorkerStopped);
                }
            }
            if finished { break; }
        }
        worker.join().unwrap().unwrap();
        cuts.into_iter().map(|(progress, sent)| {
            (progress.expect("each checkpoint fence must retain its canonical operation"), sent)
        }).collect()
    })
}

#[test]
fn writer_backed_handoff_checkpoint_crash_never_replays_durable_begun_prompt() {
    let ((state, session, receiver, transport), persist_rx) = writer_backed_checkpoint_fixture([
        bind_reply("token"), grant_reply("grant"), begin_reply("grant"),
        status_reply("sync_required"),
    ]);
    let before = fs::read(state.persistence_path.as_path()).unwrap();
    let begin = transport.requests().into_iter()
        .find(|request| request.request["operation"] == "turn_begin").unwrap().request;
    let durable_before = persisted_session_json(&state, &session);
    let cuts = drive_writer_backed_checkpoint_finish(&state, &session, &persist_rx, &transport, false);
    assert!(matches!(cuts.as_slice(), [(EngramExistingCheckpointProgress::Retained, 0)]));
    assert_eq!(fs::read(state.persistence_path.as_path()).unwrap(), before,
        "a failed first checkpoint fence cannot write the queued removal or send intent");
    assert_eq!(persisted_session_json(&state, &session), durable_before);
    assert_eq!(operations(&transport), ["session_bind", "turn_evaluate", "turn_begin"]);
    assert_eq!(begin["grant_id"], "grant");
    assert!(begin["idempotency_key"].as_str().is_some_and(|key| !key.is_empty()));

    // This is the actual file after the finishing worker has stopped. Reload
    // the whole state so no newer in-memory ledger accompanies an older row.
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let durable = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    let evaluate = durable.queued_prompts[0].engram_evaluate.as_ref().unwrap();
    let prepared = evaluate.prepared_begin.as_ref().expect("the durable Begin identity");
    let evaluate_request = serde_json::to_value(&evaluate.request).unwrap();
    assert_eq!(begin, json!({
        "operation": "turn_begin",
        "routing_token": evaluate_request["routing_token"],
        "grant_id": prepared.grant_id,
        "delivery_tokens": prepared.delivery_tokens,
        "idempotency_key": prepared.idempotency_key,
    }), "the loaded queued Begin must be the exact request sent before handoff");
    assert_eq!(
        durable.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .unwrap()
            .begun_grant_id
            .as_deref(),
        Some("grant")
    );
    // Load retains the durable Begin as recovered admission. Reconciliation
    // marks the queued interruption only after the producer status is read.
    assert!(durable.engram.recovered_admission);
    assert!(!durable.queued_prompts[0].engram_interrupted);
    assert!(receiver.try_recv().is_err(), "the original provider prompt was consumed exactly once");
    // Only after the untouched crash cut is proved and the old worker joined
    // do we retire its unwritten requests and install the loaded state.
    assert!(persist_rx.try_recv().is_err());
    drop(persist_rx);
    assert_eq!(fs::read(state.persistence_path.as_path()).unwrap(), before,
        "disposing the old receiver cannot trigger a fallback or late write");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let clock = inner.engram_budget_clock.clone();
        *inner = loaded;
        inner.engram_budget_clock = clock;
    }
    // Boot restores process-private wiring after loading the complete state.
    state.install_control_test_transport(transport.clone());
    let error = state
        .resume_session_queue(&session)
        .err()
        .expect("known interrupted delivery is an explicit hold");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        receiver.try_recv().is_err(),
        "never deliver the same prompt twice"
    );
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "session_status"
        ]
    );
    let durable: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(durable.queued_prompts[0].engram_interrupted);
}

#[test]
fn writer_backed_handoff_checkpoint_settles_only_after_its_durable_cuts() {
    let ((state, session, receiver, transport), persist_rx) = writer_backed_checkpoint_fixture([
        bind_reply("token"), grant_reply("grant"), begin_reply("grant"), checkpoint_reply("grant"),
    ]);
    let cuts = drive_writer_backed_checkpoint_finish(&state, &session, &persist_rx, &transport, true);
    assert!(matches!(cuts.as_slice(), [
        (EngramExistingCheckpointProgress::Retained, 0),
        (EngramExistingCheckpointProgress::PossiblySent, 0),
        (EngramExistingCheckpointProgress::Accepted { .. }, 1),
    ]), "send waits for both intent cuts and settlement waits for its receipt: {cuts:?}");
    assert_eq!(operations(&transport),
        ["session_bind", "turn_evaluate", "turn_begin", "turn_checkpoint"]);
    let request = transport.requests().into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint").unwrap().request;
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    let operation = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.protection.local_session_id == session).unwrap();
    operation.validate().unwrap();
    assert_eq!(serde_json::to_value(&operation.original.request).unwrap(), request);
    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_eq!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert_eq!(operation.protection.authority_confirmed,
        operation.original.settings.authority_store_key.is_some(),
        "routing provenance is independent of the accepted checkpoint settlement");
    assert!(loaded.engram_confirmed_containment_ids.contains(&operation.original.id));
    assert!(!engram_obligation_requires_exclusion(&loaded, &operation.protection));
    assert!(receiver.try_recv().is_err(), "no second provider prompt follows checkpoint settlement");
    drop(persist_rx);
}

#[test]
fn writer_backed_provider_handoff_stamps_and_persists_queue_removal_without_later_activity() {
    let (state, session, receiver, _) = root_fixture([
        bind_reply("token"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    let (persist_tx, persist_rx) = mpsc::channel();
    let state = AppState {
        persist_tx,
        ..state
    };
    let mut cache = SqlitePersistConnectionCache::new();
    let admission_watermark = std::thread::scope(|scope| {
        let worker = scope.spawn(|| deliver_turn_dispatch(&state, dispatch));
        commit_prepared_begin_fence(&state, &persist_rx, &mut cache);
        let mut batch = PersistFenceBatch::default();
        loop {
            let request = persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            let is_fence = matches!(&request, PersistRequest::Fence(_));
            batch.accept(request);
            if is_fence {
                break;
            }
        }
        let admission_delta = collect_persist_delta_from_shared_state(&state.inner, 0);
        assert_eq!(admission_delta.changed_sessions.len(), 1);
        assert_eq!(admission_delta.changed_sessions[0].queued_prompts.len(), 1);
        let watermark = admission_delta.watermark;
        persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &admission_delta,
            &mut batch,
        )
        .unwrap();
        worker.join().unwrap().unwrap();
        watermark
    });
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));

    let handoff_stamp = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(record.queued_prompts.is_empty());
        assert!(record.mutation_stamp > admission_watermark);
        record.mutation_stamp
    };
    let mut batch = PersistFenceBatch::default();
    batch.accept(persist_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap());
    while let Ok(request) = persist_rx.try_recv() {
        batch.accept(request);
    }
    let removal_delta = collect_persist_delta_from_shared_state(&state.inner, admission_watermark);
    assert_eq!(removal_delta.changed_sessions.len(), 1);
    assert!(removal_delta.changed_sessions[0].queued_prompts.is_empty());
    assert!(removal_delta.watermark >= handoff_stamp);
    persist_delta_with_fences(
        &mut cache,
        state.persistence_path.as_path(),
        &removal_delta,
        &mut batch,
    )
    .unwrap();

    let durable: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&state, &session)).unwrap();
    assert!(durable.queued_prompts.is_empty());
    let hydrated = state.get_session(&session).unwrap().session;
    assert!(hydrated.pending_prompts.is_empty());
    assert_eq!(hydrated.session_mutation_stamp, Some(handoff_stamp));
}

#[test]
fn retained_rebind_replays_with_an_old_token_and_resolves_bind_only_recovery() {
    for cold in [false, true] {
        let (state, session, receiver, transport) = root_fixture([
            status_reply("ready"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "lost rebind",
            ))),
            rebind_reply("new-token"),
            grant_reply("grant"),
            begin_reply("grant"),
        ]);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.engram.routing_token = Some("old-token".into());
            record.engram.rebind_required = true;
        }
        let dispatch = root_dispatch(&state, &session, false);
        deliver_turn_dispatch(&state, dispatch).unwrap();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            assert!(record.queued_prompts[0].engram_bind.is_some());
            assert!(record.queued_prompts[0].engram_evaluate.is_none());
            assert_eq!(record.engram.routing_token.as_deref(), Some("old-token"));
            if cold {
                let persisted = PersistedSessionRecord::from_record(record);
                let restored = persisted.into_record().unwrap();
                assert!(restored.engram.recovered_admission);
                *record = restored;
            }
        }
        state.resume_session_queue(&session).unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            CodexRuntimeCommand::Prompt { .. }
        ));
        assert!(receiver.try_recv().is_err());
        let requests = transport.requests();
        assert_eq!(
            operations(&transport),
            [
                "session_status",
                "session_bind",
                "session_bind",
                "turn_evaluate",
                "turn_begin"
            ]
        );
        assert_eq!(
            requests[1].request, requests[2].request,
            "exact saved rebind and key"
        );
        assert_eq!(requests[3].request["routing_token"], "new-token");
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram.recovered_admission);
        assert!(record.queued_prompts.is_empty());
    }
}

#[test]
fn late_cold_status_cannot_interrupt_or_clear_recovery_on_a_replacement_head() {
    for begun in [false, true] {
        let (state, session, receiver, _) = root_fixture([
            bind_reply("token"),
            ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
                "lost evaluation",
            ))),
        ]);
        let dispatch = root_dispatch(&state, &session, false);
        deliver_turn_dispatch(&state, dispatch).unwrap();
        let (mut target, original_id, owner) = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&session).unwrap();
            inner
                .session_mut_by_index(index)
                .unwrap()
                .engram
                .recovered_admission = true;
            (
                AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                    .unwrap()
                    .unwrap(),
                inner.sessions[index].queued_prompts[0]
                    .pending_prompt
                    .id
                    .clone(),
                EngramQueuedAdmissionOwner::capture(&inner.sessions[index]).unwrap(),
            )
        };
        target.admission_started_at = Some(std::time::Instant::now());
        let reply = if begun {
            ScriptedEngramControlResponse::Reply(Ok(
                json!({"phase":"turn_open", "open_grant_id":"old-grant", "open_grant_state":"begun"}),
            ))
        } else {
            status_reply("sync_required")
        };
        let (step, gate) = gated_engram_step("session_status", reply);
        let transport = GatedEngramControlTransport::new([step]);
        state.install_control_test_transport(transport.clone());
        // The target carries the transport snapshot, so refresh it after install.
        target.adapter = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .unwrap()
                .adapter
        };
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| state.restore_queued_engram_target(&mut target, &owner));
            gate.wait();
            state.cancel_queued_prompt(&session, &original_id).unwrap();
            queue_test_engram_prompt(
                &state,
                &session,
                "replacement",
                QueuedPromptSource::User,
                None,
            );
            {
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                let index = inner.find_session_index(&session).unwrap();
                inner
                    .session_mut_by_index(index)
                    .unwrap()
                    .engram
                    .recovered_admission = true;
            }
            gate.release();
            assert!(
                worker
                    .join()
                    .unwrap()
                    .unwrap_err()
                    .message
                    .contains("no longer owns")
            );
        });
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.queued_prompts[0].pending_prompt.text, "replacement");
        assert!(!record.queued_prompts[0].engram_interrupted);
        assert!(
            record.engram.recovered_admission,
            "old callback cannot clear replacement state"
        );
        assert!(!record.session.preview.contains("interrupted/unknown"));
        assert_eq!(
            transport.requests().len(),
            1,
            "no checkpoint by obsolete owner"
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn canceled_queue_head_cannot_prepare_an_unrecorded_evaluation() {
    let (state, session, receiver, transport) = root_fixture([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "cancel me",
        QueuedPromptSource::User,
        None,
    );
    let (target, intent, prompt_id, owner) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let queued = &record.queued_prompts[0];
        (
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .unwrap(),
            EngramTurnIntentSnapshot {
                session_id: session.clone(),
                dispatch_generation: 1,
                intent_fingerprint: engram_turn_intent_fingerprint(
                    &queued.pending_prompt.text,
                    None,
                    &[],
                    None,
                    queued.source,
                ),
            },
            queued.pending_prompt.id.clone(),
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
        )
    };
    state.cancel_queued_prompt(&session, &prompt_id).unwrap();
    let request = EngramControlRequest::TurnEvaluate {
        routing_token: "token".into(),
        intent_fingerprint: intent.intent_fingerprint.clone(),
        requested_effects: target.effects.clone(),
        resource_intents: Vec::new(),
        purpose: "ordinary".into(),
        idempotency_key: "cancelled-evaluation".into(),
    };
    assert!(
        state
            .queued_engram_evaluate_request(&target, &intent, &owner, request)
            .err()
            .expect("canceled evaluation must be rejected")
            .message
            .contains("no longer owns the queued prompt")
    );
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn legacy_operation_generation_parses_only_anchored_host_keys() {
    let session = "session-legacy";
    for key in [
        "termal-evaluate:session-legacy:41:fingerprint:with:colons",
        "termal-stale-reevaluate:session-legacy:42:fingerprint:7:with:colons",
        "termal-reevaluate:session-legacy:43:grant:99:with:colons",
    ] {
        let expected = if key.contains(":41:") {
            41
        } else if key.contains(":42:") {
            42
        } else {
            43
        };
        assert_eq!(
            legacy_queued_engram_operation_generation(session, key),
            Some(expected)
        );
    }
    for key in [
        "termal-evaluate:other-session:41:fingerprint",
        "termal-unknown:session-legacy:41:fingerprint",
        "termal-evaluate:session-legacy:not-a-number:fingerprint",
        "termal-evaluate:session-legacy:41",
        "termal-reevaluate:session-legacy:41:",
    ] {
        assert_eq!(
            legacy_queued_engram_operation_generation(session, key),
            None
        );
    }
}

#[test]
fn waiting_stop_shortcut_is_one_shot_and_never_handles_internal_stop_options() {
    let (state, session, receiver, transport) =
        root_fixture([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("lost bind"),
        ))]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    // Internal stop follows ordinary idle-session semantics, not a false success
    // that claims to have honored cleanup options.
    assert_eq!(
        state
            .stop_session_with_options(&session, StopSessionOptions::default())
            .err()
            .expect("idle internal stop conflicts")
            .status,
        StatusCode::CONFLICT
    );
    state.request_stop_session(&session).unwrap();
    let generation = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .dispatch_generation
    };
    assert_eq!(
        state
            .request_stop_session(&session)
            .err()
            .expect("second stop uses normal idle semantics")
            .status,
        StatusCode::CONFLICT
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.dispatch_generation, generation);
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind"]);
}

#[test]
fn mailbox_resume_replays_frozen_sequence_and_keeps_newer_wake_separate() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "lost evaluation",
        ))),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    let coordination_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&coordination_path).expect("test mailbox store opens"),
        ),
        ..state
    };
    let sender = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    }
    let message = |key: &str| SendMailboxMessageRequest {
        target_session_id: session.clone(),
        message: key.to_owned(),
        idempotency_key: key.to_owned(),
        topic: Some(key.to_owned()),
        state_stamp: None,
        class: Some("routine".into()),
    };
    state
        .append_mailbox_message_and_notify(&sender, message("first"))
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Idle;
    }
    let started = state
        .start_next_queued_turn_off_lock(&session, false, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&state, started.dispatch).unwrap();
    let original = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions[inner.find_session_index(&session).unwrap()].queued_prompts[0]
            .pending_prompt
            .clone()
    };
    state
        .append_mailbox_message_and_notify(&sender, message("second"))
        .unwrap();
    state.resume_session_queue(&session).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    assert!(receiver.try_recv().is_err());
    let requests = transport.requests();
    assert_eq!(
        requests[1].request, requests[2].request,
        "Resume must replay the first sequence exactly"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_ne!(record.queued_prompts[0].pending_prompt.id, original.id);
    assert!(
        record.queued_prompts[0]
            .pending_prompt
            .text
            .contains("second")
    );
}

#[test]
fn delegation_cancel_cleans_up_an_idle_child_with_retained_authorization() {
    let (state, root, receiver, _) = root_fixture([
        bind_reply("parent"),
        bind_reply("child"),
        grant_reply("setup"),
        begin_reply("setup"),
    ]);
    let created = state
        .create_read_only_delegation(
            &root,
            CreateDelegationRequest {
                prompt: "setup".into(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    receive_synchronous_engram_prompt(&state, &receiver, "setup prompt").unwrap();
    let child = &created.delegation.child_session_id;
    queue_test_engram_prompt(&state, child, "retained", QueuedPromptSource::User, None);
    let target = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        AppState::engram_binding_target_for_session_shape_locked(&inner, child, true)
            .unwrap()
            .unwrap()
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(child).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.engram.active_grant_id = None;
        record.engram.pending_dispatch = None;
        record.queued_prompts[0].engram_evaluate = Some(EngramQueuedEvaluate {
            begun_grant_id: None,
            prepared_begin: None,
            connection: target.connection,
            settings: target.settings,
            operation_generation: None,
            request: EngramControlRequest::TurnEvaluate {
                routing_token: "child".into(),
                intent_fingerprint: "retained".into(),
                purpose: "ordinary".into(),
                requested_effects: target.effects,
                resource_intents: Vec::new(),
                idempotency_key: "retained".into(),
            },
        });
    }
    state
        .cancel_delegation(&root, &created.delegation.id)
        .unwrap();
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(child).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.session.preview.contains("Prompt retained"));
}
