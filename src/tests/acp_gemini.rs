// ACP (Agent Client Protocol) is the JSON-RPC dialect spoken by Claude Code,
// Gemini CLI, Cursor, and OpenCode; TermAl implements the client side and drives
// each agent through `initialize`, `session/new`, optional `session/resume` or
// `session/load`, and prompt turns. Resume is used only when explicitly
// advertised under `sessionCapabilities.resume`; older agents without that
// capability retain the optimistic `session/load` compatibility path.
// Gemini CLI adds its own quirks: it reads `~/.gemini/settings.json` and
// `.env` files from the home directory only, so workspace-local `.env` files
// must be ignored for credentials (they can be committed to a repo and leak
// keys). TermAl also writes an override settings file on Windows to force
// `enableInteractiveShell=false` for headless ACP runs. Production surfaces
// live in `src/runtime.rs`: `acp_supports_session_load`, `acp_session_resume`
// via `ensure_acp_session_ready`, and the Gemini settings/env helpers.

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

// Pins the legacy top-level `capabilities.loadSession` fallback and confirms an
// empty initialize response returns `None` (unknown). Guards against dropping
// the legacy envelope, which older agents still emit, or collapsing absent
// to `Some(false)` and skipping the speculative `session/load` branch.
#[test]
fn acp_supports_session_load_reads_legacy_capabilities() {
    assert_eq!(
        acp_supports_session_load(&json!({
            "capabilities": {
                "loadSession": false,
            }
        })),
        Some(false)
    );
    assert_eq!(acp_supports_session_load(&json!({})), None);
}

// Pins ACP v1's object-shaped `sessionCapabilities.resume`, while retaining
// compatibility with boolean capability shims and a legacy envelope. Absence
// remains `None` and explicit false remains authoritative.
#[test]
fn acp_supports_session_resume_reads_object_boolean_and_legacy_capabilities() {
    assert_eq!(
        acp_supports_session_resume(&json!({
            "agentCapabilities": {
                "sessionCapabilities": {
                    "resume": {}
                }
            }
        })),
        Some(true)
    );
    assert_eq!(
        acp_supports_session_resume(&json!({
            "agentCapabilities": {
                "sessionCapabilities": {
                    "resume": false
                }
            }
        })),
        Some(false)
    );
    assert_eq!(
        acp_supports_session_resume(&json!({
            "capabilities": {
                "sessionCapabilities": {
                    "resume": true
                }
            }
        })),
        Some(true)
    );
    assert_eq!(acp_supports_session_resume(&json!({})), None);
}

#[test]
fn opencode_model_options_filter_values_outside_ingress_bounds() {
    let oversized = "x".repeat(MAX_OPENCODE_MODEL_CHARS + 1);
    let config = json!({
        "configOptions": [{
            "id": "model",
            "options": [
                { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" },
                { "value": oversized.clone(), "name": "Oversized" },
                { "value": "unsafe\nmodel", "name": "Control character" }
            ]
        }]
    });
    let options = acp_model_options(&config, AcpAgent::OpenCode);
    assert_eq!(
        options,
        vec![SessionModelOption::plain(
            "GPT-5.6 Sol",
            "openai/gpt-5.6-sol"
        )]
    );
    assert_eq!(
        acp_model_options(&config, AcpAgent::Cursor)
            .into_iter()
            .map(|option| option.value)
            .collect::<Vec<_>>(),
        vec![
            "openai/gpt-5.6-sol".to_owned(),
            oversized,
            "unsafe\nmodel".to_owned(),
        ],
        "OpenCode's ingress bounds must not silently change Cursor's model list"
    );
}

// Pins `AcpRuntimeState::default().capabilities == None`. Guards against a
// default with pre-filled capabilities, which would bias resume behavior
// before `initialize` has actually been processed.
#[test]
fn acp_runtime_state_defaults_session_load_support_to_unknown() {
    let default_state = AcpRuntimeState::default();
    assert!(
        default_state.capabilities.is_none(),
        "default capabilities must be None so the optimistic \
         session/load path fires before initialize completes"
    );
}

// Pins the optimistic path: with `supports_session_load = None`, `ensure_acp_session_ready`
// writes `session/load`, not `session/new`, and promotes the capability to
// `Some(true)` on success. Guards against older agents being forced into fresh
// sessions (losing history) when capability advertisement is missing.
#[test]
fn acp_session_resume_attempts_load_when_session_load_support_is_unknown() {
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
                opencode_mode: None,
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
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
        "session/new should not be written when resuming with unknown capability support\n{written}"
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

// Pins the compatibility downgrade for legacy non-OpenCode ACP agents that do
// not advertise load support. A typed method-not-found response proves that
// `session/load` is unavailable, so the same activation starts a fresh session
// and future activations skip the unsupported request.
#[test]
fn acp_session_load_method_not_found_downgrades_capability_and_starts_fresh() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Legacy Cursor Resume".to_owned()),
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
                opencode_mode: None,
                prompt: "Resume on a legacy agent".to_owned(),
                resume_session_id: Some("legacy-cursor-session".to_owned()),
            },
        )
    });

    let (_load_request_id, load_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    load_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32601),
            message: "Method not found".to_owned(),
            data: None,
        })))
        .expect("session/load rejection should send");

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "legacy-cursor-fresh",
            "configOptions": []
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("method-not-found should fall back to a fresh session");
    assert_eq!(external_session_id, "legacy-cursor-fresh");

    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/load\"")
            && written.contains("\"method\":\"session/new\""),
        "the optimistic load should be followed by one fresh-session fallback\n{written}"
    );
    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state
            .capabilities
            .as_ref()
            .and_then(|caps| caps.supports_session_load),
        Some(false)
    );
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("legacy-cursor-fresh")
    );
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
                opencode_mode: None,
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_resume_request_id, resume_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
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

