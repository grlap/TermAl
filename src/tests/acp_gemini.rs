// ACP (Agent Client Protocol) is the JSON-RPC dialect spoken by Gemini CLI,
// Cursor, and OpenCode; TermAl implements the client side and drives
// each agent through `initialize`, `session/new`, optional `session/resume` or
// `session/load`, and prompt turns. Resume is used only when explicitly
// advertised under `sessionCapabilities.resume`; `session/load` likewise
// requires explicit support. Continuation errors never replace the saved id.
// Antigravity CLI is launched through this adapter for Gemini sessions.
// Its own cached credentials and settings remain operator-owned.

use super::*;

fn assert_emitted_acp_delegation_mcp_descriptor(
    written: &str,
    method: &str,
    parent_session_id: &str,
) {
    let request: Value = written
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|request: &Value| request["method"].as_str() == Some(method))
        .unwrap_or_else(|| panic!("{method} request should be emitted as JSON\n{written}"));
    let servers = request
        .pointer("/params/mcpServers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{method} should emit mcpServers as an array\n{written}"));
    assert_eq!(
        servers.len(),
        1,
        "{method} should emit exactly the parent-scoped TermAl MCP bridge"
    );
    let server = &servers[0];
    assert_eq!(
        server.get("name").and_then(Value::as_str),
        Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
    );
    assert!(
        server
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        "{method} should emit a non-empty delegation MCP command"
    );
    let args = server
        .get("args")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{method} should emit delegation MCP args"));
    assert!(
        args.iter()
            .any(|arg| arg.as_str() == Some("delegation-mcp")),
        "{method} should select delegation-mcp mode"
    );
    assert!(
        args.iter()
            .any(|arg| arg.as_str() == Some(parent_session_id)),
        "{method} should bind the parent session id"
    );
    assert_eq!(
        server.get("env"),
        Some(&json!([])),
        "{method} ACP McpServer.env must use the spec EnvVariable array shape"
    );
}

#[test]
fn acp_config_option_display_text_is_bounded_before_persistence() {
    let oversized_label = "L".repeat(MAX_ACP_OPTION_LABEL_CHARS + 1);
    let oversized_description = "D".repeat(MAX_ACP_OPTION_DESCRIPTION_CHARS + 1);
    let options = acp_session_config_options(
        &json!({
            "configOptions": [{
                "id": "model",
                "options": [{
                    "value": "openai/gpt-5.6-sol",
                    "name": oversized_label,
                    "description": oversized_description,
                }]
            }]
        }),
        "model",
        Some(MAX_OPENCODE_MODEL_CHARS),
    );

    assert_eq!(options.len(), 1);
    assert_eq!(
        options[0].label, "openai/gpt-5.6-sol",
        "invalid agent labels should fall back to the already-bounded value"
    );
    assert_eq!(
        options[0].description, None,
        "invalid agent descriptions must not enter persistence or SSE state"
    );
}

// Pins `acp_supports_session_load` reading the modern `agentCapabilities.loadSession`
// boolean from an `initialize` response. Guards against drift in the JSON pointer
// path or boolean polarity, which would silently break resume support detection.
#[test]
fn acp_supports_session_load_reads_agent_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": {
                "loadSession": true,
            }
        })),
        Some(true)
    );
}

// Capability authority comes only from the ACP v1 envelope and shapes.
#[test]
fn acp_capabilities_reject_legacy_envelopes_and_malformed_flags() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "capabilities": { "loadSession": true }
        })),
        None
    );
    assert_eq!(
        acp_supports_session_load(&json!({
            "agentCapabilities": { "loadSession": "true" }
        })),
        None
    );
    assert_eq!(acp_supports_session_load(&json!({})), None);
    assert_eq!(
        acp_supports_session_resume(&json!({
            "agentCapabilities": { "sessionCapabilities": { "resume": {} } }
        })),
        Some(true)
    );
    for value in [
        json!(true),
        json!(false),
        json!(null),
        json!([]),
        json!("resume"),
    ] {
        assert_eq!(
            acp_supports_session_resume(&json!({
                "agentCapabilities": { "sessionCapabilities": { "resume": value } }
            })),
            Some(false)
        );
    }
    assert_eq!(
        acp_supports_session_resume(&json!({
            "capabilities": { "sessionCapabilities": { "resume": {} } }
        })),
        None
    );
    assert_eq!(acp_supports_session_resume(&json!({})), None);
}

// Pins `AcpRuntimeState::default().capabilities == None`. Guards against a
// default with pre-filled capabilities, which would bias resume behavior
// before `initialize` has actually been processed.
#[test]
fn acp_runtime_state_defaults_session_load_support_to_unknown() {
    let default_state = AcpRuntimeState::default();
    assert!(
        default_state.capabilities.is_none(),
        "default capabilities must not authorize a session/load probe"
    );
}

// Explicit loadSession support resumes the saved conversation and suppresses replay.
#[test]
fn acp_session_resume_loads_when_explicitly_advertised() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            gemini_inherited_effort: None,
            opencode_approval_mode: None,
            kimi_approval_mode: None,
            kimi_mode: None,
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    update_acp_runtime_capabilities(
        &runtime_state,
        &json!({
            "agentCapabilities": { "loadSession": true }
        }),
    );
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: thread_state.default_workdir.clone(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                opencode_effort: None,
                opencode_mode: None,
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) = take_pending_acp_request(&pending_requests);
    load_sender
        .send(Ok(json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [
                        {
                            "value": "auto",
                            "name": "Auto"
                        }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        {
                            "value": "ask",
                            "name": "Ask"
                        }
                    ]
                }
            ]
        })))
        .expect("session/load response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should reuse the persisted session");
    assert_eq!(external_session_id, "cursor-session-1");

    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/load\""),
        "session/load request should be written\n{written}"
    );
    assert_emitted_acp_delegation_mcp_descriptor(&written, "session/load", &created.session_id);
    assert!(
        !written.contains("\"method\":\"session/new\""),
        "session/new should not be written when resuming with advertised load support\n{written}"
    );

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-1")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-1")
    );
    assert_eq!(
        runtime_state
            .capabilities
            .as_ref()
            .and_then(|caps| caps.supports_session_load),
        Some(true)
    );
}

// A missing capability must not replace an existing conversation.
#[test]
fn acp_session_resume_without_advertised_capability_preserves_saved_id() {
    for agent in [Agent::Cursor, Agent::Gemini, Agent::OpenCode, Agent::Kimi] {
        for capabilities in [
            None,
            Some(AcpCapabilities::default()),
            Some(AcpCapabilities {
                supports_session_load: Some(false),
                supports_session_resume: Some(false),
            }),
        ] {
            let state = test_app_state();
            let session_id = test_session_id(&state, agent);
            state
                .set_external_session_id(&session_id, "saved-session".to_owned())
                .unwrap();
            let pending = Arc::new(Mutex::new(HashMap::new()));
            let runtime = Arc::new(Mutex::new(AcpRuntimeState {
                capabilities,
                ..AcpRuntimeState::default()
            }));
            let mut writer = SharedBufferWriter::default();
            let error = ensure_acp_session_ready(
                &mut writer,
                &pending,
                &state,
                &session_id,
                &runtime,
                agent.acp_runtime().unwrap(),
                &AcpPromptCommand {
                    cwd: state.default_workdir.clone(),
                    cursor_mode: None,
                    model: "auto".to_owned(),
                    opencode_effort: None,
                    opencode_mode: None,
                    prompt: "Continue".to_owned(),
                    resume_session_id: Some("saved-session".to_owned()),
                },
            )
            .expect_err("missing capability cannot start or replace a conversation");
            assert!(error.to_string().contains("did not advertise"), "{error:#}");
            assert!(writer.contents().is_empty());
            assert!(pending.lock().unwrap().is_empty());
            let inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            assert_eq!(
                inner.sessions[index].external_session_id.as_deref(),
                Some("saved-session")
            );
            assert!(runtime.lock().unwrap().current_session_id.is_none());
        }
    }
}

