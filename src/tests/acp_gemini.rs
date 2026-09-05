// ACP (Agent Client Protocol) is the JSON-RPC dialect spoken by Gemini CLI,
// Cursor, and OpenCode; TermAl implements the client side and drives
// each agent through `initialize`, `session/new`, optional `session/resume` or
// `session/load`, and prompt turns. Resume is used only when explicitly
// advertised under `sessionCapabilities.resume`; `session/load` likewise
// requires explicit support. Continuation errors never replace the saved id.
// Gemini CLI adds its own quirks: it reads `~/.gemini/settings.json` and
// `.env` files from the home directory only, so workspace-local `.env` files
// must be ignored for credentials (they can be committed to a repo and leak
// keys). TermAl also writes an override settings file on Windows to force
// `enableInteractiveShell=false` for headless ACP runs. Production surfaces
// live in `src/acp.rs` and `src/gemini.rs`; protocol types live in runtime.rs.

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
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
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
                cwd: "/tmp".to_owned(),
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
    for agent in [Agent::Cursor, Agent::Gemini, Agent::OpenCode] {
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
                    cwd: "/tmp".to_owned(),
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
            agent: Some(Agent::Cursor),
            name: Some("Cursor Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
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
        current_session_id: None,
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        opencode_config_notification_tx: None,
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(true),
            supports_session_resume: Some(true),
        }),
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
                cwd: "/tmp".to_owned(),
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
                            cwd: "/tmp".to_owned(),
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
            agent: Some(Agent::Cursor),
            name: Some("Cursor Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
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
            cwd: "/tmp".to_owned(),
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
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        opencode_config_notification_tx: None,
        capabilities: None,
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

// Pins `disable_gemini_interactive_shell_in_settings` flipping
// `tools.shell.enableInteractiveShell` to `false` while leaving sibling keys
// (`pager`, `security.auth.selectedType`) intact. Guards against a rewrite
// that clobbers the user's auth selection or other shell preferences.
#[test]
fn disable_gemini_interactive_shell_in_settings_preserves_other_values() {
    let mut settings = json!({
        "security": {
            "auth": {
                "selectedType": "oauth-personal"
            }
        },
        "tools": {
            "shell": {
                "enableInteractiveShell": true,
                "pager": "less"
            }
        }
    });

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        settings.pointer("/tools/shell/pager"),
        Some(&Value::String("less".to_owned()))
    );
    assert_eq!(
        settings.pointer("/security/auth/selectedType"),
        Some(&Value::String("oauth-personal".to_owned()))
    );
}

// Pins the override helper creating the full `/tools/shell/enableInteractiveShell`
// pointer path when the input is `{}`. Guards against a missing-key early return
// that would leave headless runs with Gemini's interactive shell still on.
#[test]
fn disable_gemini_interactive_shell_in_settings_builds_shell_path_from_empty_object() {
    let mut settings = json!({});

    disable_gemini_interactive_shell_in_settings(&mut settings);

    assert_eq!(
        settings.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
}

// Pins `load_gemini_settings_json` returning `{}` (not panicking or propagating
// the parse error) when the file contains broken JSON, and `gemini_selected_auth_type_from_settings_file`
// returning `None`. Guards against a malformed user settings file bricking
// TermAl's own override-file write or auth inspection on Windows.
#[test]
fn load_gemini_settings_json_ignores_malformed_input() {
    let temp_root = TestTempRoot::create("termal-gemini-settings-invalid");
    let settings_path = temp_root.path().join("settings.json");
    fs::write(
        &settings_path,
        r#"{"security": { "auth": { "selectedType": "oauth-personal" }"#,
    )
    .expect("invalid Gemini settings should be written");

    let loaded = load_gemini_settings_json(Some(settings_path.as_path()));
    assert_eq!(loaded, json!({}));
    assert_eq!(
        gemini_selected_auth_type_from_settings_file(settings_path.as_path()),
        None
    );
}

// Pins `gemini_dotenv_env_pairs` returning empty even when a workspace `.env`
// with plausible Gemini/Google keys is present in the current project root.
// Guards against a credential-leak regression where a committed repo `.env`
// would be silently injected into the Gemini ACP child process.
//
// Serialized via `TEST_HOME_ENV_MUTEX` and redirects HOME to an empty
// tempdir so `gemini_env_file_paths` (which reads HOME/USERPROFILE)
// cannot pick up the developer's real `~/.gemini/.env` or race against
// sibling tests that redirect HOME. Without the mutex this raced
// `find_gemini_env_file_reads_home_directory_env_files`, which writes
// a `~/.env` containing `GEMINI_API_KEY` into its own tempdir; if that
// test's HOME redirect overlapped this assertion, `overrides` came back
// non-empty.
#[test]
fn gemini_dotenv_env_pairs_ignore_workspace_env_files() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_root = TestTempRoot::create("termal-gemini-dotenv");
    let project_root = temp_root.path().join("project");
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\nexport GOOGLE_API_KEY='vertex-key'\nGOOGLE_CLOUD_PROJECT=demo-project\nGOOGLE_CLOUD_LOCATION=us-central1\n",
    )
    .expect("Gemini dotenv file should be written");

    let empty_home = temp_root.path().join("home");
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_home_dir(&empty_home);

    let overrides = gemini_dotenv_env_pairs()
        .into_iter()
        .collect::<HashMap<_, _>>();

    assert!(overrides.is_empty());
}

