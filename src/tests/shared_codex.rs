//! Shared Codex request, timeout, and runtime lifecycle tests.
//!
//! Owns runtime failure/recovery seams after a thread is bound. Deliberately
//! does not own setup/parking, event routing, text reconciliation, or
//! retry/error-notification coverage; those live in focused sibling modules.

use super::shared_codex_thread_setup::{
    answer_engram_config_for_test, answer_pending_codex_thread_setups,
    create_test_engram_codex_session, finish_engram_config_for_test,
    run_engram_config_continuation_for_test, shared_codex_setup_request_for_mcp_test,
    test_pending_codex_thread_setup,
};
use super::*;

#[test]
fn shared_codex_prompt_dispatch_clears_stale_command_state_before_turn_started_notification() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-dispatch-clears-stale-state");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    state
        .upsert_command_message(
            &session_id,
            "old-command-message",
            "Web search: previous turn",
            "previous turn",
            CommandStatus::Success,
        )
        .unwrap();

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "webSearch".to_owned(),
                        "old-command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::new(),
                    streaming_text_message_id: Some("stale-stream".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    // The session already has a thread_id so the fast path is taken and
    // input_tx is unused, but the parameter is still required.
    let (dummy_input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &dummy_input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "check the repo".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();
    let response_sender = {
        let mut pending = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned");
        let request_id = pending
            .keys()
            .next()
            .cloned()
            .expect("fire-and-forget turn/start request should be pending after dispatch");
        pending
            .remove(&request_id)
            .expect("pending turn/start response sender should exist")
    };
    response_sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-new"
            }
        })))
        .expect("turn/start waiter should accept the scripted response");

    let item_started = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-new"
            }
        }
    });
    let item_completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &item_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &item_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: previous turn".to_owned(),
                "previous turn".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Pins the turn/start race: if turn/started lands while the turn/start
// JSON-RPC request is still being written, the fast notification must NOT
// reintroduce pending_turn_start_request_id once handle_shared_codex_start_turn
// returns — its post-write state merge wins.
// Guards against the notification path re-setting pending state that the
// writer path has already cleared.
#[test]
fn shared_codex_turn_started_notification_does_not_restore_pending_state() {
    struct RaceWriter<F: FnMut()> {
        buffer: Vec<u8>,
        injected: bool,
        on_turn_start_written: F,
    }

    impl<F: FnMut()> std::io::Write for RaceWriter<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            if !self.injected && self.buffer.ends_with(b"\n") {
                let line = std::str::from_utf8(&self.buffer)
                    .expect("turn/start payload should stay valid UTF-8");
                if line.contains("\"method\":\"turn/start\"") {
                    self.injected = true;
                    (self.on_turn_start_written)();
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-turn-start-race");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let callback_state = state.clone();
    let callback_pending_requests = pending_requests.clone();
    let callback_sessions = runtime.sessions.clone();
    let callback_thread_sessions = runtime.thread_sessions.clone();
    let callback_runtime_id = runtime.runtime_id.clone();
    let (callback_input_tx, _callback_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = RaceWriter {
        buffer: Vec::new(),
        injected: false,
        on_turn_start_written: move || {
            handle_shared_codex_app_server_message(
                &json!({
                    "method": "turn/started",
                    "params": {
                        "threadId": "conversation-123",
                        "turn": {
                            "id": "turn-fast"
                        }
                    }
                }),
                &callback_state,
                &callback_runtime_id,
                &callback_pending_requests,
                &callback_sessions,
                &callback_thread_sessions,
                &callback_input_tx,
            )
            .expect("turn/started callback should be handled");
        },
    };

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "conversation-123",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "inspect race handling".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: Some("priority".to_owned()),
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let written = String::from_utf8(writer.buffer.clone()).expect("Codex request should be UTF-8");
    assert!(
        written.contains("\"serviceTier\":\"priority\""),
        "turn/start should include the session-scoped Fast service tier\n{written}"
    );

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-fast"));
        assert!(session_state.turn_started);
        assert_eq!(session_state.pending_turn_start_request_id, None);
    }

    let (_request_id, sender) = take_pending_codex_request(&pending_requests);
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-fast"
            }
        })))
        .unwrap();
}

#[test]
fn shared_codex_thread_setup_handoff_failure_rolls_back_registration() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-handoff-failure");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    drop(input_rx);

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) = take_pending_codex_request(&pending_requests);
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-orphan"
            }
        })))
        .unwrap();

    let deadline = phase_sync::PollGuard::new();
    loop {
        let (runtime_cleared, external_session_id, status, preview) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("Codex session should exist");
            let record = &inner.sessions[index];
            (
                matches!(record.runtime, SessionRuntime::None),
                record.external_session_id.clone(),
                record.session.status,
                record.session.preview.clone(),
            )
        };
        let (shared_thread_id, has_thread_mapping) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let thread_id = sessions
                .get(&session_id)
                .and_then(|session| session.thread_id.clone());
            drop(sessions);
            let thread_sessions = runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned");
            (
                thread_id,
                thread_sessions.contains_key("conversation-orphan"),
            )
        };

        if runtime_cleared
            && external_session_id.is_none()
            && shared_thread_id.is_none()
            && !has_thread_mapping
        {
            assert_eq!(status, SessionStatus::Error);
            assert!(preview.contains("failed to queue shared Codex turn/start after thread setup"));
            break;
        }

        deadline.wait(format_args!(
            "failed StartTurnAfterSetup handoff should roll back provisional thread registration"
        ));
    }
}

// Pins that a persistence failure during post-thread-setup state commit
// leaves the shared Codex runtime handle intact on the session, marks the
// session Error with a generic "Failed to save session state" preview, and
// blocks the StartTurnAfterSetup command from being queued.
// Guards against persistence IO errors tearing down a healthy runtime or
// leaking a thread mapping the caller cannot recover from.
#[test]
fn shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-persist-failure");
    let failing_persistence_path = test_temp_dir().join(format!(
        "termal-shared-codex-thread-setup-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        None,
        &session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) = take_pending_codex_request(&pending_requests);
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-persist-failure"
            }
        })))
        .unwrap();

    let deadline = phase_sync::PollGuard::new();
    loop {
        match input_rx.try_recv() {
            Ok(_) => panic!("failed thread registration should not queue StartTurnAfterSetup"),
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("test input channel should remain open")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        let failed = {
            let snapshot = state.full_snapshot();
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    session.status == SessionStatus::Error
                        && session.preview.contains("Failed to save session state")
                })
        };
        if failed {
            break;
        }
        deadline.wait(format_args!(
            "failed thread registration should mark the session error"
        ));
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        assert!(matches!(
            &inner.sessions[index].runtime,
            SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
        ));
    }
    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("conversation-persist-failure"),
        "failed thread registration should not publish a shared thread mapping"
    );

    // The app-server already wrote this thread to disk, and the persist that would
    // have made the record claim it just failed — so nothing owns it. It must be
    // disowned, exactly as every sibling failure branch does. Without this the next
    // discovery scan imports it as a phantom unlinked top-level session: the very
    // leak this change exists to close, reintroduced through the one branch that
    // forgot.
    //
    // Suppression persists too, so on a genuinely full disk it would fail as well —
    // but it updates the in-memory ignore set first, which is what we can observe
    // here and what holds for transient/permission failures.
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .ignored_discovered_codex_thread_ids
                .contains("conversation-persist-failure"),
            "a thread whose registration failed to persist must be disowned, or discovery \
             re-imports it as a phantom top-level session"
        );
    }

    remove_test_directory(failing_persistence_path);
}