// Pins the preferred restart path: an explicitly advertised resume capability
// sends `session/resume` with cwd + MCP descriptors and never replays through
// `session/load` or creates a replacement conversation.
#[test]
fn acp_session_resume_prefers_resume_when_explicitly_supported() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            gemini_inherited_effort: None,
            opencode_approval_mode: None,
            kimi_approval_mode: None,
            kimi_mode: None,
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(true),
            supports_session_resume: Some(true),
        }),
        ..AcpRuntimeState::default()
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        ensure_acp_session_ready(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::Cursor,
            &AcpPromptCommand {
                cwd: thread_state.default_workdir.clone(),
                cursor_mode: Some(CursorMode::Ask),
                model: "auto".to_owned(),
                opencode_effort: None,
                opencode_mode: None,
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_resume_request_id, resume_sender) = take_pending_acp_request(&pending_requests);
    resume_sender
        .send(Ok(json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [{ "value": "auto", "name": "Auto" }]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [{ "value": "ask", "name": "Ask" }]
                }
            ]
        })))
        .expect("session/resume response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should reuse the persisted session");
    assert_eq!(external_session_id, "cursor-session-1");

    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/resume\""),
        "session/resume request should be written\n{written}"
    );
    assert_emitted_acp_delegation_mcp_descriptor(&written, "session/resume", &created.session_id);
    assert!(
        !written.contains("\"method\":\"session/load\"")
            && !written.contains("\"method\":\"session/new\""),
        "resume support must avoid transcript-replaying load and replacement new requests\n{written}"
    );
    assert!(
        !runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .is_loading_history,
        "session/resume must not suppress live updates as replay history"
    );
}

// Neither a generic error code nor nested/prose invalid-id hints authorize
// replacing the conversation after an advertised continuation method fails.
#[test]
fn acp_continuation_errors_preserve_saved_id_and_advertised_capabilities() {
    for agent in [Agent::Cursor, Agent::Gemini, Agent::OpenCode] {
        for resume in [false, true] {
            for (code, message, data) in [
                (-32601, "Method not found", Value::Null),
                (
                    -32602,
                    "resume rejected",
                    json!({"type": "invalidSessionIdentifier"}),
                ),
                (
                    -32603,
                    "Invalid session identifier",
                    json!({"details": [{"error": "invalidSessionId"}]}),
                ),
            ] {
                let state = test_app_state();
                let session_id = test_session_id(&state, agent);
                state
                    .set_external_session_id(&session_id, "saved-session".to_owned())
                    .unwrap();
                let pending = Arc::new(Mutex::new(HashMap::new()));
                let runtime = Arc::new(Mutex::new(AcpRuntimeState {
                    capabilities: Some(AcpCapabilities {
                        supports_session_load: Some(true),
                        supports_session_resume: Some(resume),
                    }),
                    ..AcpRuntimeState::default()
                }));
                let writer = SharedBufferWriter::default();
                let worker_state = state.clone();
                let worker_session = session_id.clone();
                let worker_pending = pending.clone();
                let worker_runtime = runtime.clone();
                let mut worker_writer = writer.clone();
                let worker = std::thread::spawn(move || {
                    ensure_acp_session_ready(
                        &mut worker_writer,
                        &worker_pending,
                        &worker_state,
                        &worker_session,
                        &worker_runtime,
                        agent.acp_runtime().unwrap(),
                        &AcpPromptCommand {
                            cwd: worker_state.default_workdir.clone(),
                            cursor_mode: None,
                            model: "auto".to_owned(),
                            opencode_effort: None,
                            opencode_mode: None,
                            prompt: "Continue".to_owned(),
                            resume_session_id: Some("saved-session".to_owned()),
                        },
                    )
                });
                let (_, response) = take_pending_acp_request(&pending);
                assert_eq!(runtime.lock().unwrap().is_loading_history, !resume);
                response
                    .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
                        code: Some(code),
                        message: message.to_owned(),
                        data: Some(data),
                    })))
                    .unwrap();
                worker
                    .join()
                    .unwrap()
                    .expect_err("continuation must fail visibly");
                let requests = writer
                    .contents()
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(requests.len(), 1, "no session/new fallback is permitted");
                assert_eq!(
                    requests[0]["method"],
                    if resume {
                        "session/resume"
                    } else {
                        "session/load"
                    }
                );
                let inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session_id).unwrap();
                assert_eq!(
                    inner.sessions[index].external_session_id.as_deref(),
                    Some("saved-session")
                );
                let runtime = runtime.lock().unwrap();
                assert!(!runtime.is_loading_history);
                assert!(runtime.current_session_id.is_none());
                assert_eq!(
                    runtime.capabilities.as_ref().unwrap().supports_session_load,
                    Some(true)
                );
                assert_eq!(
                    runtime
                        .capabilities
                        .as_ref()
                        .unwrap()
                        .supports_session_resume,
                    Some(resume)
                );
            }
        }
    }
}

#[test]
fn established_cursor_config_refresh_preserves_legacy_noop_contract() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            gemini_inherited_effort: None,
            opencode_approval_mode: None,
            kimi_approval_mode: None,
            kimi_mode: None,
            agent: Some(Agent::Cursor),
            name: Some("Cursor Refresh".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Cursor session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("cursor-existing".to_owned()),
        ..AcpRuntimeState::default()
    }));
    let writer = SharedBufferWriter::default();
    let mut stdin = writer.clone();

    handle_acp_session_config_refresh(
        &mut stdin,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::Cursor,
        AcpPromptCommand {
            cwd: state.default_workdir.clone(),
            cursor_mode: Some(CursorMode::Ask),
            model: "auto".to_owned(),
            opencode_effort: None,
            opencode_mode: None,
            prompt: String::new(),
            resume_session_id: Some("cursor-existing".to_owned()),
        },
    )
    .expect("established Cursor refresh should remain a successful no-op");
    assert!(writer.contents().is_empty());
}