// Pins `find_gemini_env_file` preferring `~/.gemini/.env` and falling back to
// `~/.env`, resolved via the `HOME`/`USERPROFILE` indirection so tests can
// redirect. Guards against workspace-walking behavior re-entering and against
// the fallback order flipping, which would change which key file wins.
#[test]
fn find_gemini_env_file_reads_home_directory_env_files() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_root = TestTempRoot::create("termal-gemini-home-env");
    let home_dir = temp_root.path().to_path_buf();
    let gemini_dir = home_dir.join(".gemini");
    fs::create_dir_all(&gemini_dir).expect("Gemini home directory should be created");

    {
        let _home_env = ScopedEnvVar::set_home_dir(&home_dir);
        assert_eq!(find_gemini_env_file(), None);
        let gemini_env = gemini_dir.join(".env");
        fs::write(&gemini_env, "GEMINI_API_KEY=home-gemini-key\n")
            .expect("Gemini home env should be written");
        assert_eq!(find_gemini_env_file(), Some(gemini_env.clone()));

        fs::remove_file(&gemini_env).expect("Gemini home env should be removed");
        let fallback_env = home_dir.join(".env");
        fs::write(&fallback_env, "GEMINI_API_KEY=home-fallback-key\n")
            .expect("home fallback env should be written");
        assert_eq!(find_gemini_env_file(), Some(fallback_env));
    }
}

// Pins `select_acp_auth_method` returning `None` for Gemini when the only
// source of a `GEMINI_API_KEY` is a workspace `.env` (and no home env or
// selected-auth setting is configured). Guards against auto-selecting
// `gemini-api-key` from a repo-committed credential file.
//
// Serialized via `TEST_HOME_ENV_MUTEX` and explicitly isolates HOME plus
// every Gemini/Google env var that `select_acp_auth_method` reads. Without
// isolation, a sibling test setting a synthetic GEMINI_API_KEY could interfere:
// `env_var_source("GEMINI_API_KEY")` would see the sibling test's process-
// env value, `gemini_api_key_source()` would return `Some(...)`, and this
// assertion would flip from `None` to `Some("gemini-api-key")`.
#[test]
fn select_acp_auth_method_ignores_workspace_dotenv_credentials() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_root = TestTempRoot::create("termal-gemini-auth-method");
    let project_root = temp_root.path().join("project");
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\n",
    )
    .expect("Gemini dotenv file should be written");

    // Point HOME at an empty tempdir so `dotenv_var_source` cannot walk
    // into the developer's real `~/.gemini/.env` or `~/.env`.
    let empty_home = temp_root.path().join("home");
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_home_dir(&empty_home);

    // Unset every env var `gemini_api_key_source` / `gemini_vertex_auth_source`
    // inspect. Each `_unset_X` is an RAII guard that restores the original
    // value on drop, so the developer's real shell env is unaffected.
    let _unset_api_key = ScopedEnvVar::remove("GEMINI_API_KEY");
    let _unset_google_api_key = ScopedEnvVar::remove("GOOGLE_API_KEY");
    let _unset_google_project = ScopedEnvVar::remove("GOOGLE_CLOUD_PROJECT");
    let _unset_google_location = ScopedEnvVar::remove("GOOGLE_CLOUD_LOCATION");
    let _unset_use_vertex = ScopedEnvVar::remove("GOOGLE_GENAI_USE_VERTEXAI");
    let _unset_use_gca = ScopedEnvVar::remove("GOOGLE_GENAI_USE_GCA");

    let initialize_result = json!({
        "authMethods": [
            { "id": "vertex-ai" },
            { "id": "gemini-api-key" }
        ]
    });
    assert_eq!(
        select_acp_auth_method(
            &initialize_result,
            AcpAgent::Gemini,
            project_root
                .to_str()
                .expect("temp path should be valid UTF-8"),
        ),
        None
    );
}