// Pins that handle_shared_codex_start_turn with a runtime_id that no
// longer matches the session's current runtime is a no-op: no bytes
// written, no pending request registered, no session entry inserted, and
// active_codex_* config fields stay None.
// Guards against stale StartTurnAfterSetup handoffs writing a different
// runtime's config onto the current session.
#[test]
fn shared_codex_stale_start_turn_handoff_skips_runtime_config_persistence() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (other_runtime, _other_input_rx) = test_codex_runtime_handle("other-runtime");
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(other_runtime);
        inner.sessions[index].active_codex_approval_policy = None;
        inner.sessions[index].active_codex_reasoning_effort = None;
        inner.sessions[index].active_codex_sandbox_mode = None;
    }

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "stale-runtime",
        &sessions,
        &thread_sessions,
        None,
        &session_id,
        "conversation-stale",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::OnRequest,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale handoff".to_owned(),
            reasoning_effort: CodexReasoningEffort::XHigh,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::DangerFullAccess,
        },
    )
    .unwrap();

    assert!(writer.is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert!(
        sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .is_empty()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert_eq!(inner.sessions[index].active_codex_approval_policy, None);
    assert_eq!(inner.sessions[index].active_codex_reasoning_effort, None);
    assert_eq!(inner.sessions[index].active_codex_sandbox_mode, None);
}

// A `StartTurnAfterSetup` hand-off is enqueued by a WAITER thread, and the waiter
// clears the setup slot BEFORE it sends (`complete_shared_codex_thread_setup` returns,
// THEN `input_tx.send`). The writer is free to run in that gap — so a detach plus a
// fresh prompt can land in between, and the fresh prompt claims a NEW setup. The
// hand-off then arrives at a session that re-armed underneath it.
//
// The runtime-id check at the top of `handle_shared_codex_start_turn` does NOT catch
// this. Every session on the shared app-server carries the SAME `runtime_id` — it is
// cloned straight off `SharedCodexRuntime` in `spawn_codex_runtime` — so detach and
// re-attach yield the same id and the check returns `Applied`. It is a PROCESS check,
// not an ATTACHMENT check.
//
// This was previously "impossible": the code asserted no setup could be in flight here,
// on the theory that prompt handling and turn start are serialized on the writer thread.
// They are — but the hand-off is enqueued by a WAITER, and serializing the writer says
// nothing about what a waiter puts on the queue. The assert was reachable. In debug it
// panicked while HOLDING the shared-session mutex, poisoning it for every other Codex
// session on the shared runtime; in release it destroyed the prompt the user had just
// typed and started a stale turn on the detached attachment's thread.
#[test]
fn stale_start_turn_handoff_leaves_the_setup_that_re_armed_the_session_alone() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime_handle, _runtime_input_rx) = test_codex_runtime_handle("shared-app-server");
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    // Attached to the shared app-server, so the runtime-id check passes — exactly as it
    // does after a real detach + re-attach, because the id belongs to the process.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime_handle);
        state.commit_locked(&mut inner).unwrap();
    }

    // The session re-armed: a NEW setup is in flight with the user's freshly-typed
    // prompt parked on it.
    {
        let mut guard = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = guard.entry(session_id.clone()).or_default();
        session_state.pending_thread_setup = Some(PendingCodexThreadSetup {
            request_id: "setup-b".to_owned(),
            command: CodexPromptCommand {
                active_turn_generation: 0,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "the prompt the user just typed".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        });
    }

    // The stale hand-off, carrying the DETACHED attachment's thread and prompt.
    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "shared-app-server",
        &sessions,
        &thread_sessions,
        None,
        &session_id,
        "thread-from-the-detached-attachment",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale prompt from before the detach".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("a stale hand-off must be abandoned quietly, not fail the shared writer thread");

    let guard = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = guard
        .get(&session_id)
        .expect("the re-armed session state should still exist");

    let parked = session_state
        .pending_thread_setup
        .as_ref()
        .expect("the setup that re-armed the session must survive a stale hand-off");
    assert_eq!(parked.request_id, "setup-b");
    assert_eq!(
        parked.command.prompt, "the prompt the user just typed",
        "the stale hand-off destroyed the prompt parked on the session's CURRENT setup"
    );
    assert_eq!(
        session_state.thread_id, None,
        "the stale hand-off must not bind the detached attachment's thread onto the \
         re-armed session — its own setup will bind the right one"
    );
    assert!(
        session_state.pending_turn_start_request_id.is_none(),
        "a stale hand-off must not start a turn"
    );
    assert!(
        writer.is_empty(),
        "a stale hand-off must not write turn/start to the shared app-server"
    );
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty(),
        "a stale hand-off must not register a pending request"
    );
}

// Pins that set_external_session_id_if_runtime_matches surfaces Err when
// the commit fails rather than silently collapsing into the stale-session
// "skip" branch that returns Ok(()).
// Guards against persistence failures being masked as routine stale-runtime
// misses and thus going undetected by callers.
#[test]
fn set_external_session_id_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-thread-id-runtime");
    let failing_persistence_path = test_temp_dir().join(format!(
        "termal-codex-thread-id-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let result = state.set_external_session_id_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-thread-id-runtime".to_owned()),
        "conversation-123".to_owned(),
    );

    assert!(
        result.is_err(),
        "commit failures should not collapse into stale-session misses"
    );
    remove_test_directory(failing_persistence_path);
}

// Pins that record_codex_runtime_config_if_runtime_matches propagates
// persistence errors as Err instead of folding them into the stale-session
// skip path — the sibling helper to set_external_session_id_if_runtime_matches.
// Guards against runtime-config persistence errors being lost to callers.
#[test]
fn record_codex_runtime_config_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-runtime-config-runtime");
    let failing_persistence_path = test_temp_dir().join(format!(
        "termal-codex-runtime-config-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let result = state.record_codex_runtime_config_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-runtime-config-runtime".to_owned()),
        0,
        CodexSandboxMode::WorkspaceWrite,
        CodexApprovalPolicy::Never,
        CodexReasoningEffort::Medium,
    );

    assert!(
        result.is_err(),
        "persistence failures should remain fatal to the caller"
    );
    remove_test_directory(failing_persistence_path);
}

// Pins that a persistence failure during handle_shared_codex_start_turn's
// runtime-config commit keeps the shared runtime attached to the session
// (SessionRuntime::Codex stays), flips session to Error, and records a
// "Turn failed: Failed to save session state" transcript message.
// Guards against one session's persistence IO killing the shared process.
#[test]
fn shared_codex_start_turn_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-start-turn-persist-failure");
    let failing_persistence_path = test_temp_dir().join(format!(
        "termal-shared-codex-start-turn-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &Arc::new(Mutex::new(HashMap::new())),
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "conversation-123",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert!(matches!(
        &inner.sessions[index].runtime,
        SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
    ));
    drop(inner);

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));

    remove_test_directory(failing_persistence_path);
}

// Pins the shared_codex_event_matches_visible_turn predicate: it accepts
// events for the active turn_id, accepts events whose turn_id matches the
// completed_turn_id ONLY when there is no active turn, and rejects
// orphan/mismatched turn ids.
// Guards against the grace-period branch firing while a new turn is live,
// or orphan events being accepted with no turn context at all.
#[test]
fn shared_codex_event_matches_visible_turn_handles_active_and_completed_turns() {
    assert!(shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        None,
        Some("turn-active"),
    ));
    assert!(shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-completed"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-other"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        None,
    ));
    // No active or completed turn — event with a turn ID is rejected.
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        None,
        Some("turn-orphan"),
    ));
    // Active turn differs from event but completed turn matches — the
    // completed branch is only entered when current_turn_id is None.
    assert!(!shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        Some("turn-completed"),
        Some("turn-completed"),
    ));
}

// Pins the app-server error classifier: only the exact "session
// `<id>` not found" shape (possibly wrapped in context) counts as a stale
// session; "session ... message ... not found", "anchor message not
// found", and unrelated errors are fatal.
// Guards against over-broad stale-session matching that would swallow
// genuine persist/runtime failures as routine stale-session skips.
#[test]
fn shared_codex_app_server_error_classifier_only_ignores_missing_sessions() {
    assert!(shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` not found"
    )));
    assert!(shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` message `message-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` anchor message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "failed to persist Codex notice"
    )));
}