// Pins the typed recovery boundary for resume-capable ACP agents. A confirmed
// invalid-session identifier may start a replacement conversation for
// Cursor/Gemini, while OpenCode deliberately preserves its archived
// continuity instead of using this fallback.
#[test]
fn acp_session_resume_typed_invalid_session_starts_fresh_for_cursor() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Typed Resume Recovery".to_owned()),
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
                opencode_mode: None,
                prompt: "Recover from a missing Cursor session".to_owned(),
                resume_session_id: Some("cursor-session-missing".to_owned()),
            },
        )
    });

    let (_resume_request_id, resume_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    resume_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "resume rejected".to_owned(),
            data: Some(json!({ "type": "invalidSessionIdentifier" })),
        })))
        .expect("typed resume rejection should send");

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "cursor-session-fresh",
            "configOptions": []
        })))
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("typed invalid resume should start fresh");
    assert_eq!(external_session_id, "cursor-session-fresh");
    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/resume\"")
            && written.contains("\"method\":\"session/new\"")
            && !written.contains("\"method\":\"session/load\""),
        "typed invalid resume should fall directly back to session/new\n{written}"
    );
}

// Pins R2's authority and ordering contract for OpenCode. Both explicit
// TermAl selections must be applied and acknowledged after session/new, in
// deterministic model-then-mode order, before the handshake returns.
#[test]
fn opencode_session_new_reapplies_explicit_model_then_mode_before_ready() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Explicit Config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "openai/gpt-5.6-sol".to_owned(),
                opencode_mode: Some("plan".to_owned()),
                prompt: "Honor the saved config".to_owned(),
                resume_session_id: None,
            },
        )
    });

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "opencode-session-1",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "opencode/big-pickle",
                    "options": [
                        { "value": "opencode/big-pickle", "name": "Big Pickle" },
                        { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "build",
                    "options": [
                        { "value": "build", "name": "build" },
                        { "value": "plan", "name": "plan" }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    let (_model_request_id, model_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let session_while_config_pending = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session_while_config_pending.external_session_id.as_deref(),
        Some("opencode-session-1"),
        "continuity must publish immediately after session/new, before config acknowledgement"
    );
    assert_eq!(
        runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .current_session_id
            .as_deref(),
        Some("opencode-session-1"),
        "graceful cancellation must see the new external session while config is pending"
    );
    let after_model = writer.contents();
    assert!(
        after_model.contains(
            "\"method\":\"session/set_config_option\",\"params\":{\"configId\":\"model\""
        ),
        "model must be the first explicit OpenCode config request\n{after_model}"
    );
    assert!(
        !after_model.contains("\"params\":{\"configId\":\"mode\""),
        "mode must wait for the model acknowledgement\n{after_model}"
    );
    model_sender
        .send(Ok(json!({})))
        .expect("model config response should send");

    let (_mode_request_id, mode_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let after_mode = writer.contents();
    let model_position = after_mode
        .find("\"params\":{\"configId\":\"model\"")
        .expect("model request should be present");
    let mode_position = after_mode
        .find("\"params\":{\"configId\":\"mode\"")
        .expect("mode request should be present");
    assert!(
        model_position < mode_position,
        "OpenCode model must be acknowledged before mode is sent\n{after_mode}"
    );
    mode_sender
        .send(Ok(json!({})))
        .expect("mode config response should send");

    let external_session_id = handle
        .join()
        .expect("OpenCode ACP worker should finish")
        .expect("OpenCode session should become ready");
    assert_eq!(external_session_id, "opencode-session-1");
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("openai/gpt-5.6-sol")
    );
    assert_eq!(session.model, "openai/gpt-5.6-sol");
    assert_eq!(session.opencode_mode.as_deref(), Some("plan"));
    assert_eq!(session.opencode_current_mode.as_deref(), Some("plan"));
}

// Pins the resume-side rejection branch. A saved explicit selection can be
// valid in the advertised list yet rejected by the agent; resume must still
// complete on the agent's current value and make that recovery visible.
#[test]
fn opencode_resume_survives_explicit_config_rejection() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Resume Config Rejection".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "openai/gpt-5.6-sol".to_owned(),
                opencode_mode: Some(OPENCODE_CONFIG_AUTO.to_owned()),
                prompt: "Resume despite config rejection".to_owned(),
                resume_session_id: Some("opencode-session-resume".to_owned()),
            },
        )
    });

    let (_resume_request_id, resume_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    resume_sender
        .send(Ok(json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "opencode/big-pickle",
                    "options": [
                        { "value": "opencode/big-pickle", "name": "Big Pickle" },
                        { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "build",
                    "options": [
                        { "value": "build", "name": "Build" }
                    ]
                }
            ]
        })))
        .expect("session/resume response should send");
    let (_config_request_id, config_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    config_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "provider rejected the requested model".to_owned(),
            data: None,
        })))
        .expect("config rejection should send");

    let external_session_id = handle
        .join()
        .expect("OpenCode resume worker should finish")
        .expect("config rejection must not fail session/resume");
    assert_eq!(external_session_id, "opencode-session-resume");
    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("opencode/big-pickle")
    );
    assert_eq!(session.model, "opencode/big-pickle");
    assert!(
        session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } if text.contains("provider rejected the requested model")
                && text.contains("continues on `opencode/big-pickle`")
        )),
        "resume-side config recovery should be visible"
    );
}

