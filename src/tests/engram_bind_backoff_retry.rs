//! Ordinary connection fences, mailbox content and persistence cuts.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

#[path = "engram_bind_retry_due_control.rs"]
mod due_control;

#[test]
fn ordinary_naming_prepared_ack_obeys_its_shared_clock_without_gating_delivery() {
    // This is the retained naming owner's exact-image boundary. It does not
    // recreate the removed admission, Bind retry or automatic retry tick.
    for expiry in ["none", "before_ack", "after_ack"] {
        let mut turn = OrdinaryRoot::new("ordinary-naming-prepared-clock");
        let binding = test_control_work_binding("ordinary-naming-prepared-clock", 1);
        let store = EngramAuthorityStoreKey {
            database_path: turn.root.join("engram.db"),
            project_id: turn.project.clone(),
        };
        turn.edit(|record| record.engram.work_binding = Some(binding.clone()));
        turn.state.shutdown_persist_blocking();
        let clock = turn.state.engram_budget_clock();
        let started = clock.now();
        let deadline = started + Duration::from_secs(2);
        if expiry == "after_ack" {
            let expiry_clock = clock.clone();
            turn.state
                .inner
                .lock()
                .unwrap()
                .test_engram_authority_ack_boundary = Some(Arc::new(move |boundary| {
                if boundary == "before_owner_check" {
                    expiry_clock.advance(deadline.saturating_duration_since(expiry_clock.now()));
                }
            }));
        }
        let (tx, rx) = mpsc::channel();
        turn.state.persist_tx = tx;
        let worker = turn.state.clone();
        let worker_store = store.clone();
        let worker_binding = binding.clone();
        let task = std::thread::spawn(move || {
            worker.prepare_engram_authority_until(&worker_store, &worker_binding, deadline)
        });
        let fence = loop {
            if let PersistRequest::Fence(fence) = crate::tests::phase_sync::receive(
                &rx,
                "the retained naming owner's real Prepared fence",
            ) {
                break fence;
            }
        };
        let image = match &fence.target {
            PersistFenceTarget::EngramWorkAuthority(image) => image.clone(),
            _ => panic!("naming must request its own exact authority image"),
        };
        let completion = fence.completion.clone();
        let before_ack = clock.now();
        if expiry == "before_ack" {
            clock.advance(deadline.saturating_duration_since(clock.now()));
        }
        let mut batch = PersistFenceBatch::default();
        batch.accept(PersistRequest::Fence(fence));
        let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_with_fences(
            &mut cache,
            turn.state.persistence_path.as_path(),
            &delta,
            &mut batch,
        )
        .unwrap();
        let result = task
            .join()
            .expect("the naming owner finishes through the real writer");
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_engram_authority_ack_boundary = None;

        assert_eq!(
            before_ack, started,
            "no independent clock advances this operation"
        );
        assert_eq!(completion.deadline, deadline);
        let prepared = image.history.transition.as_ref().unwrap();
        assert_eq!(prepared.phase, EngramAuthorityPhase::Prepared);
        assert_eq!(prepared.binding, binding);
        let durable = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let saved = durable
            .engram_work_naming_history
            .iter()
            .find(|history| history.store == store && history.work_id == binding.work_id)
            .unwrap();
        assert_eq!(
            saved, &image.history,
            "even a late write retains the exact unresolved image"
        );
        assert!(engram_authority_work_unresolved(
            &durable,
            &store,
            &binding.work_id
        ));
        let ack = completion.poll_at(clock.now()).unwrap();
        match expiry {
            "none" => {
                assert!(ack.is_ok(), "the non-expired twin receives a real ACK");
                let owner = result.expect("a timely Prepared image may be used by naming");
                assert_eq!(owner.id, prepared.id);
                assert_eq!(owner.version, prepared.version);
                assert_eq!(owner.phase, EngramAuthorityPhase::Prepared);
                assert_eq!(clock.now(), started);
            }
            "before_ack" => {
                assert!(matches!(ack, Err(PersistFenceError::Deadline)));
                let error = result.expect_err("a late commit cannot authorize naming");
                assert!(error.message.contains("Deadline"), "{}", error.message);
            }
            _ => {
                assert!(ack.is_ok(), "the fence ACK precedes the owner-check expiry");
                let error = result.expect_err("a timely ACK grants no fresh owner-check budget");
                assert!(
                    error.message.contains("acknowledgement exceeded"),
                    "{}",
                    error.message
                );
            }
        }
        drop(cache);
        drop(rx);
        turn.assert_no_control();
        turn.assert_no_prompt();
        assert!(matches!(
            deliver_turn_dispatch(
                &turn.state,
                turn.prepare("ordinary delivery remains available")
            ),
            TurnDispatchDeliveryOutcome::Delivered
        ));
        assert!(
            turn.prompt()
                .prompt
                .contains("ordinary delivery remains available")
        );
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}

/// Stops only ordinary connection teardown at its existing transport boundary.
/// It supplies no authorization response or retained dispatch implementation.
struct ConnectionShutdownBoundary {
    recorder: Arc<ScriptedEngramControlTransport>,
    entered: mpsc::Sender<String>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl EngramControlTransport for ConnectionShutdownBoundary {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.recorder.request(connection, request, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.recorder.shutdown_session(session);
    }

    fn shutdown_session_until(
        &self,
        session: &str,
        _clock: &EngramBudgetClock,
        _deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.entered.send(session.into()).unwrap();
        crate::tests::phase_sync::receive(
            &self.release.lock().unwrap(),
            "release ordinary connection teardown",
        );
        Ok(())
    }
}

#[test]
fn ordinary_settings_reset_fences_the_real_queue_and_preserves_its_current_content() {
    for change in ["ordinary", "cancelled", "coalesced"] {
        let mut turn = OrdinaryRoot::new("ordinary-settings-reset");
        turn.state.mailbox_store = Arc::new(
            MailboxStore::open(&resolve_coordination_persistence_path(
                turn.state.persistence_path.as_ref(),
            ))
            .unwrap(),
        );
        let sender = test_session_id(&turn.state, Agent::Claude);
        turn.edit(|record| record.set_auto_dispatch_blocked(true));
        let mut sequence = None;
        let id = if change == "coalesced" {
            let send = |key: &str| {
                turn.state
                    .append_mailbox_message_and_notify(
                        &sender,
                        SendMailboxMessageRequest {
                            target_session_id: turn.session.clone(),
                            message: "body remains in mailbox".into(),
                            idempotency_key: key.into(),
                            topic: Some(key.into()),
                            state_stamp: None,
                            class: Some("routine".into()),
                        },
                    )
                    .unwrap()
            };
            let first = send("obsolete-reset-topic");
            let id = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
            let next = send("current-reset-topic");
            assert_eq!(first.mailbox_id, next.mailbox_id);
            sequence = Some(next.sequence);
            id
        } else {
            turn.enqueue("ordinary queued content", QueuedPromptSource::User)
        };
        if change == "cancelled" {
            turn.state.cancel_queued_prompt(&turn.session, &id).unwrap();
        }
        let queue = turn.record(|record| serde_json::to_value(&record.queued_prompts).unwrap());
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        turn.state
            .install_control_test_transport(Arc::new(ConnectionShutdownBoundary {
                recorder: turn.transport.clone(),
                entered: entered_tx,
                release: Mutex::new(released),
            }));
        let mut settings = turn
            .state
            .inner
            .lock()
            .unwrap()
            .find_project(&turn.project)
            .unwrap()
            .engram
            .clone()
            .unwrap();
        settings.enabled = false;
        let state = turn.state.clone();
        let project = turn.project.clone();
        let reset =
            std::thread::spawn(move || state.update_project_engram_settings(&project, settings));
        let fenced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(
                crate::tests::phase_sync::receive(&entered, "real settings transition teardown"),
                turn.session
            );
            {
                let inner = turn.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&turn.session).unwrap()];
                assert!(session_resource_reset_in_progress(&inner, record));
                assert!(inner.engram_project_resets.contains(&turn.project));
                assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), queue);
            }
            assert!(
                turn.state
                    .dispatch_next_queued_turn_with_prompt(&turn.session, false)
                    .unwrap()
                    .is_none()
            );
            turn.assert_no_prompt();
        }));
        let _ = release.send(());
        let reset_result = reset.join().expect("settings worker must not panic");
        if let Err(panic) = fenced {
            std::panic::resume_unwind(panic);
        }
        reset_result.unwrap();
        turn.record(|record| {
            assert_eq!(serde_json::to_value(&record.queued_prompts).unwrap(), queue);
            assert_ne!(
                record.session.status,
                SessionStatus::Active,
                "an undelivered queue must not be stranded Active"
            );
        });
        let public = turn.state.get_session(&turn.session).unwrap();
        let saved: PersistedSessionRecord =
            serde_json::from_str(&persisted_session_json(&turn.state, &turn.session)).unwrap();
        assert_eq!(public.session.status, saved.session.status);
        assert!(public.session.live_activity.is_none() && saved.session.live_activity.is_none());
        assert!(
            !turn
                .state
                .inner
                .lock()
                .unwrap()
                .engram_project_resets
                .contains(&turn.project)
        );
        if change == "cancelled" {
            turn.record(|record| assert!(record.queued_prompts.is_empty()));
            turn.assert_no_prompt();
        } else {
            turn.state.resume_session_queue(&turn.session).unwrap();
            let prompt = turn.prompt().prompt;
            if let Some(sequence) = sequence {
                assert!(
                    prompt.contains("current-reset-topic")
                        && !prompt.contains("obsolete-reset-topic")
                );
                assert!(prompt.contains(&sequence.to_string()));
            } else {
                assert!(prompt.contains("ordinary queued content"));
            }
            turn.assert_no_prompt();
        }
        turn.assert_no_control();
    }
}