// Pins that a server-initiated JSON-RPC request whose thread_id maps to
// no known session is auto-rejected with a -32001 "Session unavailable"
// error, routed back through the CodexRuntimeCommand::JsonRpcResponse
// writer-loop path.
// Guards against Codex hanging on requests the runtime cannot deliver.
#[test]
fn shared_codex_undeliverable_server_request_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-missing-session",
            "method": "session/request_permission",
            "params": {
                "threadId": "missing-thread"
            }
        }),
        &state,
        "shared-codex-missing-session",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match recv_within_guard(
        &input_rx,
        "shared codex undeliverable server request returns json rpc error: runtime command 1",
    )
    .unwrap()
    {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-missing-session",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that a request with an explicit but unknown thread id is rejected even
// if its turn id matches an active session. The turn-id fallback is only safe
// for messages that truly lack thread identity.
#[test]
fn shared_codex_server_request_with_unknown_thread_id_does_not_fallback_to_turn_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-wrong-thread-no-turn-fallback");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-live".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_app_server_message(
        &json!({
            "method": "item/completed",
            "params": {
                "threadId": "wrong-thread",
                "turnId": "turn-live",
                "item": {
                    "id": "msg-wrong-thread",
                    "type": "agentMessage",
                    "text": "Wrong-thread final answer."
                }
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-wrong-thread",
            "method": "session/request_permission",
            "params": {
                "threadId": "wrong-thread",
                "turnId": "turn-live"
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match recv_within_guard(&input_rx, "shared codex server request with unknown thread id does not fallback to turn id: runtime command 1").unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-wrong-thread",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());
}

// Pins that a request routed by the completed-turn grace window is still
// answered with an error when there is no active turn. It must not be
// silently dropped, because Codex waits for JSON-RPC request responses.
#[test]
fn shared_codex_server_request_for_completed_turn_returns_json_rpc_error() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-completed-turn-request");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Idle;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id,
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                completed_turn_id: Some("turn-completed".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-completed-turn",
            "method": "session/request_permission",
            "params": {
                "turnId": "turn-completed"
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match recv_within_guard(
        &input_rx,
        "shared codex server request for completed turn returns json rpc error: runtime command 1",
    )
    .unwrap()
    {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-completed-turn",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that a server-initiated JSON-RPC request with no thread id is
// rejected instead of being logged-and-dropped. Newer Codex app-server
// builds can emit global requests such as auth-token refresh; leaving
// those unanswered stalls the shared app-server and makes Codex turns
// look permanently active.
#[test]
fn shared_codex_server_request_missing_thread_id_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-without-thread",
            "method": "account/chatgptAuthTokens/refresh",
            "params": {}
        }),
        &state,
        "shared-codex-missing-thread",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match recv_within_guard(
        &input_rx,
        "shared codex server request missing thread id returns json rpc error: runtime command 1",
    )
    .unwrap()
    {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-without-thread",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that while handle_shared_codex_prompt_command blocks waiting on a
// turn/start JSON-RPC response, the writer loop still accepts and
// forwards other CodexRuntimeCommand::JsonRpcResponse items (e.g. an
// approval reply) to the shared stdin.
// Guards against turn/start dispatch starving the writer loop and stalling
// concurrent approval/response traffic on the shared Codex process.
#[test]
fn shared_codex_prompt_command_keeps_writer_loop_responsive_while_turn_start_is_pending() {
    assert_prompt_writer_fixture_cleanup(None);
}

#[test]
fn shared_codex_prompt_writer_fixture_releases_waiters_on_unwind() {
    assert_prompt_writer_fixture_cleanup(Some("watchdog"));
}

#[test]
fn shared_codex_prompt_writer_fixture_joins_writer_on_early_unwind() {
    assert_prompt_writer_fixture_cleanup(Some("writer"));
}

#[test]
fn shared_codex_prompt_writer_fixture_reports_writer_failure_after_cleanup() {
    assert_prompt_writer_fixture_cleanup(Some("writer-failure"));
}

#[test]
fn shared_codex_prompt_writer_fixture_survives_writer_failure_during_outer_unwind() {
    assert_prompt_writer_fixture_cleanup(Some("writer-failure-unwind"));
}

#[test]
fn shared_codex_prompt_writer_fixture_cleans_poisoned_pending_requests_during_unwind() {
    assert_prompt_writer_fixture_cleanup(Some("pending-poison-unwind"));
}

fn assert_prompt_writer_fixture_cleanup(panic_phase: Option<&'static str>) {
    let state = test_app_state();
    let temp_path = state.test_temp_root_path().unwrap().to_owned();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-writer-responsive");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let cleanup_pending = pending_requests.clone();
    let cleanup_session = SharedCodexSessionHandle {
        runtime: runtime.clone(),
        session_id: session_id.clone(),
    };
    let (input_tx, input_rx) = mpsc::channel::<Option<CodexRuntimeCommand>>();
    let cleanup_input = input_tx.clone();
    type FixtureWriter = std::thread::JoinHandle<mpsc::Receiver<Option<CodexRuntimeCommand>>>;
    let writer_owner = Arc::new(Mutex::new(None::<FixtureWriter>));
    let cleanup_writer = writer_owner.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let state = TestAppStateCleanup::new(
            state,
            "shared Codex turn/start response/watchdog",
            move || {
                // Stop and join the fixture writer even if an assertion unwinds
                // before the second command. Only then release its continuations.
                let _ = cleanup_input.send(None);
                let mut failures = Vec::new();
                // Do not hold the owner lock through join: a failing child must
                // not poison it or prevent the remaining cancellation steps.
                let writer = cleanup_writer
                    .lock()
                    .unwrap_or_else(|poisoned| {
                        failures.push("fixture writer owner mutex poisoned".to_owned());
                        poisoned.into_inner()
                    })
                    .take();
                if let Some(writer) = writer {
                    if writer.join().is_err() {
                        failures.push("fixture writer teardown: writer panicked".to_owned());
                    }
                }
                cleanup_pending
                    .lock()
                    .unwrap_or_else(|poisoned| {
                        failures.push("pending requests mutex poisoned".to_owned());
                        poisoned.into_inner()
                    })
                    .clear();
                cleanup_session.detach();
                if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("; "))
                }
            },
        );
        let thread_state = (*state).clone();
        let thread_runtime = runtime.clone();
        let thread_input_tx = runtime.input_tx.clone();

        let writer_thread = std::thread::spawn(move || {
            let mut stdin = thread_writer;
            let runtime_token = RuntimeToken::Codex(thread_runtime.runtime_id.clone());
            // This bound-thread fixture publishes a prompt and an approval reply.
            // Return the receiver after those events, not scheduler idleness.
            // The join handle retains it until the response continuation settles.
            for _ in 0..2 {
                let Some(command) = phase_sync::receive(&input_rx, "Codex prompt-loop command")
                else {
                    return input_rx;
                };
                match command {
                    CodexRuntimeCommand::Prompt {
                        session_id,
                        command,
                    } => {
                        let active_turn_generation = command.active_turn_generation;
                        handle_shared_codex_prompt_command_result(
                            &thread_state,
                            &session_id,
                            &runtime_token,
                            active_turn_generation,
                            handle_shared_codex_prompt_command(
                                &mut stdin,
                                &thread_pending_requests,
                                &thread_state,
                                &thread_runtime.runtime_id,
                                &test_missing_shared_codex_home(),
                                &thread_runtime.sessions,
                                &thread_runtime.thread_sessions,
                                &thread_input_tx,
                                None,
                                &session_id,
                                command,
                            ),
                        )
                        .unwrap();
                    }
                    CodexRuntimeCommand::StartTurnAfterSetup {
                        session_id,
                        thread_id,
                        command,
                    } => {
                        let active_turn_generation = command.active_turn_generation;
                        handle_shared_codex_prompt_command_result(
                            &thread_state,
                            &session_id,
                            &runtime_token,
                            active_turn_generation,
                            handle_shared_codex_start_turn(
                                &mut stdin,
                                &thread_pending_requests,
                                &thread_state,
                                &thread_runtime.runtime_id,
                                &thread_runtime.sessions,
                                &thread_runtime.thread_sessions,
                                None,
                                &session_id,
                                &thread_id,
                                None,
                                command,
                            ),
                        )
                        .unwrap();
                    }
                    CodexRuntimeCommand::JsonRpcResponse { response } => {
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &codex_json_rpc_response_message(&response),
                        )
                        .unwrap();
                    }
                    _ => panic!("unexpected shared Codex runtime command"),
                }
                if matches!(
                    panic_phase,
                    Some("writer-failure" | "writer-failure-unwind")
                ) {
                    panic!("injected fixture writer failure with pending response");
                }
            }
            input_rx
        });
        *writer_owner
            .lock()
            .expect("fixture writer owner mutex poisoned") = Some(writer_thread);

        input_tx
            .send(Some(CodexRuntimeCommand::Prompt {
                session_id: session_id.clone(),
                command: CodexPromptCommand {
                    active_turn_generation: 0,
                    approval_policy: CodexApprovalPolicy::Never,
                    attachments: Vec::new(),
                    cwd: "/tmp".to_owned(),
                    model: "gpt-5.4".to_owned(),
                    prompt: "check the repo".to_owned(),
                    reasoning_effort: CodexReasoningEffort::Medium,
                    service_tier: None,
                    resume_thread_id: None,
                    sandbox_mode: CodexSandboxMode::WorkspaceWrite,
                },
            }))
            .unwrap();
        if panic_phase == Some("writer") {
            panic!("injected fixture failure while writer is active");
        }
        if matches!(
            panic_phase,
            Some("writer-failure" | "writer-failure-unwind")
        ) {
            let deadline = phase_sync::PollGuard::new();
            while !writer_owner
                .lock()
                .expect("fixture writer owner mutex poisoned")
                .as_ref()
                .expect("fixture writer installed")
                .is_finished()
            {
                deadline.wait("injected writer failure must precede fixture cleanup");
            }
            if panic_phase == Some("writer-failure-unwind") {
                panic!("injected outer unwind after writer failure");
            }
            state.finish();
            return;
        }

        let deadline = phase_sync::PollGuard::new();
        loop {
            let written = writer.contents();
            let pending_count = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .len();
            if written.contains("\"method\":\"turn/start\"") && pending_count == 1 {
                break;
            }
            deadline.wait(format_args!(
                "turn/start request should stay pending while the writer loop remains active"
            ));
        }

        input_tx
            .send(Some(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: json!("approval-1"),
                    payload: CodexJsonRpcResponsePayload::Result(json!({
                        "outcome": "approved",
                    })),
                },
            }))
            .unwrap();

        let deadline = phase_sync::PollGuard::new();
        loop {
            let written = writer.contents();
            if written.contains("\"id\":\"approval-1\"") {
                break;
            }
            deadline.wait(format_args!(
                "writer loop should still write JSON-RPC responses while turn/start is pending"
            ));
        }

        let (_request_id, sender) = take_pending_codex_request(&pending_requests);
        sender
            .send(Ok(json!({
                "turn": {
                    "id": "turn-1"
                }
            })))
            .unwrap();

        let deadline = phase_sync::PollGuard::new();
        loop {
            let (turn_id, pending_turn_start) = {
                let sessions = runtime
                    .sessions
                    .lock()
                    .expect("shared Codex session mutex poisoned");
                let session_state = sessions
                    .get(&session_id)
                    .expect("shared Codex session state should exist");
                (
                    session_state.turn_id.clone(),
                    session_state.pending_turn_start_request_id.clone(),
                )
            };
            if turn_id.as_deref() == Some("turn-1") && pending_turn_start.is_some() {
                break;
            }
            deadline.wait(format_args!("turn/start waiter should record the turn id and retain its watchdog marker until turn/started arrives"));
        }

        let input_rx = writer_owner
            .lock()
            .expect("fixture writer owner mutex poisoned")
            .take()
            .expect("fixture writer installed")
            .join()
            .expect("shared Codex writer thread should join cleanly");
        assert!(
            matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "Codex prompt-loop fixture received an extra command after prompt and approval reply"
        );
        drop(input_tx);
        if panic_phase == Some("pending-poison-unwind") {
            let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _pending = pending_requests.lock().expect("pending requests mutex");
                panic!("injected pending requests poison");
            }));
            assert!(poison.is_err());
            panic!("injected outer unwind after pending requests poison");
        }
        if panic_phase == Some("watchdog") {
            panic!("injected fixture failure after watchdog arm");
        }
        state.finish();
    }));
    assert_eq!(result.is_err(), panic_phase.is_some());
    if let Err(payload) = result {
        let detail = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if panic_phase == Some("writer-failure") {
            assert!(
                detail.contains("fixture writer teardown"),
                "cleanup failure must be reported: {detail}"
            );
        } else if matches!(
            panic_phase,
            Some("writer-failure-unwind" | "pending-poison-unwind")
        ) {
            assert!(
                detail.contains("injected outer unwind"),
                "cleanup must preserve the original panic: {detail}"
            );
        }
    }
    assert!(!temp_path.exists(), "fixture must remove its database root");
}

