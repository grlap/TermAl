// Ordinary connection changes exclude provider dispatch only while their
// exact generation owns resource cleanup. Observation preferences never claim
// that fence. Queue pause, Stop and persistence ownership remain independent.
use super::*;
use crate::tests::delegation_support::test_app_state_with_delegation_codex_runtime;
use crate::tests::engram_host_adapter::real_engram_control_fixture_path;
use crate::tests::phase_sync::DEADLOCK_GUARD;

#[path = "engram_post_fence_drain.rs"]
mod post_fence_drain;

#[derive(Default)]
struct ConnectionCleanupTransport {
    requests: Mutex<Vec<Value>>,
    shutdowns: Mutex<Vec<String>>,
    cleanup_gate: Mutex<Option<(mpsc::SyncSender<String>, mpsc::Receiver<()>)>>,
    cleanup_failure: Mutex<Option<String>>,
}

impl ConnectionCleanupTransport {
    fn gate_cleanup(&self) -> (mpsc::Receiver<String>, mpsc::Sender<()>) {
        let (entered, arrival) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::channel();
        assert!(
            self.cleanup_gate
                .lock()
                .unwrap()
                .replace((entered, wait))
                .is_none()
        );
        (arrival, release)
    }
}

impl EngramControlTransport for ConnectionCleanupTransport {
    fn request(
        &self,
        _connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_value(request).unwrap());
        Err(EngramTransportError::protocol(
            "unexpected work-service request in connection cleanup test",
        ))
    }

    fn shutdown_session(&self, session: &str) {
        self.shutdowns.lock().unwrap().push(session.to_owned());
    }

    fn shutdown_session_until(
        &self,
        session: &str,
        _clock: &EngramBudgetClock,
        _deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.shutdown_session(session);
        let gate = self.cleanup_gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered
                .send(session.to_owned())
                .expect("cleanup observer stays alive");
            // Disconnection releases cleanup during panic unwinding, too.
            match release.recv_timeout(DEADLOCK_GUARD) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(EngramTransportError::deadline(
                        "test cleanup gate did not release",
                    ));
                }
            }
        }
        match self.cleanup_failure.lock().unwrap().as_ref() {
            Some(message) => Err(EngramTransportError::protocol(message.clone())),
            None => Ok(()),
        }
    }
}

struct ConnectionFixture {
    state: AppState,
    project: String,
    session: String,
    root: PathBuf,
    settings: EngramProjectSettings,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<ConnectionCleanupTransport>,
}