// Pins the ACP cancel wire contract: the active external session id is sent in
// a fire-and-forget `session/cancel` notification without allocating a pending
// request or waiting for a response that the protocol never sends.
#[test]
fn acp_cancel_sends_notification_for_active_external_session() {
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("cursor-session-1".to_owned()),
        ..AcpRuntimeState::default()
    }));
    let writer = SharedBufferWriter::default();
    let mut cancel_writer = writer.clone();
    handle_acp_cancel_command(&mut cancel_writer, &runtime_state, AcpAgent::Cursor)
        .expect("cancel notification should be written");

    let written = writer.contents();
    let notification: Value =
        serde_json::from_str(written.trim()).expect("cancel notification should be JSON");
    assert!(
        notification.get("id").is_none()
            && notification.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && notification.get("method").and_then(Value::as_str) == Some("session/cancel")
            && notification
                .pointer("/params/sessionId")
                .and_then(Value::as_str)
                == Some("cursor-session-1"),
        "session/cancel must be a notification for the active external session\n{written}"
    );
}

// Pins the initialization race: a stop before session readiness has no wire
// target and must not turn that ordinary no-op into a runtime failure.
#[test]
fn acp_cancel_before_external_session_ready_is_a_noop() {
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let writer = SharedBufferWriter::default();
    let mut cancel_writer = writer.clone();

    handle_acp_cancel_command(&mut cancel_writer, &runtime_state, AcpAgent::OpenCode)
        .expect("cancel before session readiness should be harmless");

    assert!(
        writer.contents().is_empty(),
        "no cancellation frame should be written without an external session"
    );
}

// Pins the graceful stop path: notification delivery plus a settled prompt
// preserves the external-session continuity signal while still
// terminating the detached subprocess.
#[test]
fn acp_stop_preserves_continuity_after_cancelled_prompt_settles() {
    let fixture = phase_sync::ParkedProcess::spawn();
    let process = fixture.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let turn_lifecycle: AcpTurnLifecycle = Arc::new((Mutex::new(true), Condvar::new()));
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::OpenCode,
        runtime_id: "acp-clean-cancel".to_owned(),
        input_tx,
        #[cfg(windows)]
        process_tree: None,
        process: process.clone(),
        turn_lifecycle: turn_lifecycle.clone(),
    };
    let responder = std::thread::spawn(move || {
        match recv_within_guard(&input_rx, "cancel command should arrive")
            .expect("cancel command should arrive")
        {
            AcpRuntimeCommand::Cancel => {
                set_acp_turn_active(&turn_lifecycle, false);
            }
            _ => panic!("expected ACP cancellation command"),
        }
    });

    runtime
        .stop_with_grace(Duration::from_secs(1))
        .expect("clean ACP stop should succeed");
    responder.join().expect("cancel responder should finish");
    phase_sync::process_exit(&process, "cleanly canceled ACP subprocess reaped");
}

// Pins the never-settles branch without timing assertions: once cancel was
// queued but the prompt lifecycle remains active beyond the injected
// grace, the subprocess is killed but the external session id remains owned by
// the explicit continuation contract rather than this local process observation.
#[test]
fn acp_stop_kills_after_cancel_grace_when_prompt_never_settles() {
    let fixture = phase_sync::ParkedProcess::spawn();
    let process = fixture.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let turn_lifecycle: AcpTurnLifecycle = Arc::new((Mutex::new(true), Condvar::new()));
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::OpenCode,
        runtime_id: "acp-stuck-cancel".to_owned(),
        input_tx,
        #[cfg(windows)]
        process_tree: None,
        process: process.clone(),
        turn_lifecycle,
    };
    let responder = std::thread::spawn(move || {
        match recv_within_guard(&input_rx, "cancel command should arrive")
            .expect("cancel command should arrive")
        {
            AcpRuntimeCommand::Cancel => {}
            _ => panic!("expected ACP cancellation command"),
        }
    });

    runtime
        .stop_with_grace(Duration::from_millis(20))
        .expect("fallback ACP stop should kill the process");
    responder.join().expect("cancel responder should finish");
    phase_sync::process_exit(
        &process,
        "never-settling ACP subprocess killed after cancel grace",
    );
}

// Pins the dead-writer fallback and continuity ownership. Failure to enqueue a
// local cancel command still kills the detached process, but it cannot prove
// the external ACP session invalid and therefore must not request id clearing.
#[test]
fn acp_stop_with_disconnected_writer_kills_but_preserves_continuity() {
    let fixture = phase_sync::ParkedProcess::spawn();
    let process = fixture.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    drop(input_rx);
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::OpenCode,
        runtime_id: "acp-disconnected-cancel".to_owned(),
        input_tx,
        #[cfg(windows)]
        process_tree: None,
        process: process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(true), Condvar::new())),
    };

    shutdown_stopped_runtime(KillableRuntime::Acp(runtime), "disconnected ACP test")
        .expect("disconnected cancel should fall back to process termination");
    phase_sync::process_exit(&process, "disconnected ACP subprocess reaped");
}

#[test]
fn cursor_deliberate_stop_keeps_immediate_termination_contract() {
    let fixture = phase_sync::ParkedProcess::spawn();
    let process = fixture.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-immediate-stop".to_owned(),
        input_tx,
        #[cfg(windows)]
        process_tree: None,
        process: process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(true), Condvar::new())),
    };

    shutdown_stopped_runtime(KillableRuntime::Acp(runtime), "Cursor immediate stop test")
        .expect("Cursor stop should terminate without ACP cancel grace");

    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
        "the OpenCode-only graceful stop must not enqueue session/cancel for Cursor"
    );
    phase_sync::process_exit(
        &process,
        "Cursor subprocess reaped without ACP cancellation",
    );
}

fn agy_fixture_executable(dir: &FsPath) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(if cfg!(windows) { "agy.exe" } else { "agy" });
    fs::write(&path, "native executable discovery fixture; never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::canonicalize(path).unwrap()
}

#[test]
fn gemini_launches_antigravity_acp_with_each_approval_mode() {
    let root = TestTempRoot::create("agy-launch");
    let exe = agy_fixture_executable(&root.path().join("native path with spaces"));
    // Assert the command delivered to the process boundary, including no legacy
    // flags, rather than testing a second implementation of the mapping.
    for (mode, expected) in [
        (None, vec!["acp"]),
        (Some(GeminiApprovalMode::Default), vec!["acp"]),
        (
            Some(GeminiApprovalMode::Yolo),
            vec!["acp", "--dangerously-skip-permissions"],
        ),
        (
            Some(GeminiApprovalMode::AutoEdit),
            vec!["acp", "--mode", "accept-edits"],
        ),
        (
            Some(GeminiApprovalMode::Plan),
            vec!["acp", "--mode", "plan"],
        ),
    ] {
        let command = gemini_acp_command(Some(exe.clone()), mode).unwrap();
        assert_eq!(command.get_program(), exe.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            expected
                .iter()
                .map(std::ffi::OsStr::new)
                .collect::<Vec<_>>()
        );
        assert!(
            command.get_envs().next().is_none(),
            "launch must not inject credential or settings overrides"
        );
    }
    assert!(gemini_acp_command(None, None).is_err());
}