// Pins R1: an explicit selection that disappeared from OpenCode's dynamic
// config must not remain silently stale. TermAl visibly resets that selection
// to auto and adopts the agent's current effective value.
#[test]
fn opencode_missing_explicit_config_resets_to_auto_with_visible_notice() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Stale Config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "removed/model".to_owned(),
                opencode_mode: Some("removed-mode".to_owned()),
                prompt: "Recover stale config".to_owned(),
                resume_session_id: None,
            },
        )
    });

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "opencode-session-stale",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "opencode/big-pickle",
                    "options": [
                        { "value": "opencode/big-pickle", "name": "Big Pickle" }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "build",
                    "options": [
                        { "value": "build", "name": "build" }
                    ]
                }
            ]
        })))
        .expect("session/new response should send");

    handle
        .join()
        .expect("OpenCode ACP worker should finish")
        .expect("stale selections should recover without a set request");
    let written = writer.contents();
    assert!(
        !written.contains("\"method\":\"session/set_config_option\""),
        "missing explicit selections must reset to auto, not be sent\n{written}"
    );

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
    assert_eq!(session.model, "opencode/big-pickle");
    assert_eq!(session.opencode_mode.as_deref(), Some(OPENCODE_CONFIG_AUTO));
    assert_eq!(session.opencode_current_mode.as_deref(), Some("build"));
    let notice_text = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        notice_text.contains("no longer offers model `removed/model`")
            && notice_text.contains("no longer offers mode `removed-mode`")
            && notice_text.contains("selection to `auto`"),
        "both stale selections need visible recovery notices\n{notice_text}"
    );
}

// Pins R3 against a sanitized response captured from OpenCode 1.18.8. The
// payload says only that the session service failed; it is not a structural
// missing-session discriminator, so TermAl must surface the error and preserve
// the stored continuity id instead of silently creating a replacement session.
#[test]
fn opencode_generic_resume_error_preserves_continuity_without_fallback() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Missing Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .set_external_session_id(&created.session_id, "opencode-session-missing".to_owned())
        .expect("test continuity id should persist");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: OPENCODE_CONFIG_AUTO.to_owned(),
                opencode_mode: Some(OPENCODE_CONFIG_AUTO.to_owned()),
                prompt: "Resume without discarding continuity".to_owned(),
                resume_session_id: Some("opencode-session-missing".to_owned()),
            },
        )
    });

    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/opencode/continuity-wire-errors.json"
    ))
    .expect("captured OpenCode error fixture should parse");
    let error = parse_acp_json_rpc_error(
        fixture
            .pointer("/unknownSessionLoad/error")
            .expect("captured fixture should include an error"),
    );
    let (_resume_request_id, resume_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    resume_sender
        .send(Err(AcpResponseError::JsonRpc(error)))
        .expect("session/resume error should send");

    let error = handle
        .join()
        .expect("OpenCode ACP worker should finish")
        .expect_err("generic OpenCode resume error must be surfaced");
    assert!(
        format!("{error:#}").contains(
            "OpenCode could not resume this conversation. Create a new OpenCode session to start fresh"
        ) && format!("{error:#}").contains("OpenCode service failure"),
        "captured runtime error should preserve the cause and name the non-destructive escape: {error:#}"
    );
    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/resume\"")
            && !written.contains("\"method\":\"session/new\"")
            && !written.contains("\"method\":\"session/load\""),
        "generic OpenCode resume errors must not fall back\n{written}"
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("opencode-session-missing"),
        "failed resume must preserve the stored continuity id"
    );
}

// OpenCode 1.18.8 has no typed invalid-session discriminator safe enough to
// authorize continuity loss. Even a future-looking structured marker must
// therefore preserve the exact stored id until the protocol adds a documented
// typed contract.
#[test]
fn opencode_structured_missing_session_error_preserves_continuity_after_failure() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Structured Missing Resume".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .set_external_session_id(&created.session_id, "opencode-session-gone".to_owned())
        .expect("test continuity id should persist");
    let (runtime, _input_rx) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "opencode-structured-missing");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("OpenCode session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
    }

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: None,
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
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
        handle_acp_prompt_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            &Arc::new((Mutex::new(false), Condvar::new())),
            &RuntimeToken::Acp("opencode-structured-missing".to_owned()),
            AcpAgent::OpenCode,
            AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: OPENCODE_CONFIG_AUTO.to_owned(),
                opencode_mode: Some(OPENCODE_CONFIG_AUTO.to_owned()),
                prompt: "Surface the failed resume".to_owned(),
                resume_session_id: Some("opencode-session-gone".to_owned()),
            },
        )
    });

    let (_resume_request_id, resume_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    resume_sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32001),
            message: "Session service rejected the request".to_owned(),
            data: Some(json!({
                "details": {
                    "type": "invalidSessionIdentifier"
                }
            })),
        })))
        .expect("typed session/resume error should send");

    let error = handle
        .join()
        .expect("OpenCode ACP worker should finish")
        .expect_err("typed missing-session failure must remain visible");
    assert!(format!("{error:#}").contains("Session service rejected"));
    let written = writer.contents();
    assert!(
        written.contains("\"method\":\"session/resume\"")
            && !written.contains("\"method\":\"session/new\""),
        "typed missing-session failures must not silently fall back\n{written}"
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("opencode-session-gone"),
        "OpenCode resume failures must never clear stored continuity in v1"
    );
}