impl ConnectionFixture {
    fn new() -> Self {
        let (state, receiver) = test_app_state_with_delegation_codex_runtime("ordinary-connection");
        state.select_test_scripted_engram_budget_clock();
        let root = state
            .test_temp_root
            .as_ref()
            .unwrap()
            .path()
            .join("ordinary-connection-project");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
        let project = create_test_project(&state, &root, "Ordinary connection");
        let session = create_test_project_session(&state, Agent::Codex, &project, &root);
        let runtime = state.shared_codex_runtime.lock().unwrap().clone().unwrap();
        let settings = EngramProjectSettings {
            acceptance_evaluation: None,
            enabled: true,
            execution_observability: false,
            binary_path: Some(
                real_engram_control_fixture_path()
                    .to_string_lossy()
                    .into_owned(),
            ),
            home: Some(root.to_string_lossy().into_owned()),
            authority_store_key: None,
            deadline_ms: None,
        };
        {
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|item| item.id == project)
                .unwrap()
                .engram = Some(settings.clone());
            let record = inner.session_mut(&session).unwrap();
            record.engram.context_nudge_pending = false;
            record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: runtime.process.clone(),
                shared_session: None,
            });
            state.commit_locked(&mut inner).unwrap();
        }
        let transport = Arc::new(ConnectionCleanupTransport::default());
        state.install_control_test_transport(transport.clone());
        Self {
            state,
            project,
            session,
            root,
            settings,
            receiver,
            transport,
        }
    }

    fn send(&self, text: &str) -> DispatchTurnResult {
        self.state
            .dispatch_turn(
                &self.session,
                SendMessageRequest {
                    text: text.to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap()
    }

    fn disabled(&self) -> EngramProjectSettings {
        let mut settings = self.settings.clone();
        settings.enabled = false;
        settings
    }

    fn assert_prompt(&self, expected: &str) {
        let command = self.receiver.recv_timeout(DEADLOCK_GUARD).unwrap();
        match command {
            CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } => {
                assert_eq!(session_id, self.session);
                assert_eq!(command.prompt, expected);
            }
            _ => panic!("ordinary queue must send a provider prompt"),
        }
        assert!(
            self.receiver.try_recv().is_err(),
            "only one provider delivery"
        );
        assert!(self.transport.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn connection_claims_keep_one_clock_and_stale_release_cannot_clear_a_successor() {
    let clock = EngramBudgetClock::scripted();
    clock.pin_wall();
    let mut fences = EngramProjectResetFences::default();
    let first = fences.claim_with_clock("project", clock.clone()).unwrap();
    let claim = fences.claimed_at["project"].clone();
    assert_eq!(claim.generation, first);
    assert_eq!(claim.claimed_at, clock.wall_now());
    assert_eq!(
        claim.deadline.duration_since(clock.now()),
        Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS + 1_000)
    );
    assert!(fences.claim_with_clock("project", clock.clone()).is_none());
    clock.advance(Duration::from_secs(3));
    assert_eq!(claim.clock.now(), clock.now());
    assert!(fences.release("project", first));
    let second = fences.claim_with_clock("project", clock).unwrap();
    assert_ne!(second, first);
    assert!(!fences.release("project", first));
    assert!(fences.is_owned_by("project", second));
    assert_eq!(fences.claimed_at["project"].generation, second);
    assert!(fences.release("project", second));
    assert!(!fences.contains("project"));
    assert!(fences.claimed_at("project").is_none());
}

#[test]
fn a_connection_change_holds_direct_prompts_until_cleanup_finishes() {
    let fixture = ConnectionFixture::new();
    let (entered, release) = fixture.transport.gate_cleanup();
    std::thread::scope(|scope| {
        let state = &fixture.state;
        let project = &fixture.project;
        let settings = fixture.disabled();
        let change = scope.spawn(move || state.update_project_engram_settings(project, settings));
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(
                entered.recv_timeout(DEADLOCK_GUARD).unwrap(),
                fixture.session
            );
            assert!(
                fixture
                    .state
                    .inner
                    .lock()
                    .unwrap()
                    .engram_project_resets
                    .contains(&fixture.project)
            );
            assert!(matches!(
                fixture.send("after cleanup"),
                DispatchTurnResult::Queued
            ));
            assert!(fixture.receiver.try_recv().is_err());
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(
                record.queued_prompts[0].pending_prompt.text,
                "after cleanup"
            );
        }));
        drop(release);
        let result = change.join().unwrap();
        observed.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
        result.unwrap();
    });
    assert!(
        !fixture
            .state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .contains(&fixture.project)
    );
    fixture.assert_prompt("after cleanup");
    assert_eq!(
        *fixture.transport.shutdowns.lock().unwrap(),
        vec![fixture.session.clone()]
    );
}

