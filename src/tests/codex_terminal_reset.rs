//! Terminal continuation regressions own their child, including timeout teardown.
use super::*;

const THREAD: &str = "terminal-reset-thread";
const TURN: &str = "terminal-reset-turn";

fn run_child(mode: &str) {
    let module = module_path!().split_once("::").unwrap().1;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{module}::shared_codex_terminal_reset_child"),
            "--nocapture",
        ])
        .env("TERMAL_TEST_TERMINAL_RESET", mode);
    let (status, stderr) = phase_sync::CapturedStderrProcess::spawn(&mut command)
        .wait_with_stderr("terminal callback queued reset dispatch");
    assert!(status.success(), "terminal reset child failed: {stderr}");
    assert!(
        stderr.contains("terminal-reset: successor dispatched"),
        "{stderr}"
    );
}

#[test]
fn shared_codex_terminal_reset_dispatch_does_not_reenter_session_map() {
    run_child("completed");
}

#[test]
fn shared_codex_terminal_reset_deferred_response_releases_map() {
    run_child("response-completed");
    run_child("response-failed");
}

#[test]
fn shared_codex_terminal_reset_deferred_start_releases_map() {
    run_child("start-completed");
    run_child("start-failed");
}

#[test]
fn shared_codex_terminal_reset_failure_and_plain_completion_controls() {
    run_child("failed");
    run_child("error");
    run_child("plain");
}

#[test]
fn shared_codex_terminal_reset_noncapacity_will_retry_still_fails_and_drains() {
    run_child("error-will-retry");
}

struct Fixture {
    state: AppState,
    session: String,
    sibling: String,
    runtime: SharedCodexRuntime,
    commands: mpsc::Receiver<CodexRuntimeCommand>,
    pending: CodexPendingRequestMap,
    _process: phase_sync::ParkedProcess,
}

impl Fixture {
    fn new(reset: bool, queued: bool) -> Self {
        let state = test_app_state();
        let session = test_session_id(&state, Agent::Codex);
        let sibling = test_session_id(&state, Agent::Codex);
        let process = phase_sync::ParkedProcess::spawn();
        let (runtime, commands, _) = test_shared_codex_runtime_with_process(
            "terminal-reset-runtime",
            process.process.clone(),
        );
        *state
            .shared_codex_runtime_slot(SharedCodexProfile::Default)
            .lock()
            .unwrap() = Some(runtime.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let prompt_id = inner.next_message_id();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(
                spawn_codex_runtime(
                    state.clone(),
                    session.clone(),
                    record.session.workdir.clone(),
                    SharedCodexProfile::Default,
                )
                .unwrap(),
            );
            record.attach_to_shared_codex_profile(SharedCodexProfile::Default);
            record.session.status = SessionStatus::Active;
            record.active_turn_generation = 7;
            record.runtime_reset_required = reset;
            set_record_external_session_id(record, Some(THREAD.to_owned()));
            if queued {
                record.queued_prompts.push_back(QueuedPromptRecord {
                    engram_waiting: false,
                    promoted_message_index: None,
                    promotion_disposition_known: true,
                    engram_bind: None,
                    engram_evaluate: None,
                    engram_interrupted: false,
                    source: QueuedPromptSource::User,
                    attachments: Vec::new(),
                    pending_prompt: PendingPrompt {
                        engram_interrupted: false,
                        is_engram_retained: false,
                        attachments: Vec::new(),
                        id: prompt_id,
                        timestamp: stamp_now(),
                        text: "queued reset successor".to_owned(),
                        expanded_text: None,
                        source: None,
                    },
                });
            }
        }
        {
            let mut shared = runtime.sessions.lock().unwrap();
            shared.insert(
                session.clone(),
                SharedCodexSessionState {
                    thread_id: Some(THREAD.to_owned()),
                    turn_id: Some(TURN.to_owned()),
                    active_turn_generation: Some(7),
                    turn_started: true,
                    ..Default::default()
                },
            );
            shared.insert(
                sibling.clone(),
                SharedCodexSessionState {
                    thread_id: Some("sibling-thread".to_owned()),
                    turn_id: Some("sibling-turn".to_owned()),
                    turn_started: true,
                    ..Default::default()
                },
            );
        }
        runtime
            .thread_sessions
            .lock()
            .unwrap()
            .insert(THREAD.to_owned(), session.clone());
        Self {
            state,
            session,
            sibling,
            runtime,
            commands,
            pending: Arc::new(Mutex::new(HashMap::new())),
            _process: process,
        }
    }

    fn token(&self) -> RuntimeToken {
        RuntimeToken::Codex(self.runtime.runtime_id.clone())
    }