#[test]
fn gemini_discovery_uses_native_agy_and_pins_it_before_changing_cwd() {
    let root = TestTempRoot::create("agy-discovery");
    let shim = root.path().join("shim");
    fs::create_dir_all(&shim).unwrap();
    for name in ["agy.cmd", "agy.bat", "agy.ps1", "gemini.exe", "gemini"] {
        fs::write(shim.join(name), "not the Antigravity native executable").unwrap();
    }
    let path = std::env::join_paths([&shim]).unwrap();
    assert!(resolve_gemini_executable_with(Some(&path), None).is_none());
    let install = agy_fixture_executable(&root.path().join("agy/bin"));
    if cfg!(windows) {
        assert_eq!(
            resolve_gemini_executable_with(Some(&path), Some(root.path())),
            Some(install.clone())
        );
        assert_eq!(
            resolve_gemini_executable_with(None, Some(root.path())),
            Some(install)
        );
    }
    let path_dir = root.path().join("path with spaces");
    let native = agy_fixture_executable(&path_dir);
    let path = std::env::join_paths([&shim, &path_dir]).unwrap();
    assert_eq!(
        resolve_gemini_executable_with(Some(&path), Some(root.path())),
        Some(native.clone())
    );
    let ready = gemini_agent_readiness_with(|| Some(native));
    assert_eq!(ready.agent, Agent::Gemini);
    assert_eq!(ready.status, AgentReadinessStatus::Ready);
    assert!(!ready.blocking);
    assert!(gemini_agent_readiness_with(|| None).blocking);
}

#[cfg(unix)]
#[test]
fn gemini_discovery_skips_nonexecutable_agy_files() {
    use std::os::unix::fs::PermissionsExt;
    let root = TestTempRoot::create("agy-nonexecutable");
    let first = root.path().join("nonexecutable");
    let first_exe = agy_fixture_executable(&first);
    fs::set_permissions(first_exe, fs::Permissions::from_mode(0o644)).unwrap();
    let second = root.path().join("executable");
    let native = agy_fixture_executable(&second);
    let path = std::env::join_paths([first, second]).unwrap();
    assert_eq!(
        resolve_gemini_executable_with(Some(&path), None),
        Some(native)
    );
}

#[test]
fn gemini_auth_uses_only_advertised_noninteractive_methods() {
    let advertised = json!({"authMethods":[
        {"id":"oauth-personal"}, {"id":"oauth-business"}, {"id":"terminal-login"},
        {"id":"gemini-api-key"}, {"id":"agent-platform"}
    ]});
    assert_eq!(agy_auth_method_with(&advertised, false, false), None);
    assert_eq!(
        agy_auth_method_with(&advertised, true, false).as_deref(),
        Some("gemini-api-key")
    );
    assert_eq!(
        agy_auth_method_with(&advertised, false, true).as_deref(),
        Some("agent-platform")
    );
    for initialization in [
        json!({}),
        json!({"authMethods":[]}),
        json!({"authMethods":[{"id":"oauth-personal"},{"id":"terminal-login"},{"id":"vertex-ai"}]}),
    ] {
        assert_eq!(agy_auth_method_with(&initialization, true, true), None);
    }
}

#[test]
fn gemini_readiness_and_auth_do_not_consume_legacy_settings_or_dotenv() {
    home_fixture::run(
        "tests::acp_gemini::gemini_readiness_and_auth_do_not_consume_legacy_settings_or_dotenv",
        |home| {
            let legacy = home.join(".gemini");
            fs::create_dir_all(&legacy).unwrap();
            let settings = legacy.join("settings.json");
            let dotenv = legacy.join(".env");
            fs::write(
                &settings,
                r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
            )
            .unwrap();
            fs::write(&dotenv, "GEMINI_API_KEY=fixture-not-a-real-key\n").unwrap();
            let before = fs::read(&settings).unwrap();
            let ready = gemini_agent_readiness_with(|| Some(home.join("agy.exe")));
            assert!(
                !ready.blocking,
                "cached login is checked by agy, not legacy file heuristics"
            );
            assert_eq!(
                select_acp_auth_method(
                    &json!({"authMethods":[{"id":"gemini-api-key"},{"id":"oauth-personal"}]}),
                    AcpAgent::Gemini,
                    home.to_str().unwrap()
                ),
                None
            );
            assert_eq!(fs::read(&settings).unwrap(), before);
        },
    );
}

#[path = "acp_gemini/permissions.rs"]
mod permissions;

fn gemini_reasoning_fixture(model: &str, effort: Option<&str>) -> Value {
    let mut options = vec![json!({
        "id": "model", "type": "select", "currentValue": model,
        "options": [{"value": "gemini-pro", "name": "Gemini Pro"},
                    {"value": "gemini-flash", "name": "Gemini Flash"},
                    {"value": "auto", "name": "Auto"}]
    })];
    if let Some(effort) = effort {
        options.push(json!({
            "id": "reasoning-tier", "category": "thought_level", "type": "select",
            "currentValue": effort,
            "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"},
                        {"value": "high", "name": "High"}]
        }));
    }
    json!({"sessionId": "agy-conversation", "configOptions": options})
}

struct GeminiConfigWriter<F> {
    frame: Vec<u8>,
    on_request: F,
}

impl<F: FnMut(&Value)> Write for GeminiConfigWriter<F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.frame.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let request = serde_json::from_slice(&self.frame)?;
        self.frame.clear();
        // Publish the entire scripted exchange before the writer consumes its
        // reply. No race or clock determines which observation is newer.
        (self.on_request)(&request);
        Ok(())
    }
}

struct GeminiConfigFixture {
    state: AppState,
    session_id: String,
    handle: AcpRuntimeHandle,
    pending: AcpPendingRequestMap,
    runtime: Arc<Mutex<AcpRuntimeState>>,
}

impl GeminiConfigFixture {
    fn new(default_effort: &str) -> Self {
        Self::with_model(default_effort, "gemini-pro")
    }