#[test]
fn a_connection_change_holds_a_mailbox_wake_until_cleanup_finishes() {
    let mut fixture = ConnectionFixture::new();
    let mailbox_path =
        resolve_coordination_persistence_path(fixture.state.persistence_path.as_ref());
    fixture.state.mailbox_store = Arc::new(MailboxStore::open(&mailbox_path).unwrap());
    let sender = test_session_id(&fixture.state, Agent::Claude);
    let (entered, release) = fixture.transport.gate_cleanup();
    let wake_prompt = std::thread::scope(|scope| {
        let state = &fixture.state;
        let project = &fixture.project;
        let settings = fixture.disabled();
        let change = scope.spawn(move || state.update_project_engram_settings(project, settings));
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(
                entered.recv_timeout(DEADLOCK_GUARD).unwrap(),
                fixture.session
            );
            {
                let inner = fixture.state.inner.lock().unwrap();
                assert!(inner.engram_project_resets.contains(&fixture.project));
                let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Idle);
                assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
                assert!(record.queued_prompts.is_empty());
            }
            // Append and notify through the real mailbox owner, rather than
            // inserting a synthetic queue entry or dispatching a User prompt.
            let receipt = fixture
                .state
                .append_mailbox_message_and_notify(
                    &sender,
                    SendMailboxMessageRequest {
                        target_session_id: fixture.session.clone(),
                        message: "Read the durable mailbox after connection cleanup.".to_owned(),
                        idempotency_key: "connection-cleanup-mailbox-wake".to_owned(),
                        topic: Some("connection cleanup".to_owned()),
                        state_stamp: None,
                        class: Some("routine".to_owned()),
                    },
                )
                .unwrap();
            assert!(!receipt.duplicate);
            let expected = mailbox_notification_text(
                &receipt.mailbox_id,
                1,
                receipt.sequence,
                "Test",
                Some("connection cleanup"),
            );
            {
                let inner = fixture.state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Idle);
                assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
                assert_eq!(record.queued_prompts.len(), 1);
                let queued = &record.queued_prompts[0];
                assert_eq!(queued.source, QueuedPromptSource::Mailbox);
                assert_eq!(queued.pending_prompt.text, expected);
                let source = queued.pending_prompt.source.as_ref().unwrap();
                assert!(source.is_mailbox());
                let mailbox = source.mailbox.as_ref().unwrap();
                assert_eq!(mailbox.mailbox_id, receipt.mailbox_id);
                assert_eq!(mailbox.message_id, receipt.message_id);
                assert_eq!(mailbox.sequence, receipt.sequence);
            }
            assert!(
                fixture.receiver.try_recv().is_err(),
                "a held wake cannot reach the provider"
            );
            assert!(
                fixture
                    .state
                    .dispatch_next_queued_turn(&fixture.session, false)
                    .unwrap()
                    .is_none(),
                "an explicit drain cannot cross live connection cleanup"
            );
            assert!(fixture.receiver.try_recv().is_err());
            {
                let inner = fixture.state.inner.lock().unwrap();
                assert!(inner.engram_project_resets.contains(&fixture.project));
                let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Idle);
                assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
                assert_eq!(record.queued_prompts.len(), 1);
                assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::Mailbox);
                assert_eq!(record.queued_prompts[0].pending_prompt.text, expected);
            }
            expected
        }));
        // Release and join even when an observation fails, so cleanup never
        // waits on an assertion's unwinding thread.
        drop(release);
        let result = change.join().unwrap();
        let expected = observed.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
        result.unwrap();
        expected
    });
    fixture.assert_prompt(&wake_prompt);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(!inner.engram_project_resets.contains(&fixture.project));
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert!(
        record.queued_prompts.is_empty(),
        "cleanup drains the one accepted wake"
    );
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(!record.orchestrator_auto_dispatch_blocked && !record.session.queue_paused);
    assert_eq!(
        *fixture.transport.shutdowns.lock().unwrap(),
        vec![fixture.session.clone()]
    );
}

#[test]
fn connection_release_preserves_operator_pause_and_exact_runtime_stop_owner() {
    for paused in [true, false] {
        let fixture = ConnectionFixture::new();
        let (entered, release) = fixture.transport.gate_cleanup();
        let mut stop = None;
        std::thread::scope(|scope| {
            let state = &fixture.state;
            let project = &fixture.project;
            let settings = fixture.disabled();
            let change =
                scope.spawn(move || state.update_project_engram_settings(project, settings));
            let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                entered.recv_timeout(DEADLOCK_GUARD).unwrap();
                assert!(matches!(
                    fixture.send("held independently"),
                    DispatchTurnResult::Queued
                ));
                let mut inner = fixture.state.inner.lock().unwrap();
                let record = inner.session_mut(&fixture.session).unwrap();
                record.session.preview = "Existing owner preview".to_owned();
                if paused {
                    record.set_auto_dispatch_blocked(true);
                } else {
                    let token = record.runtime.runtime_token().unwrap();
                    let generation =
                        record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token.clone());
                    stop = Some((token, generation));
                }
                fixture.state.commit_locked(&mut inner).unwrap();
            }));
            drop(release);
            let result = change.join().unwrap();
            observed.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
            result.unwrap();
        });
        assert!(
            fixture.receiver.try_recv().is_err(),
            "cleanup release grants no force-resume"
        );
        assert!(
            fixture
                .state
                .dispatch_next_queued_turn(&fixture.session, false)
                .unwrap()
                .is_none()
        );
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            assert!(!inner.engram_project_resets.contains(&fixture.project));
            let record = inner.session_mut(&fixture.session).unwrap();
            assert_eq!(record.queued_prompts.len(), 1);
            assert_eq!(record.session.preview, "Existing owner preview");
            if paused {
                assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
                record.set_auto_dispatch_blocked(false);
            } else {
                let (token, generation) = stop.as_ref().unwrap();
                assert!(record.runtime_stop_is_owned_by(
                    RuntimeStopOwnerKind::UserStop,
                    token,
                    *generation
                ));
                record.clear_runtime_stop();
            }
            fixture.state.commit_locked(&mut inner).unwrap();
        }
        let dispatch = fixture
            .state
            .dispatch_next_queued_turn(&fixture.session, false)
            .unwrap()
            .unwrap();
        deliver_turn_dispatch(&fixture.state, dispatch).unwrap();
        fixture.assert_prompt("held independently");
    }
}