    fn event(&self, message: Value) {
        handle_shared_codex_app_server_message(
            &message,
            &self.state,
            &self.runtime.runtime_id,
            &self.pending,
            &self.runtime.sessions,
            &self.runtime.thread_sessions,
            &self.runtime.input_tx,
        )
        .unwrap();
    }

    fn terminal(&self, mode: &str) -> Value {
        if mode == "error" || mode == "error-will-retry" {
            json!({"method":"error", "params":{"threadId":THREAD,"turnId":TURN,
                "willRetry": mode == "error-will-retry",
                "error":{"message":"terminal failure"}}})
        } else {
            let error = if mode == "capacity" {
                Some(json!({"message":"model at capacity", "codexErrorInfo":"serverOverloaded"}))
            } else {
                mode.ends_with("failed")
                    .then(|| json!({"message":"terminal failure"}))
            };
            json!({"method":"turn/completed", "params":{"threadId":THREAD,
                "turn":{"id":TURN,"error":error}}})
        }
    }

    fn arm_response(&self) -> mpsc::Sender<std::result::Result<Value, CodexResponseError>> {
        let mut bytes = Vec::new();
        handle_shared_codex_start_turn(
            &mut bytes,
            &self.pending,
            &self.state,
            &self.runtime.runtime_id,
            &self.runtime.sessions,
            &self.runtime.thread_sessions,
            None,
            &self.session,
            THREAD,
            None,
            CodexPromptCommand {
                active_turn_generation: 7,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "active turn".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
                delivery_receipt: None,
            },
        )
        .unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        take_pending_codex_request(&self.pending).1
    }

    fn assert_successor(&self, reset: bool) {
        let command = phase_sync::receive(&self.commands, "one successor prompt after terminal");
        assert!(
            matches!(command, CodexRuntimeCommand::Prompt { session_id, command, .. }
            if session_id == self.session && command.prompt == "queued reset successor"
                && command.active_turn_generation == 8)
        );
        assert!(self.commands.try_recv().is_err(), "exactly one successor");
        let shared = self.runtime.sessions.lock().unwrap();
        assert_eq!(!shared.contains_key(&self.session), reset);
        assert_eq!(
            shared[&self.sibling].turn_id.as_deref(),
            Some("sibling-turn")
        );
        drop(shared);
        if reset {
            assert!(
                !self
                    .runtime
                    .thread_sessions
                    .lock()
                    .unwrap()
                    .contains_key(THREAD)
            );
        }
        let inner = self.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&self.session).unwrap()];
        assert!(!record.runtime_reset_required);
        assert!(record.queued_prompts.is_empty());
        assert_eq!(
            record.codex_thread_profile,
            Some(SharedCodexProfile::Default)
        );
        assert_eq!(record.session.status, SessionStatus::Active);
        let SessionRuntime::Codex(handle) = &record.runtime else {
            panic!("reattached Codex runtime");
        };
        assert_eq!(handle.runtime_id, self.runtime.runtime_id);
        assert!(handle.shared_session.is_some());
        drop(inner);
        self.state.full_snapshot();
        eprintln!("terminal-reset: successor dispatched");
    }
}

#[test]
fn shared_codex_terminal_reset_child() {
    let Ok(mode) = std::env::var("TERMAL_TEST_TERMINAL_RESET") else {
        return;
    };
    assert!(
        [
            "completed",
            "failed",
            "error",
            "error-will-retry",
            "plain",
            "response-completed",
            "response-failed",
            "start-completed",
            "start-failed"
        ]
        .contains(&mode.as_str())
    );
    let reset = mode != "plain";
    let fixture = Fixture::new(reset, true);
    eprintln!("terminal-reset: delivering {mode} to queued reset");
    if mode.starts_with("response-") || mode.starts_with("start-") {
        let response = fixture.arm_response();
        // The terminal cannot establish its own identity: it is buffered until
        // an independent response or explicit start proves the active turn.
        fixture.event(fixture.terminal(&mode));
        assert_eq!(
            fixture.runtime.sessions.lock().unwrap()[&fixture.session]
                .deferred_terminals
                .len(),
            1
        );
        if mode.starts_with("response-") {
            response.send(Ok(json!({"turn":{"id":TURN}}))).unwrap();
        } else {
            fixture.event(
                json!({"method":"turn/started", "params":{"threadId":THREAD,"turn":{"id":TURN}}}),
            );
            response.send(Ok(json!({"turn":{"id":TURN}}))).unwrap();
        }
    } else {
        fixture.event(fixture.terminal(&mode));
    }
    fixture.assert_successor(reset);
}

