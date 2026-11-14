//! Connection-fence release drains independent sessions even when cleanup or
//! another session's provider handoff fails. Real mailbox wakes own the queue.
use super::*;

fn enable_mailbox(fixture: &mut ConnectionFixture) -> String {
    let path = resolve_coordination_persistence_path(fixture.state.persistence_path.as_ref());
    fixture.state.mailbox_store = Arc::new(MailboxStore::open(&path).unwrap());
    test_session_id(&fixture.state, Agent::Claude)
}

fn queue_mailbox(fixture: &ConnectionFixture, sender: &str, session: &str) -> String {
    let receipt = fixture
        .state
        .append_mailbox_message_and_notify(
            sender,
            SendMailboxMessageRequest {
                target_session_id: session.to_owned(),
                message: "Work arrived during connection cleanup".to_owned(),
                idempotency_key: format!("post-fence-{session}"),
                topic: Some("post-fence delivery".to_owned()),
                state_stamp: None,
                class: Some("routine".to_owned()),
            },
        )
        .unwrap();
    assert!(!receipt.duplicate);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.engram_project_resets.contains(&fixture.project));
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(!record.session.queue_paused && !record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    let queued = &record.queued_prompts[0];
    assert_eq!(queued.source, QueuedPromptSource::Mailbox);
    let mailbox = queued
        .pending_prompt
        .source
        .as_ref()
        .unwrap()
        .mailbox
        .as_ref()
        .unwrap();
    assert_eq!(mailbox.message_id, receipt.message_id);
    assert!(fixture.receiver.try_recv().is_err());
    queued.pending_prompt.text.clone()
}

fn assert_delivered(fixture: &ConnectionFixture, session: &str, text: &str) {
    // The settings call performs the synchronous handoff before returning.
    // No send, Resume, explicit drain or timing wait is allowed to repair it.
    match fixture
        .receiver
        .try_recv()
        .expect("fence release must deliver the queued wake")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, session);
            assert_eq!(command.prompt, text);
        }
        _ => panic!("expected one provider prompt"),
    }
    assert!(fixture.receiver.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&fixture.project));
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert_eq!(record.session.status, SessionStatus::Active);
}

#[test]
fn pending_revocation_does_not_strand_another_sessions_mailbox_wake() {
    let mut fixture = ConnectionFixture::new();
    let sender = enable_mailbox(&mut fixture);
    let stopped = create_test_project_session(
        &fixture.state,
        Agent::Claude,
        &fixture.project,
        &fixture.root,
    );
    let process_owner = crate::tests::phase_sync::ParkedProcess::spawn();
    let (input_tx, _input_rx) = mpsc::channel();
    let (token, generation) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner
            .engram_declared_project_ids
            .insert(fixture.project.clone());
        let descriptor = engram_mcp_runtime_config_for_session_locked(&inner, &stopped)
            .expect("enabled, declared project has an MCP descriptor")
            .installed;
        let record = inner.session_mut(&stopped).unwrap();
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "post-fence-stopped-runtime".to_owned(),
            input_tx,
            #[cfg(windows)]
            process_tree: None,
            process: process_owner.process.clone(),
        });
        record.engram_mcp_installed = Some(descriptor);
        record.session.status = SessionStatus::Active;
        let token = record.runtime.runtime_token().unwrap();
        let generation = record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token.clone());
        (token, generation)
    };
    let (entered, release) = fixture.transport.gate_cleanup();
    let (error, text) = std::thread::scope(|scope| {
        let settings = fixture.disabled();
        let state = &fixture.state;
        let project = &fixture.project;
        let change = scope.spawn(move || state.update_project_engram_settings(project, settings));
        let observation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            entered.recv_timeout(DEADLOCK_GUARD).unwrap();
            queue_mailbox(&fixture, &sender, &fixture.session)
        }));
        drop(release);
        let result = change.join().unwrap();
        let text = observation.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
        (result.err().expect("pending cleanup stays visible"), text)
    });
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("runtime cleanup is pending behind an existing Stop")
    );
    assert!(error.message.contains(&stopped));
    assert_delivered(&fixture, &fixture.session, &text);
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&stopped).unwrap()];
    assert!(record.runtime_stop_is_owned_by(RuntimeStopOwnerKind::UserStop, &token, generation));
    assert!(record.engram_mcp_revocation_pending);
}