// Pins that a CodexResponseError::JsonRpc from turn/start is recorded as
// a session-scoped turn failure (status Error, preview set to the error
// message, "Turn failed: ..." transcript message), while the shared
// runtime handle stays attached to the session record.
// Guards against one turn's JSON-RPC rejection tearing down the entire
// shared Codex process for all co-hosted sessions.
#[test]
fn shared_codex_prompt_json_rpc_errors_fail_the_turn_without_tearing_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-jsonrpc-error");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &runtime_token,
        0,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "turn/start rejected the request".to_owned(),
        ))),
    )
    .expect("JSON-RPC prompt errors should be recorded as turn failures");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "turn/start rejected the request");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Turn failed: turn/start rejected the request"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
}

// A successful JSON-RPC response without the matching turn/started event must
// not leave the durable turn Active forever. The watchdog is request-scoped,
// clears the stale attachment, and records one terminal failure.
#[test]
fn shared_codex_turn_started_watchdog_terminalizes_its_current_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "LIVE TURN".to_owned();
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-watchdog".to_owned()),
                active_turn_generation: Some(0),
                thread_id: Some("thread-watchdog".to_owned()),
                turn_id: Some("turn-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-watchdog".to_owned(), session_id.clone());

    let interrupt = std::thread::spawn(move || {
        let command = recv_within_guard(
            &input_rx,
            "watchdog should interrupt the accepted turn before terminalizing it",
        )
        .expect("watchdog should interrupt the accepted turn before terminalizing it");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                response_tx,
                thread_id,
                turn_id,
            } => {
                assert_eq!(thread_id, "thread-watchdog");
                assert_eq!(turn_id, "turn-watchdog");
                response_tx
                    .send(Ok(()))
                    .expect("watchdog interrupt acknowledgement should be received");
            }
            _ => panic!("watchdog should issue an exact turn interrupt"),
        }
    });

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-watchdog",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("did not emit turn/started"));
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-watchdog")
    );

    let _ = process.kill();
    let _ = process.wait();
}

// A response without a turn id cannot be interrupted exactly. The app-server
// may still be executing tools after TermAl terminalizes the durable turn, so
// fail closed by retiring the shared runtime and every sibling attachment.
#[test]
fn shared_codex_turn_started_watchdog_missing_turn_id_retires_shared_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let sibling_id = test_session_id(&state, Agent::Codex);
    let process_owner = phase_sync::ParkedProcess::spawn();
    let (runtime, input_rx, process) = test_shared_codex_runtime_with_process(
        "shared-codex-watchdog-missing-turn-id",
        process_owner.process.clone(),
    );
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (id, generation, preview) in [
            (&session_id, 1, "MISSING TURN ID"),
            (&sibling_id, 7, "SIBLING TURN"),
        ] {
            let index = inner
                .find_session_index(id)
                .expect("Codex session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: id.clone(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].session.preview = preview.to_owned();
            inner.sessions[index].active_turn_generation = generation;
        }
    }
    {
        let mut sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions.insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-without-id".to_owned()),
                active_turn_generation: Some(1),
                thread_id: Some("thread-without-turn-id".to_owned()),
                turn_id: None,
                ..SharedCodexSessionState::default()
            },
        );
        sessions.insert(
            sibling_id.clone(),
            SharedCodexSessionState {
                active_turn_generation: Some(7),
                thread_id: Some("thread-sibling".to_owned()),
                turn_id: Some("turn-sibling".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    }
    {
        let mut thread_sessions = runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned");
        thread_sessions.insert("thread-without-turn-id".to_owned(), session_id.clone());
        thread_sessions.insert("thread-sibling".to_owned(), sibling_id.clone());
    }

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-without-id",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    match recv_within_guard(
        &input_rx,
        "runtime retirement should request graceful shutdown",
    )
    .expect("runtime retirement should request graceful shutdown")
    {
        CodexRuntimeCommand::JsonRpcNotification { method } => {
            assert_eq!(method, "shutdown");
        }
        _ => panic!("missing turn id must retire without an inexact turn interrupt"),
    }
    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let failed = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("affected Codex session should remain");
    assert_eq!(failed.session.status, SessionStatus::Error);
    assert!(failed.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. }
            if text.contains("accepted shared Codex turn has no turn id")
                && text.contains("retiring the shared runtime"))
    }));
    assert!(matches!(failed.runtime, SessionRuntime::None));
    let sibling = inner
        .sessions
        .iter()
        .find(|record| record.session.id == sibling_id)
        .expect("sibling Codex session should remain");
    assert_eq!(sibling.session.status, SessionStatus::Error);
    assert_eq!(sibling.active_turn_generation, 7);
    assert!(matches!(sibling.runtime, SessionRuntime::None));
    drop(inner);
    phase_sync::process_exit(&process, "shared Codex runtime without a turn id reaped");
}

#[test]
fn shared_codex_turn_started_watchdog_extends_while_stdout_is_active() {
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let mut probes = 0usize;
    let timed_out = wait_for_shared_codex_turn_started_or_timeout(
        &cancel_rx,
        SharedCodexTurnStartedWatchdogConfig {
            silence_limit: Duration::from_millis(1),
            max_wait_while_active: Duration::from_secs(30),
            poll_slice: Duration::from_millis(1),
        },
        || {
            probes += 1;
            if probes == 2 {
                cancel_tx
                    .send(())
                    .expect("test cancellation should reach the watchdog");
            }
            Some(Duration::ZERO)
        },
    );

    assert!(!timed_out, "recent stdout must extend the silence budget");
    assert!(
        probes >= 2,
        "the watchdog should survive its first silence-budget poll"
    );
}

#[test]
fn shared_codex_turn_started_watchdog_is_armed_by_the_real_turn_start_waiter() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-wired-turn-started-watchdog");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 9;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-wired-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-wired-watchdog".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-wired-watchdog",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_millis(25),
        )),
        CodexPromptCommand {
            active_turn_generation: 9,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "arm the real watchdog".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) = take_pending_codex_request(&pending_requests);
    let interrupt = std::thread::spawn(move || {
        let command = recv_within_guard(
            &input_rx,
            "wired watchdog should interrupt the accepted turn",
        )
        .expect("wired watchdog should interrupt the accepted turn");
        match command {
            CodexRuntimeCommand::InterruptTurn { response_tx, .. } => response_tx
                .send(Ok(()))
                .expect("watchdog interrupt acknowledgement should send"),
            _ => panic!("wired watchdog should issue an exact turn interrupt"),
        }
    });
    response_tx
        .send(Ok(json!({ "turn": { "id": "turn-wired-watchdog" } })))
        .expect("turn/start response should send");

    let deadline = phase_sync::PollGuard::new();
    loop {
        let terminalized = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Codex session should remain");
            record.session.status == SessionStatus::Error
                && matches!(record.runtime, SessionRuntime::None)
        };
        if terminalized {
            break;
        }
        deadline.wait(format_args!("wired watchdog should expire"));
    }
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn shared_codex_turn_started_event_cancels_the_real_waiter_watchdog() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-cancelled-turn-started-watchdog");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 10;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-cancelled-watchdog".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-cancelled-watchdog".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-cancelled-watchdog",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 10,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "cancel the real watchdog".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) = take_pending_codex_request(&pending_requests);
    response_tx
        .send(Ok(json!({ "turn": { "id": "turn-cancelled-watchdog" } })))
        .expect("turn/start response should send");
    let deadline = phase_sync::PollGuard::new();
    loop {
        let armed = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .get(&session_id)
            .is_some_and(|session| session.turn_started_watchdog_cancel_tx.is_some());
        if armed {
            break;
        }
        deadline.wait(format_args!("watchdog should arm after response"));
    }
    handle_shared_codex_app_server_message(
        &json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-cancelled-watchdog",
                "turn": { "id": "turn-cancelled-watchdog" }
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("turn/started should cancel the watchdog");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn idless_turn_started_before_response_reconciles_without_arming_watchdog() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-idless-turn-started-before-response");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 11;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-idless-before-response".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-idless-before-response".to_owned(),
            session_id.clone(),
        );

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-idless-before-response",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 11,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "accept an id-less early start".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (request_id, response_tx) = take_pending_codex_request(&pending_requests);

    handle_shared_codex_app_server_message(
        &json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-idless-before-response",
                "turn": {}
            }
        }),
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .expect("id-less turn/started should be retained until the response arrives");
    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session = sessions
            .get(&session_id)
            .expect("shared Codex session should remain");
        assert_eq!(
            session.pending_turn_start_request_id.as_deref(),
            Some(request_id.as_str())
        );
        assert!(session.turn_started_before_response);
        assert!(session.turn_started);
        assert!(session.turn_id.is_none());
        assert!(session.turn_started_watchdog_cancel_tx.is_none());
    }

    response_tx
        .send(Ok(json!({
            "turn": { "id": "turn-idless-before-response" }
        })))
        .expect("turn/start response should send");
    let deadline = phase_sync::PollGuard::new();
    loop {
        let reconciled = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            sessions.get(&session_id).is_some_and(|session| {
                session.pending_turn_start_request_id.is_none()
                    && !session.turn_started_before_response
                    && session.turn_started
                    && session.turn_id.as_deref() == Some("turn-idless-before-response")
                    && session.turn_started_watchdog_cancel_tx.is_none()
            })
        };
        if reconciled {
            break;
        }
        deadline.wait(format_args!(
            "response waiter should reconcile the early notification"
        ));
    }
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Active);
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn idless_turn_completed_before_response_finishes_the_armed_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-idless-completed-before-response");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 12;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-idless-completed-before-response".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-idless-completed-before-response".to_owned(),
            session_id.clone(),
        );

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        "thread-idless-completed-before-response",
        Some(SharedCodexTurnStartedWatchdogConfig::fixed(
            Duration::from_secs(30),
        )),
        CodexPromptCommand {
            active_turn_generation: 12,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "finish before the response waiter runs".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .expect("turn/start should be written");
    let (_request_id, response_tx) = take_pending_codex_request(&pending_requests);

    for message in [
        json!({
            "method": "turn/started",
            "params": {
                "threadId": "thread-idless-completed-before-response",
                "turn": {}
            }
        }),
        json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-idless-completed-before-response",
                "turn": {}
            }
        }),
    ] {
        handle_shared_codex_app_server_message(
            &message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
        )
        .expect("the early id-less lifecycle event should be accepted");
    }
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("Codex session should remain");
        assert_eq!(record.session.status, SessionStatus::Idle);
    }
    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session = sessions
            .get(&session_id)
            .expect("shared Codex session should remain registered");
        assert!(session.pending_turn_start_request_id.is_none());
        assert!(!session.turn_started_before_response);
        assert!(!session.turn_started);
        assert!(session.turn_started_watchdog_cancel_tx.is_none());
    }

    response_tx
        .send(Ok(json!({
            "turn": { "id": "turn-too-late-for-completed" }
        })))
        .expect("the late response should still release its waiter");
    std::thread::sleep(Duration::from_millis(25));
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain");
    assert_eq!(record.session.status, SessionStatus::Idle);
    drop(inner);
    let _ = process.kill();
    let _ = process.wait();
}