    fn with_model(default_effort: &str, model: &str) -> Self {
        let state = test_app_state();
        state
            .update_app_settings(
                serde_json::from_value(json!({"defaultGeminiEffort": default_effort})).unwrap(),
            )
            .unwrap();
        let session_id = state
            .create_session_with_agent_setup_validator(
                serde_json::from_value(json!({"agent": "Gemini", "model": model})).unwrap(),
                |_, _| Ok(()),
            )
            .unwrap()
            .session_id;
        let (handle, _rx) = test_acp_runtime_handle(AcpAgent::Gemini, "agy-config-fixture");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session_id).unwrap();
            inner.session_mut_by_index(index).unwrap().runtime =
                SessionRuntime::Acp(handle.clone());
        }
        Self {
            state,
            session_id,
            handle,
            pending: Arc::new(Mutex::new(HashMap::new())),
            runtime: Arc::new(Mutex::new(AcpRuntimeState::default())),
        }
    }

    fn read(&self, message: Value) {
        handle_acp_message(
            &message,
            &self.state,
            &self.session_id,
            &RuntimeToken::Acp(self.handle.runtime_id.clone()),
            &self.pending,
            &self.runtime,
            &self.handle.input_tx,
            &mut AcpTurnState::default(),
            &mut SessionRecorder::new(self.state.clone(), self.session_id.clone()),
            AcpAgent::Gemini,
        )
        .unwrap();
    }

    fn notify_config(&self, model: &str, effort: Option<&str>) {
        let mut update = gemini_reasoning_fixture(model, effort);
        update["sessionUpdate"] = json!("config_option_update");
        self.read(json!({
            "jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "agy-conversation", "update": update}
        }));
    }

    fn reply(&self, request: &Value, config: Value) {
        self.read(json!({"jsonrpc": "2.0", "id": request["id"], "result": config}));
    }

    fn command(&self, model: &str) -> AcpPromptCommand {
        AcpPromptCommand {
            cwd: self.state.default_workdir.clone(),
            model: model.to_owned(),
            prompt: String::new(),
            resume_session_id: None,
            cursor_mode: None,
            opencode_effort: None,
            opencode_mode: None,
        }
    }

    fn configure_command(
        &self,
        command: AcpPromptCommand,
        on_request: impl FnMut(&Value),
    ) -> Result<String> {
        ensure_acp_session_ready(
            &mut GeminiConfigWriter {
                frame: Vec::new(),
                on_request,
            },
            &self.pending,
            &self.state,
            &self.session_id,
            &self.runtime,
            AcpAgent::Gemini,
            &command,
        )
    }

    fn configure(&self, model: &str, on_request: impl FnMut(&Value)) {
        let session = self
            .configure_command(self.command(model), on_request)
            .unwrap();
        assert_eq!(session, "agy-conversation");
    }

    fn refresh(&self, model: &str, on_request: impl FnMut(&Value)) -> Result<()> {
        handle_acp_session_config_refresh_inner(
            &mut GeminiConfigWriter {
                frame: Vec::new(),
                on_request,
            },
            &self.pending,
            &self.state,
            &self.session_id,
            &self.runtime,
            AcpEngramMcpSource::Runtime {
                token: &RuntimeToken::Acp(self.handle.runtime_id.clone()),
                engram: None,
            },
            AcpAgent::Gemini,
            self.command(model),
        )
    }

    fn prompt(&self, model: &str, on_request: impl FnMut(&Value)) -> Result<()> {
        let mut command = self.command(model);
        command.resume_session_id = self.durable_id();
        handle_acp_prompt_command(
            &mut GeminiConfigWriter {
                frame: Vec::new(),
                on_request,
            },
            &self.pending,
            &self.state,
            &self.session_id,
            &self.runtime,
            &self.handle.turn_lifecycle,
            &RuntimeToken::Acp(self.handle.runtime_id.clone()),
            None,
            AcpAgent::Gemini,
            command,
        )?;
        assert!(wait_for_acp_turn_settle(
            &self.handle.turn_lifecycle,
            Duration::from_secs(5),
        ));
        Ok(())
    }

    fn durable_id(&self) -> Option<String> {
        let inner = self.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&self.session_id).unwrap()]
            .external_session_id
            .clone()
    }

    fn session(&self) -> Session {
        self.state.get_session(&self.session_id).unwrap().session
    }

    fn update_settings(&self, request: Value) -> Result<StateResponse, ApiError> {
        self.state
            .update_session_settings(&self.session_id, serde_json::from_value(request).unwrap())
    }
}

#[test]
fn gemini_reasoning_applies_preference_and_explicit_effort_through_acp() {
    let fixture = GeminiConfigFixture::new("high");
    let mut requests = Vec::new();
    fixture.configure("gemini-pro", |request| {
        requests.push(request["method"].clone());
        match request["method"].as_str().unwrap() {
            "session/new" => fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low"))),
            "session/set_config_option" => {
                assert_eq!(
                    request["params"],
                    json!({"sessionId": "agy-conversation", "configId": "reasoning-tier", "value": "high"})
                );
                fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("high")));
            }
            method => panic!("unexpected request: {method}"),
        }
    });
    assert_eq!(
        requests,
        [json!("session/new"), json!("session/set_config_option")]
    );
    assert_eq!(
        fixture.session().gemini_reasoning.current.as_deref(),
        Some("high")
    );

    fixture
        .update_settings(json!({"geminiEffort": "low"}))
        .unwrap();
    fixture.configure("gemini-pro", |request| {
        assert_eq!(request["method"], "session/set_config_option");
        assert_eq!(
            request["params"],
            json!({"sessionId": "agy-conversation", "configId": "reasoning-tier", "value": "low"})
        );
        fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
    });
    let reasoning = fixture.session().gemini_reasoning;
    assert_eq!(reasoning.requested.as_deref(), Some("low"));
    assert_eq!(reasoning.current.as_deref(), Some("low"));

    for (model, effort) in [("gemini-flash", None), ("auto", Some("low"))] {
        fixture.update_settings(json!({"model": model})).unwrap();
        let mut setters = 0;
        fixture.configure(model, |request| {
            setters += 1;
            assert_eq!(request["method"], "session/set_config_option");
            assert_eq!(
                request["params"],
                json!({"sessionId": "agy-conversation", "configId": "model", "value": model})
            );
            fixture.reply(request, gemini_reasoning_fixture(model, effort));
        });
        assert_eq!(setters, 1, "model change needs only one setter here");
        let session = fixture.session();
        assert_eq!(session.model, model);
        assert_eq!(session.gemini_reasoning.requested.as_deref(), Some("low"));
        assert_eq!(session.gemini_reasoning.current.as_deref(), effort);
        assert_eq!(
            session.gemini_reasoning.options.is_empty(),
            effort.is_none()
        );
    }
}

#[test]
fn gemini_reasoning_reconciles_replies_and_notifications_in_receive_order() {
    for newer_notification in [false, true] {
        let fixture = GeminiConfigFixture::new("high");
        let mut setters = 0;
        fixture.configure("gemini-pro", |request| {
            if request["method"] == "session/new" {
                fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
                return;
            }
            assert_eq!(request["method"], "session/set_config_option");
            assert_eq!(
                request["params"],
                json!({"sessionId": "agy-conversation", "configId": "reasoning-tier", "value": "high"})
            );
            setters += 1;
            fixture.notify_config("gemini-pro", Some("medium"));
            fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("high")));
            if newer_notification && setters == 1 {
                fixture.notify_config("gemini-pro", Some("low"));
            }
        });
        assert_eq!(setters, if newer_notification { 2 } else { 1 });
        assert_eq!(
            fixture.session().gemini_reasoning.current.as_deref(),
            Some("high")
        );
    }
}

#[test]
fn gemini_reasoning_clears_and_repopulates_observation_on_model_notifications() {
    let fixture = GeminiConfigFixture::new("auto");
    fixture.configure("gemini-pro", |request| {
        assert_eq!(request["method"], "session/new");
        fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
    });
    fixture
        .update_settings(json!({"geminiEffort": "low"}))
        .unwrap();

    fixture.notify_config("gemini-flash", Some("medium"));
    let mismatched = fixture.session();
    assert_eq!(mismatched.model, "gemini-pro");
    assert_eq!(
        mismatched.gemini_reasoning.requested.as_deref(),
        Some("low")
    );
    assert!(mismatched.gemini_reasoning.current.is_none());
    assert!(mismatched.gemini_reasoning.options.is_empty());
    assert!(
        fixture
            .update_settings(json!({"geminiEffort": "high"}))
            .is_err()
    );

    fixture.notify_config("gemini-pro", Some("medium"));
    let matching = fixture.session();
    assert_eq!(matching.model, "gemini-pro");
    assert_eq!(matching.gemini_reasoning.current.as_deref(), Some("medium"));
    assert!(
        matching
            .gemini_reasoning
            .options
            .iter()
            .any(|option| option.value == "high")
    );
    fixture
        .update_settings(json!({"geminiEffort": "high"}))
        .unwrap();
}