#[test]
fn ordinary_settings_write_failure_rolls_back_metadata_and_keeps_the_queue_and_public_state() {
    let turn = OrdinaryRoot::new("ordinary-reset-write-failure");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    turn.enqueue("survives failed settings write", QueuedPromptSource::User);
    turn.state.shutdown_persist_blocking();
    let before = turn.saved();
    let settings = turn
        .state
        .inner
        .lock()
        .unwrap()
        .find_project(&turn.project)
        .unwrap()
        .engram
        .clone()
        .unwrap();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_ordinary_settings BEFORE UPDATE ON state_metadata_units
        WHEN NEW.field_name='projects' BEGIN SELECT RAISE(ABORT,'ordinary settings fault'); END;",
        )
        .unwrap();
    drop(connection);
    let mut changed = settings.clone();
    changed.enabled = false;
    let error = match turn
        .state
        .update_project_engram_settings(&turn.project, changed)
    {
        Err(error) => error,
        Ok(_) => panic!("the real metadata write must fail"),
    };
    assert!(
        error.message.contains("ordinary settings fault"),
        "{}",
        error.message
    );
    assert_eq!(
        turn.state
            .inner
            .lock()
            .unwrap()
            .find_project(&turn.project)
            .unwrap()
            .engram
            .as_ref(),
        Some(&settings)
    );
    assert_eq!(turn.saved(), before);
    assert!(
        !turn
            .state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .contains(&turn.project)
    );
    let public = turn.state.get_session(&turn.session).unwrap();
    let stored: PersistedSessionRecord =
        serde_json::from_str(&persisted_session_json(&turn.state, &turn.session)).unwrap();
    assert_eq!(public.session.status, stored.session.status);
    assert!(public.session.live_activity.is_none() && stored.session.live_activity.is_none());
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection
        .execute_batch("DROP TRIGGER fail_ordinary_settings")
        .unwrap();
    drop(connection);
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_connection_fence_blocks_a_prepared_handoff_without_touching_a_newer_owner() {
    let turn = OrdinaryRoot::new("ordinary-connection-handoff");
    let dispatch = turn.prepare("prepared before connection change");
    let generation = {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.claim_engram_project_reset(&turn.project).unwrap()
    };
    let before = turn.saved();
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Held { .. }
    ));
    assert_eq!(
        turn.saved(),
        before,
        "resource exclusion must not rewrite the active owner's data"
    );
    turn.assert_no_prompt();
    assert!(
        turn.state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .release(&turn.project, generation)
    );
    turn.assert_no_control();
}