// Pins R6's prompt boundary. OpenCode owns discovery of AGENTS.md/CLAUDE.md;
// TermAl sends only the resolved task envelope and must not duplicate
// repository instruction contents into the ACP prompt.
#[test]
fn opencode_prompt_dispatch_does_not_inject_repository_instruction_files() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-opencode-prompt-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).expect("OpenCode prompt test root should be created");
    fs::write(
        root.join("AGENTS.md"),
        "AGENTS_SENTINEL_MUST_NOT_ENTER_RUNTIME_PROMPT",
    )
    .expect("AGENTS.md fixture should write");
    fs::write(
        root.join("CLAUDE.md"),
        "CLAUDE_SENTINEL_MUST_NOT_ENTER_RUNTIME_PROMPT",
    )
    .expect("CLAUDE.md fixture should write");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Prompt Boundary".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let (runtime, _input_rx) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "opencode-prompt-boundary");

    let started = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("OpenCode session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("OpenCode session index should be valid");
        record.runtime = SessionRuntime::Acp(runtime);
        state
            .start_turn_on_record(
                record,
                "message-opencode-boundary".to_owned(),
                "/review-code".to_owned(),
                Vec::new(),
                Some("Review the current staged and unstaged changes.".to_owned()),
                None,
            )
            .expect("OpenCode turn should dispatch")
    };
    let runtime_prompt = match started.dispatch {
        TurnDispatch::PersistentAcp { command, .. } => command.prompt,
        _ => panic!("OpenCode should dispatch through the ACP runtime"),
    };
    assert_eq!(
        runtime_prompt,
        "Review the current staged and unstaged changes."
    );
    assert!(!runtime_prompt.contains("AGENTS_SENTINEL"));
    assert!(!runtime_prompt.contains("CLAUDE_SENTINEL"));
    assert!(!runtime_prompt.contains("AGENTS.md"));
    assert!(!runtime_prompt.contains("CLAUDE.md"));

    fs::remove_dir_all(root).expect("OpenCode prompt test root should clean up");
}

// Pins R2's live-drift path. ACP config updates arrive on the reader thread,
// so they must snapshot TermAl's persisted explicit selections and queue a
// writer-thread reconciliation instead of merely adopting OpenCode's drift.
#[test]
fn opencode_config_update_queues_explicit_selection_reconciliation() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Config Drift".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("openai/gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_mode: Some("plan".to_owned()),
            },
        )
        .expect("explicit OpenCode mode should persist");

    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();
    let (input_tx, input_rx) = mpsc::channel();
    let config_result = json!({
        "sessionUpdate": "config_options_update",
        "configOptions": [
            {
                "id": "model",
                "currentValue": "opencode/big-pickle",
                "options": [
                    { "value": "opencode/big-pickle", "name": "Big Pickle" },
                    { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                ]
            },
            {
                "id": "mode",
                "currentValue": "build",
                "options": [
                    { "value": "build", "name": "Build" },
                    { "value": "plan", "name": "Plan" }
                ]
            }
        ]
    });

    handle_acp_session_update(
        &config_result,
        &state,
        &created.session_id,
        &input_tx,
        &mut turn_state,
        &mut recorder,
        AcpAgent::OpenCode,
    )
    .expect("OpenCode config update should enqueue reconciliation");

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("writer reconciliation should be queued")
    {
        AcpRuntimeCommand::ReconcileOpenCodeConfig {
            config_result: queued_config,
        } => {
            assert_eq!(queued_config, config_result);
        }
        _ => panic!("expected OpenCode config reconciliation command"),
    }
}

#[test]
fn opencode_model_only_config_payload_preserves_absent_mode_state() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Partial Config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("openai/gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .sync_session_opencode_config(
            &created.session_id,
            Some((
                "openai/gpt-5.6-sol".to_owned(),
                Some("openai/gpt-5.6-sol".to_owned()),
                vec![SessionModelOption::plain(
                    "GPT-5.6 Sol",
                    "openai/gpt-5.6-sol",
                )],
            )),
            Some((
                "plan".to_owned(),
                Some("plan".to_owned()),
                vec![
                    SessionModelOption::plain("Build", "build"),
                    SessionModelOption::plain("Plan", "plan"),
                ],
            )),
            Vec::new(),
        )
        .expect("full OpenCode config fixture should sync");

    let command = state
        .opencode_config_command(&created.session_id)
        .expect("OpenCode config command should resolve");
    let mut writer = SharedBufferWriter::default();
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    reconcile_opencode_config(
        &mut writer,
        &pending_requests,
        &state,
        &created.session_id,
        AcpAgent::OpenCode,
        "opencode-partial-config",
        &command,
        &json!({
            "configOptions": [{
                "id": "model",
                "currentValue": "openai/gpt-5.6-sol",
                "options": [
                    { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" },
                    { "value": "opencode/big-pickle", "name": "Big Pickle" }
                ]
            }]
        }),
    )
    .expect("model-only OpenCode config payload should reconcile");

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(session.opencode_mode.as_deref(), Some("plan"));
    assert_eq!(session.opencode_current_mode.as_deref(), Some("plan"));
    assert_eq!(
        session
            .opencode_mode_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["build", "plan"],
        "a model-only update must not reset or clear the absent mode option"
    );
}