#[test]
fn shared_codex_terminal_effect_refuses_successor_generation_and_attachment() {
    for replacement in ["generation", "attachment"] {
        for terminal in ["completed", "failed", "error", "capacity"] {
            let fixture = Fixture::new(false, false);
            let message = fixture.terminal(terminal);
            let effect = {
                let mut shared = fixture.runtime.sessions.lock().unwrap();
                apply_owned_shared_codex_notification(
                    message["method"].as_str().unwrap(),
                    &message,
                    &fixture.state,
                    &fixture.session,
                    &fixture.token(),
                    &fixture.runtime.thread_sessions,
                    shared.get_mut(&fixture.session).unwrap(),
                )
                .unwrap()
                .unwrap()
            };
            // This is the deterministic barrier between extraction and apply.
            let generation = if replacement == "generation" { 8 } else { 7 };
            {
                let mut inner = fixture.state.inner.lock().unwrap();
                let i = inner.find_session_index(&fixture.session).unwrap();
                let record = &mut inner.sessions[i];
                record.active_turn_generation = generation;
                record.session.status = SessionStatus::Active;
                record.session.preview = "successor stays active".to_owned();
                record.prompt_deliveries.insert(
                    "successor-receipt".to_owned(),
                    PromptDelivery {
                        disposition: PromptDeliveryDisposition::HandingOff,
                        attempt: 1,
                        write_ahead: "successor-write".to_owned(),
                        sequence: 1,
                        message_id: None,
                        turn_generation: Some(generation),
                        attempt_key: None,
                        reason: None,
                        entry: None,
                        write_ahead_acknowledged: true,
                        resume: None,
                        runtime_write: PromptRuntimeWrite::Begun,
                    },
                );
            }
            if replacement == "attachment" {
                fixture.runtime.sessions.lock().unwrap().insert(
                    fixture.session.clone(),
                    SharedCodexSessionState {
                        thread_id: Some(THREAD.to_owned()),
                        active_turn_generation: Some(7),
                        completed_turn_id: Some(TURN.to_owned()),
                        ..Default::default()
                    },
                );
            }
            let nonce = fixture
                .state
                .prepare_provider_capacity_failure(
                    &fixture.session,
                    &fixture.token(),
                    Some(generation),
                    true,
                    &json!({"codexErrorInfo":"serverOverloaded"}),
                    false,
                )
                .unwrap();
            let before = fixture.state.full_snapshot();
            effect
                .apply(&fixture.state, &fixture.runtime.sessions)
                .unwrap();
            let after = fixture.state.full_snapshot();
            assert_eq!(
                serde_json::to_value(before.sessions).unwrap(),
                serde_json::to_value(after.sessions).unwrap(),
                "stale {terminal} changed {replacement} successor"
            );
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            assert_eq!(
                record.provider_capacity_retry.as_ref().unwrap().nonce,
                nonce
            );
            assert_eq!(
                record.prompt_deliveries["successor-receipt"].disposition,
                PromptDeliveryDisposition::HandingOff
            );
            drop(inner);
            assert!(fixture.commands.try_recv().is_err());
        }
    }
}

#[test]
fn shared_codex_terminal_cleanup_keeps_reinserted_identical_turn_ids() {
    let fixture = Fixture::new(false, false);
    let owner = {
        let shared = fixture.runtime.sessions.lock().unwrap();
        SharedCodexTerminalIdentity::capture(
            &fixture.state,
            &fixture.session,
            &fixture.token(),
            &shared[&fixture.session],
        )
        .unwrap()
    };
    fixture.runtime.sessions.lock().unwrap().insert(
        fixture.session.clone(),
        SharedCodexSessionState {
            thread_id: Some(THREAD.to_owned()),
            active_turn_generation: Some(7),
            completed_turn_id: Some(TURN.to_owned()),
            recorder: SessionRecorderState {
                streaming_text_message_id: Some("new-draft".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mut pending = vec![SharedCodexCompletedTurnCleanup {
        owner,
        completed_turn_id: TURN.to_owned(),
        due_at: std::time::Instant::now(),
    }];
    assert!(run_due_shared_codex_completed_turn_cleanups(
        &Arc::downgrade(&fixture.runtime.sessions),
        &mut pending
    ));
    assert!(pending.is_empty());
    let shared = fixture.runtime.sessions.lock().unwrap();
    assert_eq!(
        shared[&fixture.session].completed_turn_id.as_deref(),
        Some(TURN)
    );
    assert_eq!(
        shared[&fixture.session]
            .recorder
            .streaming_text_message_id
            .as_deref(),
        Some("new-draft")
    );
}