#[test]
fn late_connection_cleanup_cannot_release_the_next_generation_or_drain_its_queue() {
    let fixture = ConnectionFixture::new();
    let (entered, release) = fixture.transport.gate_cleanup();
    let mut successor = None;
    std::thread::scope(|scope| {
        let state = &fixture.state;
        let project = &fixture.project;
        let settings = fixture.disabled();
        let change = scope.spawn(move || state.update_project_engram_settings(project, settings));
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            entered.recv_timeout(DEADLOCK_GUARD).unwrap();
            assert!(matches!(
                fixture.send("successor owns this wait"),
                DispatchTurnResult::Queued
            ));
            let mut inner = fixture.state.inner.lock().unwrap();
            let original = inner.engram_project_resets.claimed_at[&fixture.project].generation;
            assert!(
                inner
                    .engram_project_resets
                    .release(&fixture.project, original)
            );
            let next = inner.claim_engram_project_reset(&fixture.project).unwrap();
            assert_ne!(next, original);
            assert!(
                inner
                    .engram_project_reset_claim(&fixture.project, original)
                    .is_none()
            );
            successor = Some(next);
        }));
        drop(release);
        let result = change.join().unwrap();
        observed.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
        let error = result.err().expect("lost ownership must be reported");
        assert!(
            error.message.contains("ownership changed during cleanup"),
            "{}",
            error.message
        );
    });
    assert!(fixture.receiver.try_recv().is_err());
    let mut inner = fixture.state.inner.lock().unwrap();
    let next = successor.unwrap();
    assert!(
        inner
            .engram_project_resets
            .is_owned_by(&fixture.project, next)
    );
    assert_eq!(
        inner.sessions[inner.find_session_index(&fixture.session).unwrap()]
            .queued_prompts
            .len(),
        1
    );
    assert!(inner.engram_project_resets.release(&fixture.project, next));
}

#[test]
fn observability_changes_do_not_claim_cleanup_or_change_queue_pause_and_stop() {
    let fixture = ConnectionFixture::new();
    let (token, generation) = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.session).unwrap();
        record.set_auto_dispatch_blocked(true);
        let token = record.runtime.runtime_token().unwrap();
        let generation = record.claim_runtime_stop(RuntimeStopOwnerKind::UserStop, token.clone());
        fixture.state.commit_locked(&mut inner).unwrap();
        (token, generation)
    };
    assert!(matches!(
        fixture.send("waiting for the operator"),
        DispatchTurnResult::Queued
    ));
    let before = {
        let inner = fixture.state.inner.lock().unwrap();
        serde_json::to_value(
            &inner.sessions[inner.find_session_index(&fixture.session).unwrap()].queued_prompts,
        )
        .unwrap()
    };
    for enabled in [true, false] {
        let mut settings = fixture.settings.clone();
        settings.execution_observability = enabled;
        fixture
            .state
            .update_project_engram_settings(&fixture.project, settings)
            .unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.contains(&fixture.project));
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked && record.session.queue_paused);
        assert!(record.runtime_stop_is_owned_by(
            RuntimeStopOwnerKind::UserStop,
            &token,
            generation
        ));
        assert_eq!(
            serde_json::to_value(&record.queued_prompts).unwrap(),
            before
        );
        assert_eq!(
            inner
                .find_project(&fixture.project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .execution_observability,
            enabled
        );
        drop(inner);
        let disk = load_state(&fixture.state.persistence_path)
            .unwrap()
            .unwrap();
        assert_eq!(
            disk.find_project(&fixture.project)
                .unwrap()
                .engram
                .as_ref()
                .unwrap()
                .execution_observability,
            enabled
        );
    }
    assert!(fixture.transport.shutdowns.lock().unwrap().is_empty());
    assert!(fixture.transport.requests.lock().unwrap().is_empty());
    assert!(fixture.receiver.try_recv().is_err());
}