#[test]
fn opencode_invalid_agent_current_values_are_not_persisted() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Invalid Current Config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let command = state
        .opencode_config_command(&created.session_id)
        .expect("OpenCode config command should resolve");
    let mut writer = SharedBufferWriter::default();
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));

    reconcile_opencode_config(
        &mut writer,
        &pending_requests,
        &state,
        &created.session_id,
        AcpAgent::OpenCode,
        "opencode-invalid-current-config",
        &command,
        &json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "malicious\nmodel",
                    "options": [
                        { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                    ]
                },
                {
                    "id": "mode",
                    "currentValue": "build\u{7}",
                    "options": [
                        { "value": "build", "name": "Build" },
                        { "value": "plan", "name": "Plan" }
                    ]
                }
            ]
        }),
    )
    .expect("invalid agent current values should be ignored, not runtime-fatal");

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(session.model, OPENCODE_CONFIG_AUTO);
    assert_eq!(session.opencode_current_mode, None);
    let notices = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        notices.contains("invalid current model") && notices.contains("invalid current mode"),
        "both rejected agent values should produce bounded notices\n{notices}"
    );
    assert!(
        !notices.contains("malicious"),
        "notices must not reflect untrusted agent config values"
    );
}

// Pins the live-update failure boundary: an unsolicited config update can race
// session readiness or be rejected independently of an active prompt. That
// auxiliary failure must be surfaced without escaping to the writer loop and
// tearing down the whole ACP runtime.
#[test]
fn opencode_config_update_reconciliation_failure_is_visible_and_nonfatal() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Config Update Failure".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = SharedBufferWriter::default();

    handle_opencode_config_reconcile_command(
        &mut writer,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        &json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "opencode/big-pickle",
                    "options": [
                        { "value": "opencode/big-pickle", "name": "Big Pickle" }
                    ]
                }
            ]
        }),
    )
    .expect("late config failure should be contained");

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.status,
        SessionStatus::Idle,
        "auxiliary config failures must not change the session lifecycle"
    );
    let notice = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .find(|text| text.contains("OpenCode config update warning"))
        .expect("config reconciliation failure should be visible");
    assert!(
        notice.contains("session is not ready for config reconciliation")
            && notice.contains("current session remains available"),
        "warning should retain actionable failure context: {notice}"
    );
}

#[test]
fn opencode_late_config_update_after_session_removal_is_nonfatal() {
    let state = test_app_state();
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("removed-opencode-session".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = SharedBufferWriter::default();

    handle_opencode_config_reconcile_command(
        &mut writer,
        &pending_requests,
        &state,
        "session-already-removed",
        &runtime_state,
        AcpAgent::OpenCode,
        &json!({
            "configOptions": [{
                "id": "model",
                "currentValue": "opencode/big-pickle",
                "options": [{
                    "value": "opencode/big-pickle",
                    "name": "Big Pickle"
                }]
            }]
        }),
    )
    .expect("a late auxiliary update must not tear down the ACP writer");

    assert!(
        writer.contents().is_empty()
            && pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned")
                .is_empty(),
        "removed sessions must not produce protocol reconciliation work"
    );
}

// Pins the recoverable protocol-rejection branch. OpenCode can reject a saved
// explicit value while the ACP process and prompt remain healthy; TermAl adopts
// the agent's current value, emits a notice, and keeps the writer loop alive.
#[test]
fn opencode_config_rejection_reverts_to_current_without_failing_reconcile() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Config Rejection".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("openai/gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-session-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_session_id = created.session_id.clone();
    let thread_runtime_state = runtime_state.clone();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        handle_opencode_config_reconcile_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            &json!({
                "configOptions": [
                    {
                        "id": "model",
                        "currentValue": "opencode/big-pickle",
                        "options": [
                            { "value": "opencode/big-pickle", "name": "Big Pickle" },
                            { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                        ]
                    },
                    {
                        "id": "mode",
                        "currentValue": "build",
                        "options": [
                            { "value": "build", "name": "Build" }
                        ]
                    }
                ]
            }),
        )
    });

    let (_request_id, response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    response_tx
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "model is temporarily unavailable".to_owned(),
            data: None,
        })))
        .expect("config rejection should send");
    handle
        .join()
        .expect("config reconcile worker should finish")
        .expect("protocol rejection should be contained");

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain present");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("opencode/big-pickle")
    );
    assert_eq!(session.model, "opencode/big-pickle");
    let notice_text = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        notice_text.contains("rejected model `openai/gpt-5.6-sol`")
            && notice_text.contains("model is temporarily unavailable")
            && notice_text.contains("continues on `opencode/big-pickle`"),
        "protocol rejection should be visible and identify the adopted value\n{notice_text}"
    );
}