#[test]
fn ordinary_mailbox_coalescing_publishes_the_current_payload_with_one_identity_and_handoff() {
    let mut turn = OrdinaryRoot::new("ordinary-coalesced-body");
    turn.state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            turn.state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&turn.state, Agent::Claude);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let send = |topic: &str| {
        turn.state
            .append_mailbox_message_and_notify(
                &sender,
                SendMailboxMessageRequest {
                    target_session_id: turn.session.clone(),
                    message: "durable body only".into(),
                    idempotency_key: topic.into(),
                    topic: Some(topic.into()),
                    state_stamp: None,
                    class: Some("routine".into()),
                },
            )
            .unwrap()
    };
    let first = send("ordinary-obsolete-topic");
    let id = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let second = send("ordinary-current-topic");
    assert_eq!(first.mailbox_id, second.mailbox_id);
    let expected = turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        let prompt = &record.queued_prompts[0].pending_prompt;
        assert_eq!(prompt.id, id);
        assert_eq!(
            prompt
                .source
                .as_ref()
                .unwrap()
                .mailbox
                .as_ref()
                .unwrap()
                .sequence,
            second.sequence
        );
        prompt.clone()
    });
    let mut events = turn.state.subscribe_delta_events();
    turn.state.resume_session_queue(&turn.session).unwrap();
    let runtime_prompt = turn.prompt().prompt;
    assert!(
        runtime_prompt.contains("ordinary-current-topic")
            && !runtime_prompt.contains("ordinary-obsolete-topic")
    );
    let index = turn.record(|record| {
        let rows = record
            .session
            .messages
            .iter()
            .filter(|message| message.id() == id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        let Message::Text {
            text,
            expanded_text,
            attachments,
            source,
            ..
        } = rows[0]
        else {
            panic!("ordinary transcript prompt is text")
        };
        assert_eq!(text, &expected.text);
        assert_eq!(expanded_text, &expected.expanded_text);
        assert_eq!(
            serde_json::to_value(attachments).unwrap(),
            serde_json::to_value(&expected.attachments).unwrap()
        );
        assert_eq!(source, &expected.source);
        global_message_index(record, cached_message_index_on_record(record, &id).unwrap())
    });
    let mut published = false;
    while let Ok(payload) = events.try_recv() {
        if let DeltaEvent::MessageCreated {
            message_id,
            message_index,
            message,
            session_seq,
            body_seq_epoch,
            ..
        } = serde_json::from_str(&payload).unwrap()
        {
            if message_id == id {
                assert_eq!(message_index, index);
                assert!(session_seq.is_some());
                assert_eq!(
                    body_seq_epoch.as_deref(),
                    Some(turn.state.server_instance_id.as_str())
                );
                let Message::Text { text, .. } = message else {
                    panic!("published prompt is text")
                };
                assert_eq!(text, expected.text);
                published = true;
            }
        }
    }
    assert!(
        published,
        "ordinary promotion publishes the exact coalesced body"
    );
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_queue_mutation_is_selected_after_a_consumed_persist_watermark_and_written_to_sqlite() {
    let mut turn = OrdinaryRoot::new("ordinary-queue-watermark");
    turn.state.shutdown_persist_blocking();
    let (tx, _rx) = mpsc::channel();
    turn.state.persist_tx = tx;
    let mut cache = SqlitePersistConnectionCache::new();
    let mut batch = PersistFenceBatch::default();
    let first = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(
        &mut cache,
        turn.state.persistence_path.as_path(),
        &first,
        &mut batch,
    )
    .unwrap();
    let id = turn.enqueue(
        "ordinary queue after saved watermark",
        QueuedPromptSource::User,
    );
    turn.edit(|record| {
        record.engram.historical_control.insert(
            "engramBindRetry".into(),
            json!({"originalKey":"historical-key","attempts":9,"phase":"old"}),
        );
    });
    let expected = turn.saved();
    let next = collect_persist_delta_from_shared_state(&turn.state.inner, first.watermark);
    assert!(next.watermark > first.watermark);
    let row = next
        .changed_sessions
        .iter()
        .find(|saved| saved.session.id == turn.session)
        .expect("the exact newer row is selected");
    assert_eq!(serde_json::to_value(row).unwrap(), expected);
    persist_delta_with_fences(
        &mut cache,
        turn.state.persistence_path.as_path(),
        &next,
        &mut batch,
    )
    .unwrap();
    drop(cache);
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session).unwrap()];
    assert_eq!(record.queued_prompts[0].pending_prompt.id, id);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "ordinary queue after saved watermark"
    );
    assert_eq!(
        record.engram.historical_control["engramBindRetry"],
        expected["engramBindRetry"]
    );
    assert!(record.provider_capacity_retry.is_none());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_rejected_delegation_followup_is_failed_in_public_stored_and_parent_views() {
    let turn = OrdinaryRoot::new("ordinary-followup-rejection");
    let created = turn
        .state
        .create_read_only_delegation(
            &turn.session,
            CreateDelegationRequest {
                prompt: "initial ordinary child".into(),
                title: Some("ordinary followup".into()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Explorer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let id = created.delegation.id;
    let child = created.delegation.child_session_id;
    let mut initial = 0;
    while let Ok(command) = turn.receiver.try_recv() {
        if let CodexRuntimeCommand::Prompt { session_id, .. } = command {
            assert_eq!(session_id, child);
            initial += 1;
        }
    }
    assert_eq!(initial, 1);
    crate::tests::delegation_support::finish_delegation_child_with_assistant_text(
        &turn.state,
        &child,
        "## Result\nStatus: completed\n\nSummary:\nordinary child completed",
    );
    turn.state
        .refresh_delegation_for_child_session(&child)
        .unwrap();
    assert_eq!(
        turn.state
            .get_delegation(&turn.session, &id)
            .unwrap()
            .delegation
            .status,
        DelegationStatus::Completed
    );
    let (runtime, closed_receiver, _process) =
        test_shared_codex_runtime("ordinary-closed-followup-runtime");
    drop(closed_receiver);
    install_single_wire_codex_fixture(&turn.state, Some(runtime));
    assert!(
        turn.state
            .followup_delegation(&turn.session, &id, "rejected ordinary followup".into())
            .is_err()
    );
    turn.state.sync_delegation_attempt_for_child_session(&child);
    let public = turn.state.get_delegation(&turn.session, &id).unwrap();
    assert_eq!(public.delegation.status, DelegationStatus::Failed);
    assert_eq!(
        public.delegation.result.unwrap().status,
        DelegationStatus::Failed
    );
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let json: String = connection
        .query_row(
            "SELECT value_json FROM delegations WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    let saved: DelegationRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(saved.status, DelegationStatus::Failed);
    assert_eq!(saved.result.unwrap().status, DelegationStatus::Failed);
    drop(connection);
    let parent = turn.state.get_session(&turn.session).unwrap();
    assert!(parent.session.messages.iter().any(|message| matches!(message,
        Message::ParallelAgents { agents, .. } if agents.iter().any(|agent|
            agent.id == id && agent.source == ParallelAgentSource::Delegation && agent.status == ParallelAgentStatus::Error))));
    turn.assert_no_control();
}