// If the accepted turn cannot be interrupted, reporting Error is not enough:
// the shared process may still be executing tools. The watchdog must retire
// that exact runtime before it terminalizes the durable turn.
#[test]
fn shared_codex_turn_started_watchdog_retires_runtime_when_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-interrupt-failure");

    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "LIVE TURN".to_owned();
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some(
                    "turn-start-watchdog-interrupt-failure".to_owned(),
                ),
                active_turn_generation: Some(0),
                thread_id: Some("thread-watchdog-interrupt-failure".to_owned()),
                turn_id: Some("turn-watchdog-interrupt-failure".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-watchdog-interrupt-failure".to_owned(),
            session_id.clone(),
        );

    let interrupt = std::thread::spawn(move || {
        let command = recv_within_guard(
            &input_rx,
            "watchdog should attempt to interrupt the accepted turn",
        )
        .expect("watchdog should attempt to interrupt the accepted turn");
        match command {
            CodexRuntimeCommand::InterruptTurn { response_tx, .. } => {
                response_tx
                    .send(Err("scripted interrupt refusal".to_owned()))
                    .expect("watchdog interrupt refusal should be received");
            }
            _ => panic!("watchdog should issue an exact turn interrupt"),
        }
    });

    assert!(handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "turn-start-watchdog-interrupt-failure",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    interrupt
        .join()
        .expect("watchdog interrupt responder should finish");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none(),
        "a failed interrupt must retire the shared runtime slot"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text.contains("interrupt failed"))
    }));
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

// The request id is the watchdog generation. Once turn/started cleared it (or
// a newer request replaced it), the old timer must not touch the live runtime.
#[test]
fn shared_codex_turn_started_watchdog_ignores_a_completed_or_superseded_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-stale");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("newer-turn-start".to_owned()),
                active_turn_generation: Some(0),
                thread_id: Some("thread-current".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );

    assert!(!handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "old-turn-start",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

// A shared app-server runtime id identifies the process, not a particular
// logical turn. A watchdog that armed for generation N must not terminalize a
// successor turn N+1 merely because both use the same shared process.
#[test]
fn shared_codex_turn_started_watchdog_ignores_a_successor_on_the_same_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-started-watchdog-successor");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "SUCCESSOR TURN".to_owned();
        inner.sessions[index].active_turn_generation = 2;
    }
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("old-turn-start".to_owned()),
                active_turn_generation: Some(1),
                thread_id: Some("thread-old".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );

    assert!(!handle_shared_codex_turn_started_watchdog_expiry(
        &state,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.runtime_id,
        &session_id,
        "old-turn-start",
        SHARED_CODEX_TURN_STARTED_EVENT_TIMEOUT,
    ));
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "SUCCESSOR TURN");
    assert_eq!(record.active_turn_generation, 2);
    assert!(
        record
            .runtime
            .matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone()))
    );
    drop(inner);

    let _ = process.kill();
    let _ = process.wait();
}

#[test]
fn watchdog_routing_cleanup_preserves_a_successor_turn_generation() {
    let sessions = SharedCodexSessions::new();
    sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            "session-successor".to_owned(),
            SharedCodexSessionState {
                thread_id: Some("thread-shared".to_owned()),
                active_turn_generation: Some(12),
                turn_id: Some("turn-successor".to_owned()),
                turn_started: true,
                ..SharedCodexSessionState::default()
            },
        );
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::from([(
        "thread-shared".to_owned(),
        "session-successor".to_owned(),
    )])));

    forget_shared_codex_thread_if_generation_matches(
        &sessions,
        &thread_sessions,
        "session-successor",
        "thread-shared",
        11,
    );

    let sessions = sessions.lock().expect("shared Codex mutex poisoned");
    let successor = sessions
        .get("session-successor")
        .expect("successor attachment should remain");
    assert_eq!(successor.active_turn_generation, Some(12));
    assert_eq!(successor.thread_id.as_deref(), Some("thread-shared"));
    assert_eq!(successor.turn_id.as_deref(), Some("turn-successor"));
    drop(sessions);
    assert_eq!(
        thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .get("thread-shared")
            .map(String::as_str),
        Some("session-successor")
    );
}

// Pins that a startup timeout against a SILENT app-server is treated as a
// shared-transport failure, not as one bad turn. If the server said nothing for
// the entire wait window it is wedge-shaped, and keeping the shared runtime
// attached would route every later Codex session into the same stuck process —
// nothing else would ever retire it (reader EOF and the `wait()` thread only
// fire when the process actually dies; the stdin watchdog only covers blocked
// writes). The busy-server counterpart is
// `shared_codex_startup_timeout_on_active_server_fails_only_that_turn`.
#[test]
fn shared_codex_startup_timeout_on_silent_server_tears_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let idle_session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-startup-timeout");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();

        let idle_index = inner
            .find_session_index(&idle_session_id)
            .expect("idle Codex session should exist");
        inner.sessions[idle_index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: idle_session_id.clone(),
            }),
        });
        inner.sessions[idle_index].session.status = SessionStatus::Idle;
        inner.sessions[idle_index].session.preview = "Idle Codex tab".to_owned();
    }

    // `test_shared_codex_runtime` stamps stdout activity at construction, so a
    // zero-length wait window means the server cannot have spoken DURING the
    // wait — the silence condition — without back-dating an `Instant` (which
    // would underflow on a freshly booted machine).
    handle_shared_codex_startup_response_error(
        &state,
        &runtime.runtime_id,
        &session_id,
        0,
        Duration::ZERO,
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `turn/start`".to_owned(),
        ),
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session
            .preview
            .contains("failed to communicate with shared Codex app-server")
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: failed to communicate with shared Codex app-server")
    ));

    let idle_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == idle_session_id)
        .expect("idle session should remain present");
    assert_eq!(idle_session.status, SessionStatus::Idle);
    assert_eq!(idle_session.preview, "Idle Codex tab");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    let idle_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == idle_session_id)
        .expect("idle Codex session should exist");
    assert!(matches!(idle_record.runtime, SessionRuntime::None));

    drop(inner);
    drop(runtime_token);
}

/// Builds a pending request entry the liveness-scaled waiter can be pointed
/// at, plus the sender a test uses to play the app-server's side.
fn test_pending_codex_response(
    request_id: &str,
) -> (
    CodexPendingRequestMap,
    PendingCodexJsonRpcRequest,
    mpsc::Sender<std::result::Result<Value, CodexResponseError>>,
) {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert(request_id.to_owned(), tx.clone());
    (
        pending_requests,
        PendingCodexJsonRpcRequest {
            request_id: request_id.to_owned(),
            response_rx: rx,
        },
        tx,
    )
}

// Pins the extension rule of the liveness-scaled waiter: a response that
// lands AFTER the silence budget still completes as long as the app-server
// kept emitting stdout — under the old flat wait this exact timing failed the
// turn. This reproduces the large-resume-behind-a-larger-sibling incident in
// miniature.
#[test]
fn shared_codex_patient_wait_outlasts_silence_budget_while_server_is_active() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-patient-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, tx) = test_pending_codex_response("patient-wait");

    // The app-server side: stdout stays chatty the whole time, and the
    // response arrives well after the 100ms silence budget.
    let stamper_activity = runtime.stdout_activity.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(10));
            *stamper_activity
                .lock()
                .expect("shared Codex stdout activity mutex poisoned") = std::time::Instant::now();
        }
        let _ = tx.send(Ok(json!({"thread": {"id": "thread-patient"}})));
    });

    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(100),
        Duration::from_secs(5),
        Duration::from_millis(20),
    );
    server.join().expect("server thread should finish");

    assert_eq!(
        result.expect("late response from an active server should succeed"),
        json!({"thread": {"id": "thread-patient"}})
    );
}