#[test]
fn degraded_cleanup_still_delivers_the_mailbox_wake_before_returning_its_error() {
    let mut fixture = ConnectionFixture::new();
    let sender = enable_mailbox(&mut fixture);
    *fixture.transport.cleanup_failure.lock().unwrap() =
        Some("injected cleanup failure".to_owned());
    let (entered, release) = fixture.transport.gate_cleanup();
    let (error, text) = std::thread::scope(|scope| {
        let settings = fixture.disabled();
        let state = &fixture.state;
        let project = &fixture.project;
        let change = scope.spawn(move || state.update_project_engram_settings(project, settings));
        let observation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            entered.recv_timeout(DEADLOCK_GUARD).unwrap();
            queue_mailbox(&fixture, &sender, &fixture.session)
        }));
        drop(release);
        let result = change.join().unwrap();
        let text = observation.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
        (result.err().expect("degraded cleanup stays visible"), text)
    });
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("injected cleanup failure"));
    assert_delivered(&fixture, &fixture.session, &text);
}

#[test]
fn a_failed_session_drain_or_delivery_does_not_abandon_later_sessions() {
    for fail_delivery in [false, true] {
        let mut fixture = ConnectionFixture::new();
        let sender = enable_mailbox(&mut fixture);
        let later = create_test_project_session(
            &fixture.state,
            Agent::Codex,
            &fixture.project,
            &fixture.root,
        );
        let ordered = project_engram_session_ids_locked(
            &fixture.state.inner.lock().unwrap(),
            &fixture.project,
        );
        assert!(
            ordered
                .iter()
                .position(|id| id == &fixture.session)
                .unwrap()
                < ordered.iter().position(|id| id == &later).unwrap()
        );
        let (entered, release) = fixture.transport.gate_cleanup();
        let (error, text) = std::thread::scope(|scope| {
            let settings = fixture.disabled();
            let state = &fixture.state;
            let project = &fixture.project;
            let change =
                scope.spawn(move || state.update_project_engram_settings(project, settings));
            let observation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                entered.recv_timeout(DEADLOCK_GUARD).unwrap();
                queue_mailbox(&fixture, &sender, &fixture.session);
                let text = queue_mailbox(&fixture, &sender, &later);
                let mut inner = fixture.state.inner.lock().unwrap();
                let record = inner.session_mut(&fixture.session).unwrap();
                // Close just this runtime's writer to exercise a real rejected
                // handoff; the shared writer for the later session stays live.
                let (closed, receiver) = mpsc::channel();
                drop(receiver);
                let process = match &record.runtime {
                    SessionRuntime::Codex(runtime) => runtime.process.clone(),
                    _ => panic!("fixture retains its Codex runtime during cleanup"),
                };
                record.runtime = if fail_delivery {
                    SessionRuntime::Codex(CodexRuntimeHandle {
                        runtime_id: "post-fence-closed-writer".to_owned(),
                        input_tx: closed,
                        process,
                        shared_session: None,
                    })
                } else {
                    // A mismatched attached runtime fails during preparation,
                    // before a provider handoff is even constructed.
                    let (input_tx, _input_rx) = mpsc::channel();
                    SessionRuntime::Claude(ClaudeRuntimeHandle {
                        runtime_id: "post-fence-mismatched-runtime".to_owned(),
                        input_tx,
                        #[cfg(windows)]
                        process_tree: None,
                        process,
                    })
                };
                record.clear_runtime_reset();
                text
            }));
            drop(release);
            let result = change.join().unwrap();
            let text = observation.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
            (
                result.err().expect("one session's error stays visible"),
                text,
            )
        });
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
        let expected_error = if fail_delivery {
            "failed to queue prompt for Codex session"
        } else {
            "unexpected Claude runtime attached to Codex session"
        };
        assert!(error.message.contains(expected_error), "{}", error.message);
        assert_delivered(&fixture, &later, &text);
    }
}