// Pins the bounded live-reconciliation contract. Some ACP agents acknowledge a
// config change but immediately re-emit the same stale config snapshot. TermAl
// must not turn that behavior into an unbounded set/notification loop.
#[test]
fn opencode_duplicate_config_update_is_reconciled_once() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Config Dedupe".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("openai/gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-session-dedupe".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let config_result = json!({
        "configOptions": [
            {
                "id": "model",
                "currentValue": "opencode/big-pickle",
                "options": [
                    { "value": "opencode/big-pickle", "name": "Big Pickle" },
                    { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                ]
            }
        ]
    });

    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let thread_config = config_result.clone();
    let first = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        handle_opencode_config_reconcile_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            &thread_config,
        )
    });
    let (_request_id, response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    response_tx
        .send(Ok(json!({})))
        .expect("config acknowledgement should send");
    first
        .join()
        .expect("first reconcile worker should finish")
        .expect("first reconcile should succeed");

    let mut duplicate_writer = writer.clone();
    handle_opencode_config_reconcile_command(
        &mut duplicate_writer,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        &config_result,
    )
    .expect("duplicate update should be ignored");

    let mut alternating_writer = writer.clone();
    handle_opencode_config_reconcile_command(
        &mut alternating_writer,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        &json!({
            "configOptions": [{
                "id": "model",
                "currentValue": "openai/gpt-5.6-sol",
                "options": [
                    { "value": "opencode/big-pickle", "name": "Big Pickle" },
                    { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                ]
            }]
        }),
    )
    .expect("a distinct adopted snapshot should reconcile without a set request");
    handle_opencode_config_reconcile_command(
        &mut alternating_writer,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        &config_result,
    )
    .expect("the earlier A snapshot in an A/B/A cycle should stay suppressed");
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty(),
        "duplicate update must not allocate another request"
    );
    assert_eq!(
        writer
            .contents()
            .matches("\"method\":\"session/set_config_option\"")
            .count(),
        1,
        "identical config drift should be reconciled only once"
    );
}

// Pins the fatal boundary: a dead ACP transport is not an ordinary config
// rejection and must still escape to the writer loop's runtime teardown path.
#[test]
fn opencode_config_transport_failure_remains_runtime_fatal() {
    struct ClosedStdin;

    impl std::io::Write for ClosedStdin {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode Config Transport Failure".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("openai/gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-session-transport".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let error = handle_opencode_config_reconcile_command(
        &mut ClosedStdin,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        &json!({
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "opencode/big-pickle",
                    "options": [
                        { "value": "opencode/big-pickle", "name": "Big Pickle" },
                        { "value": "openai/gpt-5.6-sol", "name": "GPT-5.6 Sol" }
                    ]
                }
            ]
        }),
    )
    .expect_err("dead stdin must escape to runtime teardown");
    assert!(
        acp_error_is_transport_failure(&error)
            && format!("{error:#}").contains("failed to encode OpenCode ACP message")
    );
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty(),
        "failed config writes must not leak a pending request"
    );
}

// Pins the no-silent-divergence boundary for live OpenCode settings: selected
// authority remains unchanged while the request is pending and advances only
// after the tracked protocol acknowledgement.
#[test]
fn opencode_live_config_commits_only_after_protocol_acknowledgement() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode acknowledged config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .sync_session_opencode_config(
            &created.session_id,
            Some((
                OPENCODE_CONFIG_AUTO.to_owned(),
                Some("opencode/big-pickle".to_owned()),
                vec![SessionModelOption::plain(
                    "GPT-5.6 Sol",
                    "openai/gpt-5.6-sol",
                )],
            )),
            Some((
                OPENCODE_CONFIG_AUTO.to_owned(),
                Some("build".to_owned()),
                Vec::new(),
            )),
            Vec::new(),
        )
        .expect("OpenCode config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-live-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::from([json!({
            "requestedModel": "auto",
            "requestedMode": "auto",
            "config": {"stale": true}
        })]),
        capabilities: None,
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        handle_opencode_config_apply_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            Some("openai/gpt-5.6-sol".to_owned()),
            None,
            started_tx,
            proceed_rx,
            response_tx,
        )
    });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("serialized config writer should start");
    proceed_tx
        .send(())
        .expect("API scheduling waiter should authorize execution");
    let (_request_id, sender) = take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let before_ack = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(before_ack.opencode_model.as_deref(), Some("auto"));

    sender
        .send(Ok(json!({})))
        .expect("set_config_option acknowledgement should send");
    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("acknowledged update should remain runtime-safe");
    assert_eq!(
        response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("API response should arrive"),
        Ok(())
    );
    let after_ack = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        after_ack.opencode_model.as_deref(),
        Some("openai/gpt-5.6-sol")
    );
    assert_eq!(after_ack.model, "openai/gpt-5.6-sol");
    assert!(
        runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .opencode_reconcile_fingerprints
            .is_empty(),
        "an acknowledged user-authority change must invalidate old reconciliation fingerprints"
    );
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
            opencode_mode: None,
            prompt: String::new(),
            resume_session_id: Some("cursor-existing".to_owned()),
        },
    )
    .expect("established Cursor refresh should remain a successful no-op");
    assert!(writer.contents().is_empty());
}

