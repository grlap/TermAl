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