#[test]
fn gemini_reasoning_only_explicit_provider_default_resets_the_effort() {
    let fixture = GeminiConfigFixture::new("auto");
    let config = |effort| {
        let mut config = gemini_reasoning_fixture("gemini-pro", Some(effort));
        config["configOptions"][1]["options"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "Auto", "value": "auto"}));
        config
    };
    fixture.configure("gemini-pro", |request| {
        assert_eq!(request["method"], "session/new");
        fixture.reply(request, config("high"));
    });
    let reasoning = fixture.session().gemini_reasoning;
    assert!(reasoning.inherited.is_none());
    assert_eq!(reasoning.current.as_deref(), Some("high"));

    fixture
        .update_settings(json!({"geminiEffort": "auto"}))
        .unwrap();
    let mut setters = 0;
    fixture.configure("gemini-pro", |request| {
        setters += 1;
        assert_eq!(request["method"], "session/set_config_option");
        assert_eq!(request["params"]["value"], "auto");
        fixture.reply(request, config("auto"));
    });
    assert_eq!(setters, 1);
    assert_eq!(
        fixture.session().gemini_reasoning.current.as_deref(),
        Some("auto")
    );
}

#[test]
fn gemini_reasoning_keeps_scoped_bootstrap_config_in_reader_order() {
    for setup in ["session/new", "session/load", "session/resume"] {
        let fixture = GeminiConfigFixture::new("high");
        let mut command = fixture.command("gemini-pro");
        if setup != "session/new" {
            command.resume_session_id = Some("agy-conversation".to_owned());
            let capabilities = if setup == "session/load" {
                json!({"agentCapabilities":{"loadSession":true}})
            } else {
                json!({"agentCapabilities":{"sessionCapabilities":{"resume":{}}}})
            };
            update_acp_runtime_capabilities(&fixture.runtime, &capabilities);
        }

        let mut setters = 0;
        let session = fixture.configure_command(command, |request| {
            let method = request["method"].as_str().unwrap();
            if method == setup {
                if setup == "session/load" {
                    fixture.notify_config("gemini-pro", Some("low"));
                    fixture.read(json!({"jsonrpc":"2.0","method":"session/update","params":{
                        "sessionId":"agy-conversation","update":{
                            "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old transcript"}
                        }
                    }}));
                    fixture.reply(request, json!({}));
                } else {
                    fixture.reply(request, gemini_reasoning_fixture("gemini-pro", None));
                    fixture.notify_config("gemini-pro", Some("low"));
                }
                fixture.read(json!({"jsonrpc":"2.0","method":"session/update","params":{
                    "sessionId":"another-conversation","update":{
                        "sessionUpdate":"config_option_update",
                        "configOptions":gemini_reasoning_fixture("gemini-flash", None)["configOptions"]
                    }
                }}));
                return;
            }
            setters += 1;
            assert_eq!(method, "session/set_config_option");
            assert_eq!(request["params"]["configId"], "reasoning-tier");
            assert_eq!(request["params"]["value"], "high");
            fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("high")));
        }).unwrap();
        assert_eq!(session, "agy-conversation");
        assert_eq!(setters, 1, "{setup} retained the scoped effort catalog");
        let session = fixture.session();
        assert_eq!(session.gemini_reasoning.current.as_deref(), Some("high"));
        assert!(
            session.messages.is_empty(),
            "load does not replay the transcript"
        );
    }
}

#[test]
fn gemini_reasoning_definitive_refusal_falls_back_and_invalidates_the_choice() {
    for explicit in [false, true] {
        let fixture = GeminiConfigFixture::new(if explicit { "medium" } else { "high" });
        if explicit {
            fixture.configure("gemini-pro", |request| {
                assert_eq!(request["method"], "session/new");
                fixture.reply(
                    request,
                    gemini_reasoning_fixture("gemini-pro", Some("medium")),
                );
            });
            fixture
                .update_settings(json!({"geminiEffort": "high"}))
                .unwrap();
            fixture.notify_config("gemini-pro", Some("low"));
        }
        let mut attempts = Vec::new();
        fixture.configure("gemini-pro", |request| {
            if request["method"] == "session/new" {
                fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
                return;
            }
            assert_eq!(request["method"], "session/set_config_option");
            let value = request["params"]["value"].as_str().unwrap();
            attempts.push(value.to_owned());
            if value == "high" {
                fixture.read(json!({
                    "jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32602, "message":"Unsupported value",
                        "data":{"reason":"unsupported_value","configId":"reasoning-tier","value":"high"}
                    }
                }));
            } else {
                assert_eq!(value, "medium");
                fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some(value)));
            }
        });
        let expected = if explicit {
            vec!["high", "medium"]
        } else {
            vec!["high"]
        };
        assert_eq!(attempts, expected);
        let reason = fixture.session().gemini_reasoning;
        assert_eq!(
            reason.current.as_deref(),
            Some(if explicit { "medium" } else { "low" })
        );
        assert_eq!(reason.requested.as_deref(), explicit.then_some("high"));
        assert_eq!(
            reason.inherited.as_deref(),
            Some(if explicit { "medium" } else { "high" })
        );
        assert!(!reason.options.iter().any(|option| option.value == "high"));
        fixture.configure("gemini-pro", |request| {
            panic!("repeated invalid selection: {request}")
        });
    }
}

#[test]
fn gemini_reasoning_ambiguous_and_transport_errors_still_reject_the_prompt() {
    for transport in [false, true] {
        let fixture = GeminiConfigFixture::new("high");
        let error = fixture
            .prompt("gemini-pro", |request| {
                if request["method"] == "session/new" {
                    fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
                    return;
                }
                assert_eq!(request["method"], "session/set_config_option");
                if transport {
                    fail_pending_acp_requests(&fixture.pending, "Gemini fixture transport closed");
                } else {
                    let mut update = gemini_reasoning_fixture("gemini-pro", Some("low"));
                    update["sessionUpdate"] = json!("config_option_update");
                    update["configOptions"][1]["options"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|option| option["value"] != "high");
                    fixture.read(json!({"jsonrpc":"2.0","method":"session/update","params":{
                        "sessionId":"agy-conversation","update":update
                    }}));
                    fixture.read(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                        "code":-32602,"message":"Invalid session id"
                    }}));
                }
            })
            .unwrap_err();
        assert_eq!(acp_error_is_transport_failure(&error), transport);
        let detail = if transport {
            "fixture transport closed"
        } else {
            "Invalid session id"
        };
        assert!(format!("{error:#}").contains(detail));
        assert_eq!(
            fixture
                .session()
                .gemini_reasoning
                .options
                .iter()
                .any(|option| option.value == "high"),
            transport
        );
        assert_eq!(fixture.durable_id(), None);
    }
}