// Pins `prepare_termal_gemini_system_settings` (Windows only) writing a settings
// file whose `/tools/shell/enableInteractiveShell` is `false`. Guards against
// the override being skipped, written to the wrong path, or emitting content
// that lets Gemini re-enable the interactive shell during headless ACP runs.
#[test]
fn prepare_termal_gemini_system_settings_writes_override_file() {
    if !cfg!(windows) {
        return;
    }

    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp_root = TestTempRoot::create("termal-gemini-system-settings");
    let project_root = temp_root.path().join("project");
    fs::create_dir_all(&project_root).expect("Gemini override project root should be created");
    let empty_home = temp_root.path().join("home");
    fs::create_dir_all(&empty_home).expect("Gemini override home dir should be created");
    let _home_env = ScopedEnvVar::set_home_dir(&empty_home);
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    let settings_path = prepare_termal_gemini_system_settings(workdir)
        .expect("Gemini settings override should prepare")
        .expect("Windows should create a Gemini settings override");
    let written: Value = serde_json::from_str(
        &fs::read_to_string(&settings_path).expect("Gemini override file should be readable"),
    )
    .expect("Gemini override file should parse");

    assert_eq!(
        written.pointer("/tools/shell/enableInteractiveShell"),
        Some(&Value::Bool(false))
    );
}

// Pins `gemini_interactive_shell_warning` (Windows only) producing a TermAl-forces
// warning that names the offending settings file when the workspace
// `.gemini/settings.json` enables the interactive shell, and returning `None`
// once that setting is flipped to `false`. Guards against the warning firing
// even after the user complied, or going silent when they haven't.
#[test]
fn gemini_interactive_shell_warning_respects_workspace_settings() {
    if !cfg!(windows) {
        return;
    }

    // Hold the home-env mutex so this test's USERPROFILE and
    // GEMINI_CLI_SYSTEM_SETTINGS_PATH redirects don't race with other
    // home-env tests that run in parallel.
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let temp_root = TestTempRoot::create("termal-gemini-interactive-shell");
    let project_root = temp_root.path().join("project");
    let settings_dir = project_root.join(".gemini");
    fs::create_dir_all(&settings_dir).expect("Gemini settings directory should be created");
    let settings_path = settings_dir.join("settings.json");
    let workdir = project_root
        .to_str()
        .expect("test workdir should be valid UTF-8");

    // Point GEMINI_CLI_SYSTEM_SETTINGS_PATH at a path that does not exist so
    // the real C:\ProgramData\gemini-cli\settings.json (written by TermAl with
    // enableInteractiveShell=false) does not shadow the project setting we are
    // testing here.
    let absent_system_settings = project_root.join("no-system-settings.json");
    let _system_env =
        ScopedEnvVar::set_path("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &absent_system_settings);

    // Redirect USERPROFILE to an empty temp dir so the developer's real
    // ~/.gemini/settings.json is not consulted either.
    let empty_home = temp_root.path().join("home");
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_home_dir(&empty_home);

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":true}}}"#,
    )
    .expect("enabled Gemini settings should be written");
    let enabled_warning = gemini_interactive_shell_warning(workdir)
        .expect("enabled interactive shell should warn on Windows");
    assert!(enabled_warning.contains("TermAl forces Gemini"));
    assert!(enabled_warning.contains(&display_path_for_user(&settings_path)));

    fs::write(
        &settings_path,
        r#"{"tools":{"shell":{"enableInteractiveShell":false}}}"#,
    )
    .expect("disabled Gemini settings should be written");
    assert_eq!(gemini_interactive_shell_warning(workdir), None);
}