#[test]
fn a_failed_connection_settings_commit_rolls_back_metadata_and_leaves_queue_and_runtime_owned() {
    let fixture = ConnectionFixture::new();
    let dispatch = match fixture.send("active turn before failed settings save") {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("the fixture starts an actual current turn before queueing")
        }
    };
    deliver_turn_dispatch(&fixture.state, dispatch)
        .into_public_result()
        .unwrap();
    fixture.assert_prompt("active turn before failed settings save");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let record = inner.session_mut(&fixture.session).unwrap();
        record.set_auto_dispatch_blocked(true);
        record.session.preview = "Preview before failed save".to_owned();
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    assert!(matches!(
        fixture.send("keep this queued"),
        DispatchTurnResult::Queued
    ));
    let (before, token, preview, reset_required) = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        (
            serde_json::to_value(&record.queued_prompts).unwrap(),
            record.runtime.runtime_token().unwrap(),
            record.session.preview.clone(),
            record.runtime_reset_required,
        )
    };
    fixture.state.shutdown_persist_blocking();
    let mut failing = fixture.state.clone();
    let failed_path = fixture.root.join("reject-settings.sqlite");
    fs::create_dir_all(&failed_path).unwrap();
    failing.persistence_path = Arc::new(failed_path);
    let error = failing
        .update_project_engram_settings(&fixture.project, fixture.disabled())
        .err()
        .expect("a directory cannot accept a SQLite settings commit");
    assert!(
        error
            .message
            .contains("failed to persist Engram project settings"),
        "{}",
        error.message
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        inner
            .find_project(&fixture.project)
            .unwrap()
            .engram
            .as_ref(),
        Some(&fixture.settings)
    );
    assert!(!inner.engram_project_resets.contains(&fixture.project));
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts).unwrap(),
        before
    );
    assert_eq!(record.runtime.runtime_token().as_ref(), Some(&token));
    assert_eq!(record.runtime_reset_required, reset_required);
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.session.preview, preview);
    assert!(
        fixture.transport.shutdowns.lock().unwrap().is_empty(),
        "rollback precedes external cleanup"
    );
    assert!(fixture.receiver.try_recv().is_err());
}

#[test]
fn overlapping_settings_changes_refuse_before_diagnostics_or_cleanup() {
    let fixture = ConnectionFixture::new();
    let generation = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .claim_engram_project_reset(&fixture.project)
        .unwrap();
    let error = fixture
        .state
        .update_project_engram_settings(&fixture.project, fixture.disabled())
        .err()
        .expect("the current connection owner excludes another settings writer");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        error.message,
        "Engram connection change is already in progress"
    );
    assert!(fixture.transport.shutdowns.lock().unwrap().is_empty());
    assert!(fixture.transport.requests.lock().unwrap().is_empty());
    let mut inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        inner
            .find_project(&fixture.project)
            .unwrap()
            .engram
            .as_ref(),
        Some(&fixture.settings)
    );
    assert!(
        inner
            .engram_project_resets
            .is_owned_by(&fixture.project, generation)
    );
    assert!(
        inner
            .engram_project_resets
            .release(&fixture.project, generation)
    );
}