#[test]
fn gemini_reasoning_requires_confirmed_explicit_model_before_prompt_write() {
    for (model, acknowledged, superseded, failure) in [
        ("manual-gemini", false, false, Some("Unknown model")),
        (
            "gemini-flash",
            false,
            false,
            Some("did not acknowledge the selected model"),
        ),
        (
            "gemini-flash",
            true,
            true,
            Some("changed the model after acknowledging"),
        ),
        ("manual-gemini", true, false, None),
    ] {
        let fixture = GeminiConfigFixture::with_model("auto", model);
        let mut methods = Vec::new();
        let result = fixture.prompt(model, |request| {
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            match method {
                "session/new" => {
                    fixture.reply(request, gemini_reasoning_fixture("gemini-pro", None));
                }
                "session/set_config_option" => {
                    assert_eq!(request["params"]["configId"], "model");
                    assert_eq!(request["params"]["value"], model);
                    let config = gemini_reasoning_fixture(
                        if acknowledged { model } else { "gemini-pro" },
                        None,
                    );
                    if model == "manual-gemini" && !acknowledged {
                        fixture.read(json!({"jsonrpc":"2.0","id":request["id"],"error":{
                            "code":-32602,"message":"Unknown model"
                        }}));
                    } else {
                        fixture.reply(request, config);
                    }
                    if superseded {
                        fixture.notify_config("gemini-pro", None);
                    }
                }
                "session/prompt" if failure.is_none() => {
                    assert_eq!(fixture.durable_id().as_deref(), Some("agy-conversation"));
                    fixture.reply(request, json!({"stopReason":"end_turn"}));
                }
                _ => panic!("unexpected request: {request}"),
            }
        });
        if let Some(failure) = failure {
            assert!(format!("{:#}", result.unwrap_err()).contains(failure));
            assert_eq!(methods, ["session/new", "session/set_config_option"]);
            assert_eq!(fixture.session().model, model);
            assert_eq!(fixture.durable_id(), None);
        } else {
            result.unwrap();
            assert_eq!(
                methods,
                ["session/new", "session/set_config_option", "session/prompt"]
            );
        }
    }
}

#[test]
fn gemini_reasoning_provider_default_can_resolve_to_a_concrete_model() {
    let fixture = GeminiConfigFixture::with_model("auto", "auto");
    let mut methods = Vec::new();
    fixture
        .prompt("auto", |request| {
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            match method {
                "session/new" => {
                    fixture.reply(request, gemini_reasoning_fixture("gemini-pro", None));
                }
                "session/set_config_option" => {
                    assert_eq!(request["params"]["configId"], "model");
                    assert_eq!(request["params"]["value"], "auto");
                    fixture.reply(request, gemini_reasoning_fixture("gemini-flash", None));
                }
                "session/prompt" => {
                    fixture.reply(request, json!({"stopReason":"end_turn"}));
                }
                _ => panic!("unexpected request: {request}"),
            }
        })
        .unwrap();
    assert_eq!(
        methods,
        ["session/new", "session/set_config_option", "session/prompt"]
    );
    assert_eq!(fixture.session().model, "auto");
}

#[test]
fn gemini_reasoning_manual_refresh_discovers_only_the_selected_model_before_prompt() {
    let fixture = GeminiConfigFixture::new("high");
    fixture
        .refresh("gemini-pro", |request| {
            assert_eq!(request["method"], "session/new");
            fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
        })
        .unwrap();
    fixture
        .update_settings(json!({"model":"gemini-flash"}))
        .unwrap();
    assert!(fixture.session().gemini_reasoning.options.is_empty());
    let mut refreshed = false;
    fixture
        .refresh("gemini-flash", |request| {
            assert!(!refreshed);
            refreshed = true;
            assert_eq!(request["method"], "session/set_config_option");
            assert_eq!(
                request["params"],
                json!({"sessionId":"agy-conversation","configId":"model","value":"gemini-flash"})
            );
            fixture.reply(
                request,
                gemini_reasoning_fixture("gemini-flash", Some("medium")),
            );
        })
        .unwrap();
    assert!(refreshed);
    assert_eq!(
        fixture.session().gemini_reasoning.current.as_deref(),
        Some("medium")
    );
    fixture
        .update_settings(json!({"geminiEffort":"low"}))
        .unwrap();
    assert_eq!(
        fixture.session().gemini_reasoning.requested.as_deref(),
        Some("low")
    );
    assert_eq!(fixture.durable_id(), None);
}

#[test]
fn gemini_reasoning_clone_discovery_is_disposable_until_prompt_admission() {
    let mut fixture = GeminiConfigFixture::new("low");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
            binary_path: "engram".to_owned(),
            home: fixture.state.default_workdir.clone(),
            actor_id: engram_seat_id("dev", &record.session),
            actor_context: engram_actor_context(&record.session),
            store_key: None,
        });
    }
    fixture
        .refresh("gemini-pro", |request| {
            assert_eq!(request["method"], "session/new");
            let mut discovery = gemini_reasoning_fixture("gemini-pro", Some("low"));
            discovery["sessionId"] = json!("discovery-only");
            fixture.reply(request, discovery);
        })
        .unwrap();
    assert_eq!(fixture.durable_id(), None);
    fixture
        .update_settings(json!({"geminiEffort":"high"}))
        .unwrap();
    let (fresh_handle, _rx) = test_acp_runtime_handle(AcpAgent::Gemini, "agy-clone-prompt");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert!(record.runtime_reset_required);
        assert!(record.external_session_id.is_none());
        record.runtime = SessionRuntime::Acp(fresh_handle.clone());
        record.runtime_reset_required = false;
    }
    fixture.handle = fresh_handle;
    fixture.runtime = Arc::new(Mutex::new(AcpRuntimeState::default()));

    let mut methods = Vec::new();
    fixture
        .prompt("gemini-pro", |request| {
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            match method {
                "session/new" => {
                    fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
                }
                "session/set_config_option" => {
                    assert_eq!(request["params"]["value"], "high");
                    assert_eq!(fixture.durable_id(), None);
                    fixture.reply(
                        request,
                        gemini_reasoning_fixture("gemini-pro", Some("high")),
                    );
                }
                "session/prompt" => {
                    assert_eq!(request["params"]["sessionId"], "agy-conversation");
                    assert_eq!(fixture.durable_id().as_deref(), Some("agy-conversation"));
                    fixture.reply(request, json!({"stopReason":"end_turn"}));
                }
                _ => panic!("unexpected request: {request}"),
            }
        })
        .unwrap();
    assert_eq!(
        methods,
        ["session/new", "session/set_config_option", "session/prompt"]
    );

    fixture.runtime = Arc::new(Mutex::new(AcpRuntimeState::default()));
    update_acp_runtime_capabilities(
        &fixture.runtime,
        &json!({"agentCapabilities":{"loadSession":true}}),
    );
    let mut methods = Vec::new();
    fixture
        .prompt("gemini-pro", |request| {
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            assert_eq!(request["params"]["sessionId"], "agy-conversation");
            match method {
                "session/load" => {
                    fixture.reply(
                        request,
                        gemini_reasoning_fixture("gemini-pro", Some("high")),
                    );
                }
                "session/prompt" => {
                    fixture.reply(request, json!({"stopReason":"end_turn"}));
                }
                _ => panic!("unexpected request: {request}"),
            }
        })
        .unwrap();
    assert_eq!(methods, ["session/load", "session/prompt"]);
    assert_eq!(fixture.durable_id().as_deref(), Some("agy-conversation"));
}