// Pins that a server silent past the budget still fails on the old schedule:
// the waiter must NOT extend for a server that stopped talking, or a wedged
// process would hold sessions on "working" until the hard cap. The stamp is
// only ever the construction-time one here, so silence and elapsed cross the
// budget together.
#[test]
fn shared_codex_patient_wait_gives_up_on_silent_server_at_silence_budget() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-silent-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, _tx) = test_pending_codex_response("silent-wait");

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(80),
        Duration::from_secs(30),
        Duration::from_millis(20),
    );
    let took = started.elapsed();

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `thread/resume`")
    ));
    // Far below the 30s active-cap: the silent budget governed the give-up.
    assert!(took < Duration::from_secs(5), "took {took:?}");
    // The abandoned entry must not linger — a late response has nobody to
    // reach and unknown ids are dropped by the reader.
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Pins the hard cap: stdout activity alone cannot extend a wait forever — a
// lost request against a chatty server fails once `max_wait_while_active`
// elapses (scoped downstream, since the server is demonstrably alive).
#[test]
fn shared_codex_patient_wait_hits_hard_cap_despite_active_server() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) = test_shared_codex_runtime("shared-codex-capped-wait");
    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }
    let (pending_requests, pending, _tx) = test_pending_codex_response("capped-wait");

    let stamper_activity = runtime.stdout_activity.clone();
    let stamper_done = Arc::new(Mutex::new(false));
    let stamper_stop = stamper_done.clone();
    let stamper = std::thread::spawn(move || {
        while !*stamper_stop.lock().expect("stamper stop mutex poisoned") {
            std::thread::sleep(Duration::from_millis(10));
            *stamper_activity
                .lock()
                .expect("shared Codex stdout activity mutex poisoned") = std::time::Instant::now();
        }
    });

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "turn/start",
        &state,
        &runtime.runtime_id,
        Duration::from_secs(30),
        Duration::from_millis(200),
        Duration::from_millis(20),
    );
    let took = started.elapsed();
    *stamper_done.lock().expect("stamper stop mutex poisoned") = true;
    stamper.join().expect("stamper thread should finish");

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `turn/start`")
    ));
    // The cap is a floor on the give-up (an active server is never failed
    // early) and the assertion ceiling is generous for scheduler noise.
    assert!(took >= Duration::from_millis(200), "took {took:?}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

// Pins the degraded path: when the shared slot no longer holds this runtime,
// the probe reports no liveness and the wait falls back to the silent budget
// instead of extending on a runtime that is already gone.
#[test]
fn shared_codex_patient_wait_without_registered_runtime_uses_silence_budget() {
    let state = test_app_state();
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("shared-codex-unregistered-wait");
    // Deliberately NOT placed into `state.shared_codex_runtime`.
    let (pending_requests, pending, _tx) = test_pending_codex_response("unregistered-wait");

    let started = std::time::Instant::now();
    let result = wait_for_shared_codex_response_while_server_active(
        &pending_requests,
        pending,
        "thread/resume",
        &state,
        &runtime.runtime_id,
        Duration::from_millis(80),
        Duration::from_secs(30),
        Duration::from_millis(20),
    );
    let took = started.elapsed();

    assert!(matches!(result, Err(CodexResponseError::Timeout(_))));
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

// Pins the busy-server side of the timeout split: when the app-server emitted
// stdout during the wait window (here: the activity stamp is fresh and the
// window is 120s), a response timeout fails ONLY the requesting session's
// turn. The shared runtime survives and a sibling session's in-flight turn is
// untouched. The incident this pins against: one slow `thread/resume` (a
// ~39MB rollout, contending with a ~170MB thread mid-turn) killed every
// session on the shared server, and the replacement server then had to
// re-parse the same rollouts — re-creating the very contention that caused
// the timeout.
#[test]
fn shared_codex_startup_timeout_on_active_server_fails_only_that_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let busy_session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-busy-timeout");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();

        let busy_index = inner
            .find_session_index(&busy_session_id)
            .expect("busy Codex session should exist");
        inner.sessions[busy_index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: busy_session_id.clone(),
            }),
        });
        inner.sessions[busy_index].session.status = SessionStatus::Active;
        inner.sessions[busy_index].session.preview = "Streaming a sibling turn".to_owned();
    }

    handle_shared_codex_startup_response_error(
        &state,
        &runtime.runtime_id,
        &session_id,
        0,
        Duration::from_secs(120),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `turn/start`".to_owned(),
        ),
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains(
                "Turn failed: timed out waiting for Codex app-server response to `turn/start`"
            ) && text.contains("only this turn was abandoned")
    ));

    let busy_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == busy_session_id)
        .expect("busy session should remain present");
    assert_eq!(busy_session.status, SessionStatus::Active);
    assert_eq!(busy_session.preview, "Streaming a sibling turn");

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    // Scoped failure must not detach anyone from the surviving runtime.
    let inner = state.inner.lock().expect("state mutex poisoned");
    for id in [&session_id, &busy_session_id] {
        let record = inner
            .sessions
            .iter()
            .find(|record| &record.session.id == id)
            .expect("session record should exist");
        assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    }
    drop(inner);
}

// Pins that an old thread-setup waiter cannot retire the shared app-server
// after its session has already stopped or rebound away from that runtime.
#[test]
fn shared_codex_stale_thread_setup_timeout_does_not_clear_shared_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, _process) =
        test_shared_codex_runtime("shared-codex-stale-thread-setup-timeout");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    handle_shared_codex_thread_setup_response_error_if_current(
        &runtime.sessions,
        &state,
        &runtime.runtime_id,
        &session_id,
        "old-thread-setup-request",
        Duration::from_secs(180),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `thread/start`".to_owned(),
        ),
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.messages.is_empty());
}

// Pins that a stale thread-setup waiter cannot fail a newer setup attempt
// on the same shared app-server runtime after the session has restarted.
#[test]
fn shared_codex_stale_thread_setup_timeout_ignores_newer_same_runtime_request() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-stale-thread-setup-same-runtime");

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "New thread setup is active".to_owned();
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_thread_setup: Some(test_pending_codex_thread_setup(
                    "new-thread-setup-request",
                )),
                ..SharedCodexSessionState::default()
            },
        );

    handle_shared_codex_thread_setup_response_error_if_current(
        &runtime.sessions,
        &state,
        &runtime.runtime_id,
        &session_id,
        "old-thread-setup-request",
        Duration::from_secs(180),
        CodexResponseError::Timeout(
            "timed out waiting for Codex app-server response to `thread/start`".to_owned(),
        ),
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .as_ref()
            .is_some_and(|shared_runtime| shared_runtime.runtime_id == runtime.runtime_id)
    );

    let pending_request_id = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .get(&session_id)
        .and_then(|session| {
            session
                .pending_thread_setup
                .as_ref()
                .map(|setup| setup.request_id.clone())
        });
    assert_eq!(
        pending_request_id.as_deref(),
        Some("new-thread-setup-request")
    );

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "New thread setup is active");
    assert!(session.messages.is_empty());
}

// Pins the runtime-exit queued-dispatch ordering: for a dying shared Codex
// runtime, the AppState shared-runtime slot is cleared before the queued turn
// dispatcher attempts recovery work.
#[test]
fn shared_codex_runtime_exit_clears_shared_slot_before_queued_dispatch_attempt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-runtime-exit-clear-before-dispatch");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        let queued_prompt_id = inner.next_message_id();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Waiting for Codex".to_owned();
        record.remote_id = Some("remote-proxy-to-block-dispatch-spawn".to_owned());
        record.remote_session_id = Some("remote-session".to_owned());
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
                id: queued_prompt_id,
                timestamp: stamp_now(),
                text: "queued recovery prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
    }

    let error = state
        .handle_runtime_exit_if_matches(
            &session_id,
            &runtime_token,
            Some("shared app-server exited"),
        )
        .expect_err("remote proxy queued dispatch should fail after slot clear");
    assert!(
        format!("{error:#}").contains("remote proxy sessions must dispatch"),
        "unexpected runtime-exit error: {error:#}"
    );
    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none(),
        "shared runtime slot should be cleared before queued dispatch can fail"
    );
}

// Pins that an item/agentMessage/delta arriving while turn_started=false
// is held (no transcript write) and then takes effect once turn/started
// flips the flag for the matching turn_id.
// Guards against streamed text appearing before Codex has acknowledged the
// turn, which would ride ahead of turn-setup notices.
#[test]
fn shared_codex_app_server_agent_message_delta_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-agent-delta-turn-started");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "msg-1",
            "delta": "Hello"
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Hello");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
}