// A protocol-level setting rejection is scoped to that request: it preserves
// authority and does not escape as a runtime-fatal writer error.
#[test]
fn opencode_live_config_rejection_preserves_authority_and_runtime() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode rejected config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    state
        .sync_session_opencode_config(
            &created.session_id,
            Some((
                OPENCODE_CONFIG_AUTO.to_owned(),
                Some("opencode/big-pickle".to_owned()),
                vec![SessionModelOption::plain(
                    "GPT-5.6 Sol",
                    "openai/gpt-5.6-sol",
                )],
            )),
            Some((
                OPENCODE_CONFIG_AUTO.to_owned(),
                Some("build".to_owned()),
                Vec::new(),
            )),
            Vec::new(),
        )
        .expect("OpenCode config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-rejected-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        handle_opencode_config_apply_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            Some("openai/gpt-5.6-sol".to_owned()),
            None,
            started_tx,
            proceed_rx,
            response_tx,
        )
    });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("serialized config writer should start");
    proceed_tx
        .send(())
        .expect("API scheduling waiter should authorize execution");
    let (_request_id, sender) = take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: "model rejected".to_owned(),
            data: None,
        })))
        .expect("set_config_option rejection should send");
    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("protocol rejection must not tear down the runtime");
    assert!(
        response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("API response should arrive")
            .is_err()
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(session.opencode_model.as_deref(), Some("auto"));
    assert_eq!(session.model, "opencode/big-pickle");
}

#[test]
fn opencode_config_command_skips_apply_after_scheduling_waiter_expires() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("Expired OpenCode config".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("expired-opencode-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let writer = SharedBufferWriter::default();
    let mut stdin = writer.clone();
    let (started_tx, started_rx) = mpsc::channel();
    drop(started_rx);
    let (_proceed_tx, proceed_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();

    handle_opencode_config_apply_command(
        &mut stdin,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        Some("openai/gpt-5.6-sol".to_owned()),
        None,
        started_tx,
        proceed_rx,
        response_tx,
    )
    .expect("expired config command should be dropped without failing the runtime");

    assert!(writer.contents().is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
    assert!(response_rx.recv().is_err());
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
}

#[test]
fn opencode_config_command_requires_post_start_authorization() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("Canceled OpenCode config start".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(OPENCODE_CONFIG_AUTO.to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("OpenCode session should be created");
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("canceled-opencode-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        capabilities: None,
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_session_id = created.session_id.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        handle_opencode_config_apply_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            Some("openai/gpt-5.6-sol".to_owned()),
            None,
            started_tx,
            proceed_rx,
            response_tx,
        )
    });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("serialized config writer should report its start");
    drop(proceed_tx);
    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("canceled start authorization should be a clean no-op");

    assert!(writer.contents().is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
    assert!(response_rx.recv().is_err());
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
}

// Pins the short-circuit: with `supports_session_load = Some(false)`,
// `ensure_acp_session_ready` writes `session/new` and never `session/load`,
// and the capability stays `Some(false)`. Guards against wasting a round-trip
// (and surfacing a spurious error) against agents that explicitly opted out.
#[test]
fn acp_session_resume_skips_load_when_session_load_is_explicitly_unsupported() {
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
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(false),
            supports_session_resume: None,
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
                opencode_mode: None,
                prompt: "Resume the prior session".to_owned(),
                resume_session_id: Some("cursor-session-1".to_owned()),
            },
        )
    });

    let (_new_request_id, new_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    new_sender
        .send(Ok(json!({
            "sessionId": "cursor-session-new",
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
        .expect("session/new response should send");

    let external_session_id = handle
        .join()
        .expect("Cursor ACP worker should finish")
        .expect("Cursor resume should start a fresh ACP session");
    assert_eq!(external_session_id, "cursor-session-new");

    let written = writer.contents();
    assert!(
        !written.contains("\"method\":\"session/load\""),
        "session/load should not be written when support is explicitly unavailable\n{written}"
    );
    assert!(
        written.contains("\"method\":\"session/new\""),
        "session/new should be written when support is explicitly unavailable\n{written}"
    );
    assert_emitted_acp_delegation_mcp_descriptor(&written, "session/new", &created.session_id);

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("cursor-session-new")
    );

    let runtime_state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime_state.current_session_id.as_deref(),
        Some("cursor-session-new")
    );
    assert_eq!(
        runtime_state
            .capabilities
            .as_ref()
            .and_then(|caps| caps.supports_session_load),
        Some(false),
        "explicit not-supported capability must persist unchanged \
         through the session/new fallback"
    );
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
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
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
        match input_rx
            .recv_timeout(Duration::from_secs(1))
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
    assert!(
        wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test ACP")
            .expect("child status should be readable")
            .is_some(),
        "cleanly canceled ACP subprocess should still be reaped"
    );
}

// Pins the never-settles branch without timing assertions: once cancel was
// queued but the prompt lifecycle remains active beyond the injected
// grace, the subprocess is killed but the external session id remains owned by
// the typed resume classifier rather than this local process observation.
#[test]
fn acp_stop_kills_after_cancel_grace_when_prompt_never_settles() {
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
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
        match input_rx
            .recv_timeout(Duration::from_secs(1))
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
    assert!(
        wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test ACP")
            .expect("child status should be readable")
            .is_some(),
        "never-settling ACP subprocess should be killed after the grace bound"
    );
}

// Pins the dead-writer fallback and continuity ownership. Failure to enqueue a
// local cancel command still kills the detached process, but it cannot prove
// the external ACP session invalid and therefore must not request id clearing.
#[test]
fn acp_stop_with_disconnected_writer_kills_but_preserves_continuity() {
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
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
    assert!(
        wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test ACP")
            .expect("child status should be readable")
            .is_some(),
        "disconnected ACP subprocess should still be reaped"
    );
}

#[test]
fn cursor_deliberate_stop_keeps_immediate_termination_contract() {
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
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
    assert!(
        wait_for_shared_child_exit_timeout(&process, Duration::from_secs(1), "test Cursor ACP")
            .expect("child status should be readable")
            .is_some(),
        "Cursor subprocess should be reaped immediately"
    );
}

// Pins `is_gemini_invalid_session_load_error` matching "Invalid session identifier"
// when it appears as an inner anyhow source, not just the outermost message.
// Guards against a `.to_string()`-only check that would miss the substring once
// a context like "session/load failed" is layered on top.
#[test]
fn gemini_invalid_session_load_error_matches_wrapped_chain_messages() {
    let err = anyhow::anyhow!("Invalid session identifier").context("session/load failed");
    assert!(is_gemini_invalid_session_load_error(&err));
}

// Pins `acp_error_data_indicates_invalid_session_identifier` descending through
// `details` wrapper fields and arrays while honoring the depth cap — 10 levels
// match, 11 do not. Guards against unbounded recursion on hostile payloads and
// against false negatives when agents wrap the marker in their own envelopes.
#[test]
fn acp_invalid_session_identifier_detection_handles_wrappers_and_depth_limits() {
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &json!({
            "details": [{
                "error": "invalidSessionId"
            }]
        })
    ));

    let mut boundary = json!("invalidSessionIdentifier");
    for _ in 0..10 {
        boundary = json!({ "details": boundary });
    }
    assert!(acp_error_data_indicates_invalid_session_identifier(
        &boundary
    ));

    let mut nested = json!("invalidSessionIdentifier");
    for _ in 0..11 {
        nested = json!({ "details": nested });
    }
    assert!(!acp_error_data_indicates_invalid_session_identifier(
        &nested
    ));
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
    let settings_path =
        std::env::temp_dir().join(format!("termal-gemini-settings-invalid-{}", Uuid::new_v4()));
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

    let _ = fs::remove_file(settings_path);
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

    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-dotenv-env-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\nexport GOOGLE_API_KEY='vertex-key'\nGOOGLE_CLOUD_PROJECT=demo-project\nGOOGLE_CLOUD_LOCATION=us-central1\n",
    )
    .expect("Gemini dotenv file should be written");

    let empty_home =
        std::env::temp_dir().join(format!("termal-gemini-dotenv-home-{}", Uuid::new_v4()));
    fs::create_dir_all(&empty_home).expect("empty home dir should be created");
    let _home_env = ScopedEnvVar::set_home_dir(&empty_home);

    let overrides = gemini_dotenv_env_pairs()
        .into_iter()
        .collect::<HashMap<_, _>>();

    assert!(overrides.is_empty());

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
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
    let home_dir = std::env::temp_dir().join(format!("termal-gemini-home-env-{}", Uuid::new_v4()));
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

    let _ = fs::remove_dir_all(home_dir);
}