#[test]
fn canceling_a_prompt_during_connection_cleanup_does_not_deliver_it_on_release() {
    for has_successor in [false, true] {
        let fixture = ConnectionFixture::new();
        let (entered, release) = fixture.transport.gate_cleanup();
        std::thread::scope(|scope| {
            let state = &fixture.state;
            let project = &fixture.project;
            let settings = fixture.disabled();
            let change =
                scope.spawn(move || state.update_project_engram_settings(project, settings));
            let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                entered.recv_timeout(DEADLOCK_GUARD).unwrap();
                assert!(matches!(
                    fixture.send("cancel before cleanup finishes"),
                    DispatchTurnResult::Queued
                ));
                if has_successor {
                    assert!(matches!(
                        fixture.send("independent successor"),
                        DispatchTurnResult::Queued
                    ));
                }
                let prompt = {
                    let inner = fixture.state.inner.lock().unwrap();
                    inner.sessions[inner.find_session_index(&fixture.session).unwrap()]
                        .queued_prompts[0]
                        .pending_prompt
                        .id
                        .clone()
                };
                fixture
                    .state
                    .cancel_queued_prompt(&fixture.session, &prompt)
                    .unwrap();
                assert!(fixture.receiver.try_recv().is_err());
            }));
            drop(release);
            let result = change.join().unwrap();
            observed.unwrap_or_else(|failure| std::panic::resume_unwind(failure));
            result.unwrap();
        });
        if has_successor {
            // Release is an independent ordinary drain; only uncanceled B may run.
            fixture.assert_prompt("independent successor");
        } else {
            assert!(fixture.receiver.try_recv().is_err());
        }
        let inner = fixture.state.inner.lock().unwrap();
        assert!(!inner.engram_project_resets.contains(&fixture.project));
        assert!(
            inner.sessions[inner.find_session_index(&fixture.session).unwrap()]
                .queued_prompts
                .is_empty()
        );
        assert!(fixture.transport.requests.lock().unwrap().is_empty());
    }
}

#[test]
fn connection_cleanup_does_not_promote_a_successor_over_a_busy_turn() {
    let fixture = ConnectionFixture::new();
    let dispatch = match fixture.send("already running") {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle fixture must dispatch"),
    };
    deliver_turn_dispatch(&fixture.state, dispatch).unwrap();
    fixture.assert_prompt("already running");
    let generation = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session).unwrap()].active_turn_generation
    };
    assert!(matches!(
        fixture.send("busy successor"),
        DispatchTurnResult::Queued
    ));
    fixture
        .state
        .update_project_engram_settings(&fixture.project, fixture.disabled())
        .unwrap();
    assert!(fixture.receiver.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.active_turn_generation, generation);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "busy successor"
    );
    assert!(!inner.engram_project_resets.contains(&fixture.project));
}

#[test]
fn observability_preference_does_not_activate_or_rebind_3850_historical_sessions() {
    let fixture = ConnectionFixture::new();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let template = inner.sessions[inner.find_session_index(&fixture.session).unwrap()].clone();
        for ordinal in 1..3_850 {
            let mut record = template.clone();
            record.session.id = format!("ordinary-history-{ordinal}");
            record.runtime = SessionRuntime::None;
            record.hidden = ordinal % 2 == 0;
            record.set_auto_dispatch_blocked(true);
            inner.sessions.push(record);
        }
    }
    let mut settings = fixture.settings.clone();
    settings.execution_observability = true;
    fixture
        .state
        .update_project_engram_settings(&fixture.project, settings)
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        inner
            .sessions
            .iter()
            .filter(|record| record.session.project_id.as_deref() == Some(fixture.project.as_str()))
            .count(),
        3_850
    );
    assert_eq!(
        inner.engram_project_resets.next_generation, 0,
        "preference changes claim no resource generation"
    );
    assert!(
        inner
            .sessions
            .iter()
            .filter(|record| record.session.id != fixture.session)
            .all(|record| record.orchestrator_auto_dispatch_blocked
                && record.runtime.runtime_token().is_none())
    );
    assert!(fixture.transport.shutdowns.lock().unwrap().is_empty());
    assert!(fixture.transport.requests.lock().unwrap().is_empty());
    assert!(fixture.receiver.try_recv().is_err());
}

#[test]
fn persisted_queue_survives_restart_without_restoring_a_process_local_connection_fence() {
    let fixture = ConnectionFixture::new();
    let generation = fixture
        .state
        .inner
        .lock()
        .unwrap()
        .claim_engram_project_reset(&fixture.project)
        .unwrap();
    assert!(matches!(
        fixture.send("still ordinary after restart"),
        DispatchTurnResult::Queued
    ));
    let disk = load_state(&fixture.state.persistence_path)
        .unwrap()
        .unwrap();
    assert!(!disk.engram_project_resets.contains(&fixture.project));
    assert!(
        disk.engram_project_reset_claim(&fixture.project, generation)
            .is_none()
    );
    let record = &disk.sessions[disk.find_session_index(&fixture.session).unwrap()];
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(
        record.queued_prompts[0].pending_prompt.text,
        "still ordinary after restart"
    );
    assert!(fixture.receiver.try_recv().is_err());
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .release(&fixture.project, generation)
    );
}