// Pins that a server-initiated request (item/tool/requestUserInput) is
// rejected while turn_started=false, and only records a
// Message::UserInputRequest once turn/started has fired for the matching turn_id.
// Guards against user-input prompts rendering before the turn is actually
// live, while still answering Codex's JSON-RPC request instead of dropping it.
#[test]
fn shared_codex_app_server_request_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-request-turn-started");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let request = json!({
        "id": "req-1",
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "conversation-123",
            "turnId": "turn-current",
            "questions": [
                {
                    "header": "Scope",
                    "id": "scope",
                    "question": "What should Codex review?"
                }
            ]
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
    )
    .unwrap();

    match recv_within_guard(
        &input_rx,
        "shared codex app server request waits for turn started: runtime command 1",
    )
    .unwrap()
    {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "req-1",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest { title, detail, state, .. })
            if title == "Codex needs input"
                && detail == "Codex requested additional input for \"Scope\"."
                && *state == InteractionRequestState::Pending
    ));
}

fn set_active_codex_approval_policy(
    state: &AppState,
    session_id: &str,
    active_policy: CodexApprovalPolicy,
    configured_policy: CodexApprovalPolicy,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("Codex session should exist");
    inner.sessions[index].active_codex_approval_policy = Some(active_policy);
    inner.sessions[index].codex_approval_policy = configured_policy;
    inner.sessions[index].session.approval_policy = Some(configured_policy);
}

fn install_read_only_codex_delegation(state: &AppState, child_session_id: &str) {
    let parent_session_id = test_session_id(state, Agent::Codex);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_id = inner.next_delegation_id();
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("Codex child session should exist");
    inner.sessions[child_index].session.parent_delegation_id = Some(delegation_id.clone());
    inner.delegations.push(DelegationRecord {
        id: delegation_id,
        parent_session_id,
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Read-only Codex reviewer".to_owned(),
        prompt: "Review without writing.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 1,
        acceptance_evaluation: None,
    });
}

fn codex_auto_approval_requests() -> Vec<(&'static str, Value, Value, Value)> {
    vec![
        (
            "item/commandExecution/requestApproval",
            json!({
                "id": "command-approval",
                "params": { "command": "cargo test", "cwd": "/tmp" }
            }),
            json!("command-approval"),
            json!({ "decision": "accept" }),
        ),
        (
            "item/fileChange/requestApproval",
            json!({
                "id": "file-approval",
                "params": { "reason": "Apply the requested fix" }
            }),
            json!("file-approval"),
            json!({ "decision": "accept" }),
        ),
        (
            "item/permissions/requestApproval",
            json!({
                "id": "permissions-approval",
                "params": {
                    "permissions": {
                        "network": { "enabled": true }
                    }
                }
            }),
            json!("permissions-approval"),
            json!({
                "permissions": {
                    "network": { "enabled": true }
                },
                "scope": "turn"
            }),
        ),
    ]
}

#[test]
fn codex_auto_approve_maps_to_native_on_request() {
    assert_eq!(
        CodexApprovalPolicy::AutoApprove.as_cli_value(),
        "on-request"
    );
    assert_eq!(
        serde_json::to_value(CodexApprovalPolicy::AutoApprove).unwrap(),
        json!("auto-approve")
    );
    assert_eq!(
        codex_approval_policy_from_json_value(&json!("auto-approve")),
        Some(CodexApprovalPolicy::AutoApprove)
    );
}

#[test]
fn codex_auto_approve_accepts_all_approval_kinds_without_pending_cards() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    // The active turn owns the decision even if the mutable next-turn setting
    // has already changed.
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::AutoApprove,
        CodexApprovalPolicy::OnRequest,
    );
    let (input_tx, input_rx) = mpsc::channel();

    for (method, request, expected_request_id, expected_result) in codex_auto_approval_requests() {
        assert!(
            try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("AutoApprove request should be classified")
        );
        assert!(matches!(
            recv_within_guard(&input_rx, "codex auto approve accepts all approval kinds without pending cards: runtime command 1").unwrap(),
            CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id,
                    payload: CodexJsonRpcResponsePayload::Result(result),
                }
            } if request_id == expected_request_id && result == expected_result
        ));
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("Codex session should exist")];
    assert!(record.session.messages.is_empty());
    assert!(record.pending_codex_approvals.is_empty());
}

#[test]
fn codex_auto_approve_declines_all_approval_kinds_for_read_only_delegations() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    install_read_only_codex_delegation(&state, &session_id);
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::AutoApprove,
        CodexApprovalPolicy::AutoApprove,
    );
    let (input_tx, input_rx) = mpsc::channel();

    for (method, request, expected_request_id, accepted_result) in codex_auto_approval_requests() {
        assert!(
            try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("read-only AutoApprove request should be classified")
        );
        let expected_result = if method == "item/permissions/requestApproval" {
            json!({ "permissions": {}, "scope": "turn" })
        } else {
            json!({ "decision": "decline" })
        };
        assert_ne!(expected_result, accepted_result);
        assert!(matches!(
            recv_within_guard(&input_rx, "codex auto approve declines all approval kinds for read only delegations: runtime command 1").unwrap(),
            CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id,
                    payload: CodexJsonRpcResponsePayload::Result(result),
                }
            } if request_id == expected_request_id && result == expected_result
        ));
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner
        .find_session_index(&session_id)
        .expect("Codex session should exist")];
    assert!(record.session.messages.is_empty());
    assert!(record.pending_codex_approvals.is_empty());
}

#[test]
fn codex_auto_approve_leaves_non_approval_requests_and_next_turn_settings_alone() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    set_active_codex_approval_policy(
        &state,
        &session_id,
        CodexApprovalPolicy::OnRequest,
        CodexApprovalPolicy::AutoApprove,
    );
    let (input_tx, input_rx) = mpsc::channel();
    let command_request = &codex_auto_approval_requests()[0];
    assert!(
        !try_auto_respond_codex_approval_request(
            command_request.0,
            &command_request.1,
            &state,
            &session_id,
            &input_tx,
        )
        .expect("current turn should remain interactive")
    );

    for (method, request) in [
        (
            "item/tool/requestUserInput",
            json!({ "id": "input", "params": { "questions": [] } }),
        ),
        (
            "mcpServer/elicitation/request",
            json!({ "id": "elicitation", "params": {} }),
        ),
        ("item/tool/call", json!({ "id": "generic", "params": {} })),
    ] {
        assert!(
            !try_auto_respond_codex_approval_request(
                method,
                &request,
                &state,
                &session_id,
                &input_tx,
            )
            .expect("non-approval request should remain interactive")
        );
    }
    assert!(input_rx.try_recv().is_err());
}

struct EngramBootstrapFixture {
    state: AppState,
    session_id: String,
    runtime: SharedCodexRuntime,
    input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    pending: CodexPendingRequestMap,
    writer: Vec<u8>,
}

impl EngramBootstrapFixture {
    fn new() -> Self {
        let state = test_app_state();
        let session_id = create_test_engram_codex_session(&state, "bootstrap");
        let (runtime, input_rx, process) = test_shared_codex_runtime("bootstrap");
        *state.shared_codex_runtime.lock().unwrap() = Some(runtime.clone());
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: runtime.runtime_id.clone(),
                input_tx: runtime.input_tx.clone(),
                process,
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: runtime.clone(),
                    session_id: session_id.clone(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
        }
        Self {
            state,
            session_id,
            runtime,
            input_rx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            writer: Vec::new(),
        }
    }