// Pins `select_acp_auth_method` returning `None` for Gemini when the only
// source of a `GEMINI_API_KEY` is a workspace `.env` (and no home env or
// selected-auth setting is configured). Guards against auto-selecting
// `gemini-api-key` from a repo-committed credential file.
//
// Serialized via `TEST_HOME_ENV_MUTEX` and explicitly isolates HOME plus
// every Gemini/Google env var that `select_acp_auth_method` reads. Without
// isolation this test raced `gemini_invalid_session_load_falls_back_to_session_new`
// in `src/tests/mod.rs` (which sets `GEMINI_API_KEY=test-key-not-real`) —
// `env_var_source("GEMINI_API_KEY")` would see the sibling test's process-
// env value, `gemini_api_key_source()` would return `Some(...)`, and this
// assertion would flip from `None` to `Some("gemini-api-key")`.
#[test]
fn select_acp_auth_method_ignores_workspace_dotenv_credentials() {
    let _env_lock = TEST_HOME_ENV_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-auth-method-dotenv-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should be created");
    fs::write(
        project_root.join(".env"),
        "GEMINI_API_KEY=dotenv-gemini-key\n",
    )
    .expect("Gemini dotenv file should be written");

    // Point HOME at an empty tempdir so `dotenv_var_source` cannot walk
    // into the developer's real `~/.gemini/.env` or `~/.env`.
    let empty_home =
        std::env::temp_dir().join(format!("termal-gemini-auth-home-{}", Uuid::new_v4()));
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

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
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
    let project_root =
        std::env::temp_dir().join(format!("termal-gemini-system-settings-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("Gemini override project root should be created");
    let empty_home = std::env::temp_dir().join(format!(
        "termal-gemini-system-settings-home-{}",
        Uuid::new_v4()
    ));
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

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
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

    let project_root = std::env::temp_dir().join(format!(
        "termal-gemini-interactive-shell-{}",
        Uuid::new_v4()
    ));
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
    let empty_home = std::env::temp_dir().join(format!("termal-gemini-home-{}", Uuid::new_v4()));
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

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(empty_home);
}