#[test]
fn gemini_reasoning_provider_default_accepts_bound_effort_only_updates() {
    let fixed = |effort| {
        let mut config = gemini_reasoning_fixture("fixed-model", Some(effort));
        config["configOptions"].as_array_mut().unwrap().remove(0);
        config
    };
    let fixture = GeminiConfigFixture::with_model("high", "default");
    fixture
        .refresh("default", |request| {
            assert_eq!(request["method"], "session/new");
            fixture.reply(request, fixed("high"));
        })
        .unwrap();
    assert_eq!(
        fixture.session().gemini_reasoning.current.as_deref(),
        Some("high")
    );
    for (external_id, effort, expected) in [
        ("another-conversation", "medium", "high"),
        ("agy-conversation", "low", "low"),
    ] {
        let mut update = fixed(effort);
        update["sessionUpdate"] = json!("config_option_update");
        fixture.read(json!({"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":external_id,"update":update
        }}));
        assert_eq!(
            fixture.session().gemini_reasoning.current.as_deref(),
            Some(expected)
        );
    }
    let mut methods = Vec::new();
    fixture
        .prompt("default", |request| {
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            match method {
                "session/set_config_option" => {
                    assert_eq!(request["params"]["configId"], "reasoning-tier");
                    assert_eq!(request["params"]["value"], "high");
                    fixture.reply(request, fixed("high"));
                }
                "session/prompt" => {
                    fixture.reply(request, json!({"stopReason":"end_turn"}));
                }
                _ => panic!("unexpected request: {request}"),
            }
        })
        .unwrap();
    assert_eq!(methods, ["session/set_config_option", "session/prompt"]);
}

#[test]
fn gemini_reasoning_delayed_refresh_cannot_write_over_a_newer_prompt() {
    let fixture = GeminiConfigFixture::new("auto");
    fixture.configure("gemini-pro", |request| {
        assert_eq!(request["method"], "session/new");
        fixture.reply(request, gemini_reasoning_fixture("gemini-pro", Some("low")));
    });
    let delayed_refresh = fixture.command("gemini-pro");
    fixture
        .update_settings(json!({"model":"gemini-flash"}))
        .unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session_id).unwrap();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    }
    let mut prompt_request = None;
    let token = RuntimeToken::Acp(fixture.handle.runtime_id.clone());
    handle_acp_prompt_command(
        &mut GeminiConfigWriter {
            frame: Vec::new(),
            on_request: |request: &Value| match request["method"].as_str().unwrap() {
                "session/set_config_option" => {
                    assert_eq!(request["params"]["value"], "gemini-flash");
                    fixture.reply(
                        request,
                        gemini_reasoning_fixture("gemini-flash", Some("low")),
                    );
                }
                "session/prompt" => prompt_request = Some(request.clone()),
                method => panic!("unexpected request: {method}"),
            },
        },
        &fixture.pending,
        &fixture.state,
        &fixture.session_id,
        &fixture.runtime,
        &fixture.handle.turn_lifecycle,
        &token,
        None,
        AcpAgent::Gemini,
        fixture.command("gemini-flash"),
    )
    .unwrap();
    let prompt_request = prompt_request.expect("newer prompt reached the provider before refresh");
    let (response_tx, response_rx) = mpsc::channel();
    let refresh_result = handle_acp_session_config_refresh_for_runtime(
        &mut GeminiConfigWriter {
            frame: Vec::new(),
            on_request: |request: &Value| {
                panic!("stale refresh wrote to an active runtime: {request}");
            },
        },
        &fixture.pending,
        &fixture.state,
        &fixture.session_id,
        &fixture.runtime,
        &token,
        None,
        AcpAgent::Gemini,
        delayed_refresh,
        response_tx,
    );
    let prompt_was_active = *fixture.handle.turn_lifecycle.0.lock().unwrap();
    fixture.reply(&prompt_request, json!({"stopReason":"end_turn"}));
    assert!(wait_for_acp_turn_settle(
        &fixture.handle.turn_lifecycle,
        Duration::from_secs(5)
    ));
    assert!(
        refresh_result.is_ok(),
        "refresh deferral must not exit the writer"
    );
    assert!(matches!(
        response_rx.try_recv().unwrap(),
        Err(AcpConfigRefreshError::Deferred)
    ));
    assert!(prompt_was_active);
    assert_eq!(fixture.session().model, "gemini-flash");
}

#[test]
fn gemini_reasoning_preference_persists_and_only_applies_to_new_gemini_sessions() {
    let state = test_app_state();
    let create = |agent| {
        state
            .create_session_with_agent_setup_validator(
                serde_json::from_value(json!({"agent": agent})).unwrap(),
                |_, _| Ok(()),
            )
            .unwrap()
            .session
    };
    let earlier = create("Gemini");
    state
        .update_app_settings(
            serde_json::from_value(json!({
                "defaultGeminiEffort": "high", "defaultCodexReasoningEffort": "low"
            }))
            .unwrap(),
        )
        .unwrap();
    let later = create("Gemini");
    let codex = create("Codex");
    let forwarded = state
        .create_session_with_agent_setup_validator(
            serde_json::from_value(json!({"agent": "Gemini", "geminiInheritedEffort": "low"}))
                .unwrap(),
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(
        forwarded.session.gemini_reasoning.inherited.as_deref(),
        Some("low")
    );
    state
        .observe_gemini_acp_config(
            &later.id,
            None,
            Some(&later.model),
            &gemini_reasoning_fixture(&later.model, Some("high")),
            true,
            &HashSet::new(),
        )
        .unwrap();
    state
        .update_session_settings(
            &later.id,
            serde_json::from_value(json!({"geminiEffort": "low"})).unwrap(),
        )
        .unwrap();
    let tracker = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(tracker.preferences.default_gemini_effort, "high");
    assert_eq!(
        tracker.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::Low
    );
    assert!(
        state
            .get_session(&earlier.id)
            .unwrap()
            .session
            .gemini_reasoning
            .requested
            .is_none()
    );
    assert_eq!(later.gemini_reasoning.inherited.as_deref(), Some("high"));
    let restored = &tracker.sessions[tracker.find_session_index(&later.id).unwrap()];
    assert!(matches!(&restored.runtime, SessionRuntime::None));
    assert!(restored.session.model_options.is_empty());
    assert_eq!(
        restored.session.gemini_reasoning,
        GeminiReasoning {
            requested: Some("low".to_owned()),
            inherited: Some("high".to_owned()),
            ..GeminiReasoning::default()
        }
    );
    assert!(codex.gemini_reasoning.is_empty());
    assert_eq!(codex.reasoning_effort, Some(CodexReasoningEffort::Low));
    assert!(
        state
            .update_session_settings(
                &codex.id,
                serde_json::from_value(json!({"geminiEffort": "auto"})).unwrap()
            )
            .is_err()
    );
}