    fn prompt(&mut self, text: &str) {
        let cwd = self
            .state
            .test_temp_root
            .as_ref()
            .unwrap()
            .path()
            .join("bootstrap");
        handle_shared_codex_prompt_command(
            &mut self.writer,
            &self.pending,
            &self.state,
            &self.runtime.runtime_id,
            &test_missing_shared_codex_home(),
            &self.runtime.sessions,
            &self.runtime.thread_sessions,
            &self.runtime.input_tx,
            None,
            &self.session_id,
            CodexPromptCommand {
                active_turn_generation: 0,
                approval_policy: CodexApprovalPolicy::Never,
                attachments: vec![],
                cwd: cwd.to_string_lossy().into_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: text.to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                service_tier: None,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        )
        .unwrap();
    }

    fn messages(&self) -> Vec<Value> {
        std::str::from_utf8(&self.writer)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for EngramBootstrapFixture {
    fn drop(&mut self) {
        self.runtime
            .sessions
            .lock()
            .unwrap()
            .remove(&self.session_id);
        self.pending.lock().unwrap().clear();
    }
}

#[test]
fn engram_bootstrap_preserves_effective_instructions_and_newest_prompt() {
    let mut f = EngramBootstrapFixture::new();
    f.prompt("opener");
    let first = f.messages();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["method"], "config/read");
    let cwd = f
        .state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("bootstrap");
    assert_eq!(first[0]["params"]["cwd"], cwd.to_string_lossy().as_ref());
    f.prompt("newest");
    assert_eq!(
        f.messages().len(),
        1,
        "writer returned, newer prompt coalesced"
    );
    let base = "  user + project instructions\r\nkeep trailing whitespace  \n";
    finish_engram_config_for_test(
        &f.state,
        &f.runtime,
        &f.pending,
        &f.runtime.input_tx,
        &f.input_rx,
        &mut f.writer,
        json!({"config": {"developer_instructions": base}}),
    );
    let written = f.messages();
    assert_eq!(written.len(), 2);
    assert_eq!(written[1]["method"], "thread/start");
    assert_eq!(written[1]["params"]["cwd"], first[0]["params"]["cwd"]);
    assert_eq!(
        written[1]["params"]["developerInstructions"],
        format!("{base}\n\n{CODEX_ENGRAM_RECOVERY_BOOTSTRAP}")
    );
    answer_pending_codex_thread_setups(&f.pending, "engram-thread");
    let command =
        recv_within_guard(&f.input_rx, "resolved setup should deliver latest prompt").unwrap();
    let CodexRuntimeCommand::StartTurnAfterSetup { command, .. } = command else {
        panic!("expected prompt delivery");
    };
    assert_eq!(command.prompt, "newest");
}

#[test]
fn engram_bootstrap_config_failure_is_visible_and_allows_retry() {
    for response in [
        Err(CodexResponseError::JsonRpc("config/read denied".to_owned())),
        Err(CodexResponseError::Timeout(
            "config/read timed out".to_owned(),
        )),
        Ok(json!({"unexpected": {}})),
        Ok(json!({"config": {"developer_instructions": 42}})),
    ] {
        let mut f = EngramBootstrapFixture::new();
        f.prompt("opener");
        f.prompt("newest");
        // A short config deadline must not tear down a quiet shared server.
        *f.runtime.stdout_activity.lock().unwrap() =
            std::time::Instant::now() - Duration::from_secs(900);
        answer_engram_config_for_test(&f.pending, response);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = f.state.full_snapshot();
            let session = snapshot
                .sessions
                .iter()
                .find(|s| s.id == f.session_id)
                .unwrap();
            if session.status == SessionStatus::Error {
                assert!(session.messages.iter().any(|m| matches!(m,
                    Message::Text { text, .. } if text.contains("config/read"))));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "config failure was not surfaced"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            f.runtime
                .sessions
                .lock()
                .unwrap()
                .get(&f.session_id)
                .unwrap()
                .pending_thread_setup
                .is_none()
        );
        assert_eq!(
            f.messages().len(),
            1,
            "must not start without resolved instructions"
        );
        assert!(f.state.session_matches_runtime_token(
            &f.session_id,
            &RuntimeToken::Codex(f.runtime.runtime_id.clone())
        ));
        f.prompt("retry");
        assert_eq!(
            f.messages().len(),
            2,
            "retry must issue a fresh config/read"
        );
        assert_eq!(f.messages()[1]["method"], "config/read");
    }
}

#[test]
fn engram_bootstrap_queued_response_cannot_start_detached_or_replacement_setup() {
    let mut f = EngramBootstrapFixture::new();
    f.prompt("old");
    answer_engram_config_for_test(&f.pending, Ok(json!({"config": {}})));
    let command = recv_within_guard(&f.input_rx, "config continuation should arrive").unwrap();
    f.runtime.sessions.lock().unwrap().remove(&f.session_id);
    f.prompt("replacement");
    let replacement = f
        .runtime
        .sessions
        .lock()
        .unwrap()
        .get(&f.session_id)
        .unwrap()
        .pending_thread_setup
        .as_ref()
        .unwrap()
        .request_id
        .clone();
    run_engram_config_continuation_for_test(
        &f.state,
        &f.runtime,
        &f.pending,
        &f.runtime.input_tx,
        &mut f.writer,
        command,
    );
    assert!(shared_codex_thread_setup_is_current(
        &f.runtime.sessions,
        &f.session_id,
        &replacement
    ));
    assert!(f.messages().iter().all(|m| m["method"] == "config/read"));
}

#[test]
fn engram_bootstrap_composition_matches_override_precedence_and_null_inheritance() {
    for (params, config, base) in [
        (json!({}), json!({}), ""),
        (json!({}), json!({"developer_instructions": null}), ""),
        (
            json!({"developerInstructions": null}),
            json!({"developer_instructions": "inherited"}),
            "inherited",
        ),
        (
            json!({"developerInstructions": ""}),
            json!({"developer_instructions": "ignored"}),
            "",
        ),
        (
            json!({"config": {"developer_instructions": "override"}}),
            json!({"developer_instructions": "inherited"}),
            "override",
        ),
        (
            json!({"developerInstructions": "explicit", "config": {"developer_instructions": "override"}}),
            json!({"developer_instructions": "inherited"}),
            "explicit",
        ),
    ] {
        let mut params = params;
        compose_codex_engram_instructions(&mut params, &json!({"config": config})).unwrap();
        let expected = if base.is_empty() {
            CODEX_ENGRAM_RECOVERY_BOOTSTRAP.to_owned()
        } else {
            format!("{base}\n\n{CODEX_ENGRAM_RECOVERY_BOOTSTRAP}")
        };
        assert_eq!(params["developerInstructions"], expected);
    }
    for malformed in [Value::Null, json!({}), json!({"config": []})] {
        assert!(compose_codex_engram_instructions(&mut json!({}), &malformed).is_err());
    }
}

#[test]
fn engram_bootstrap_disabled_and_resume_do_not_read_or_override_instructions() {
    for (enabled, resume) in [(false, None), (false, Some("old")), (true, Some("old"))] {
        let request = shared_codex_setup_request_for_mcp_test(
            &test_missing_shared_codex_home(),
            resume,
            enabled,
        );
        assert_eq!(
            request["method"],
            if resume.is_some() {
                "thread/resume"
            } else {
                "thread/start"
            }
        );
        assert!(request["params"].get("developerInstructions").is_none());
    }
}

#[test]
fn engram_bootstrap_thread_start_write_failure_releases_resolved_setup() {
    let mut f = EngramBootstrapFixture::new();
    f.prompt("start");
    answer_engram_config_for_test(&f.pending, Ok(json!({"config": {}})));
    let command = recv_within_guard(&f.input_rx, "config continuation should arrive").unwrap();
    let CodexRuntimeCommand::StartThreadAfterConfig {
        session_id,
        request_id,
        params,
    } = command
    else {
        panic!("expected config continuation");
    };
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test broken writer",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let result = handle_shared_codex_start_thread_after_config(
        &mut BrokenWriter,
        &f.pending,
        &f.state,
        &f.runtime.runtime_id,
        &f.runtime.sessions,
        &f.runtime.thread_sessions,
        &f.runtime.input_tx,
        None,
        &session_id,
        request_id,
        params,
    );
    assert!(
        result.is_err(),
        "transport failure must reach the writer loop"
    );
    assert!(f.pending.lock().unwrap().is_empty());
    assert!(
        f.runtime
            .sessions
            .lock()
            .unwrap()
            .get(&f.session_id)
            .unwrap()
            .pending_thread_setup
            .is_none()
    );
}

#[test]
fn engram_bootstrap_closed_writer_channel_releases_setup() {
    let mut f = EngramBootstrapFixture::new();
    f.prompt("start");
    // Dropping the command receiver must terminate the setup, not leave it parked.
    let (_, replacement_rx) = mpsc::channel();
    drop(std::mem::replace(&mut f.input_rx, replacement_rx));
    answer_engram_config_for_test(&f.pending, Ok(json!({"config": {}})));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let sessions = f.runtime.sessions.lock().unwrap();
        let pending = sessions
            .get(&f.session_id)
            .and_then(|s| s.pending_thread_setup.as_ref());
        if pending.is_none() {
            break;
        }
        drop(sessions);
        assert!(
            std::time::Instant::now() < deadline,
            "closed writer stranded config setup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(f.pending.lock().unwrap().is_empty());
    assert_eq!(f.messages().len(), 1);
}

#[test]
fn engram_bootstrap_first_config_write_failure_releases_slot_and_allows_retry() {
    let mut f = EngramBootstrapFixture::new();
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "config write failed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let result = handle_shared_codex_prompt_command(
        &mut BrokenWriter,
        &f.pending,
        &f.state,
        &f.runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &f.runtime.sessions,
        &f.runtime.thread_sessions,
        &f.runtime.input_tx,
        None,
        &f.session_id,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::Never,
            attachments: vec![],
            cwd: f
                .state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join("bootstrap")
                .to_string_lossy()
                .into_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "first prompt".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    );
    assert!(
        result.is_err(),
        "transport error must reach the writer loop"
    );
    assert!(f.pending.lock().unwrap().is_empty());
    assert!(
        f.runtime
            .sessions
            .lock()
            .unwrap()
            .get(&f.session_id)
            .unwrap()
            .pending_thread_setup
            .is_none()
    );
    f.prompt("retry after write failure");
    assert_eq!(
        f.messages().len(),
        1,
        "retry must not park behind a failed setup"
    );
    assert_eq!(f.messages()[0]["method"], "config/read");
}

#[test]
fn engram_bootstrap_config_read_has_a_fixed_deadline_despite_sibling_activity() {
    let mut f = EngramBootstrapFixture::new();
    let started = std::time::Instant::now();
    f.prompt("deadline");
    let guard = CODEX_ENGRAM_CONFIG_READ_TIMEOUT + Duration::from_secs(10);
    loop {
        // Keep the shared server visibly active: the rollout patience budget
        // would otherwise keep this read parked for fifteen minutes.
        *f.runtime.stdout_activity.lock().unwrap() = std::time::Instant::now();
        let snapshot = f.state.full_snapshot();
        let session = snapshot
            .sessions
            .iter()
            .find(|s| s.id == f.session_id)
            .unwrap();
        if session.status == SessionStatus::Error {
            assert!(session.messages.iter().any(|m| matches!(m,
                Message::Text { text, .. } if text.contains("config/read") && text.contains("timed out"))));
            break;
        }
        assert!(
            started.elapsed() < guard,
            "config read borrowed the rollout replay deadline"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(f.pending.lock().unwrap().is_empty());
    assert!(f.state.session_matches_runtime_token(
        &f.session_id,
        &RuntimeToken::Codex(f.runtime.runtime_id.clone())
    ));
    assert_eq!(f.messages().len(), 1, "timeout must not start a thread");
}
