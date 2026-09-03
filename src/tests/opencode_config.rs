// Owns OpenCode ACP configuration, resume, reconciliation, and prompt-dispatch tests.
// Does not own generic ACP, Gemini, or Cursor transport behavior.
// Split from acp_gemini.rs to keep OpenCode's dynamic configuration suite cohesive.

use super::*;

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

#[test]
fn opencode_effort_options_are_dynamic_and_filter_values_outside_ingress_bounds() {
    let oversized = "x".repeat(MAX_OPENCODE_EFFORT_CHARS + 1);
    let config = json!({
        "configOptions": [{
            "id": "effort",
            "options": [
                { "value": "none", "name": "None" },
                { "value": "xhigh", "name": "XHigh" },
                { "value": oversized, "name": "Oversized" },
                { "value": "unsafe\nvariant", "name": "Control character" }
            ]
        }]
    });

    assert_eq!(
        acp_opencode_effort_options(&config),
        vec![
            SessionModelOption::plain("None", "none"),
            SessionModelOption::plain("XHigh", "xhigh"),
        ],
        "TermAl must use OpenCode's live variants without hard-coding their names"
    );
}

// Pins R2's authority and ordering contract for OpenCode. Both explicit
// TermAl selections must be applied and acknowledged after session/new, in
// deterministic model-then-effort-then-mode order, before the handshake
// returns.
#[test]
fn opencode_session_new_reapplies_explicit_model_effort_then_mode_before_ready() {
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
                opencode_effort: Some("high".to_owned()),
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
                    "id": "effort",
                    "currentValue": "medium",
                    "options": [
                        { "value": "low", "name": "Low" },
                        { "value": "medium", "name": "Medium" },
                        { "value": "high", "name": "High" }
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
        !after_model.contains("\"params\":{\"configId\":\"effort\"")
            && !after_model.contains("\"params\":{\"configId\":\"mode\""),
        "effort and mode must wait for the model acknowledgement\n{after_model}"
    );
    model_sender
        .send(Ok(json!({})))
        .expect("model config response should send");

    let (_effort_request_id, effort_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let after_effort = writer.contents();
    let model_position = after_effort
        .find("\"params\":{\"configId\":\"model\"")
        .expect("model request should be present");
    let effort_position = after_effort
        .find("\"params\":{\"configId\":\"effort\"")
        .expect("effort request should be present");
    assert!(
        model_position < effort_position,
        "OpenCode model must be acknowledged before effort is sent\n{after_effort}"
    );
    assert!(
        !after_effort.contains("\"params\":{\"configId\":\"mode\""),
        "mode must wait for the effort acknowledgement\n{after_effort}"
    );
    effort_sender
        .send(Ok(json!({})))
        .expect("effort config response should send");

    let (_mode_request_id, mode_sender) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let after_mode = writer.contents();
    let mode_position = after_mode
        .find("\"params\":{\"configId\":\"mode\"")
        .expect("mode request should be present");
    assert!(
        model_position < effort_position && effort_position < mode_position,
        "OpenCode config order must be model, effort, then mode\n{after_mode}"
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
    assert_eq!(session.opencode_effort.as_deref(), Some("high"));
    assert_eq!(session.opencode_current_effort.as_deref(), Some("high"));
    assert_eq!(
        session
            .opencode_effort_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["low", "medium", "high"]
    );
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: "openai/gpt-5.6-sol".to_owned(),
                opencode_effort: None,
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
                opencode_effort: None,
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
            AcpAgent::OpenCode,
            &AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: OPENCODE_CONFIG_AUTO.to_owned(),
                opencode_effort: None,
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
        handle_acp_prompt_command(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            &Arc::new((Mutex::new(false), Condvar::new())),
            &RuntimeToken::Acp("opencode-structured-missing".to_owned()),
            None,
            AcpAgent::OpenCode,
            AcpPromptCommand {
                cwd: "/tmp".to_owned(),
                cursor_mode: None,
                model: OPENCODE_CONFIG_AUTO.to_owned(),
                opencode_effort: None,
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
                None,
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
                codex_fast_mode: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
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
fn opencode_model_only_config_payload_preserves_absent_effort_and_mode_state() {
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "openai/gpt-5.6-sol".to_owned(),
                    current: Some("openai/gpt-5.6-sol".to_owned()),
                    options: vec![SessionModelOption::plain(
                        "GPT-5.6 Sol",
                        "openai/gpt-5.6-sol",
                    )],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "high".to_owned(),
                    current: Some("high".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Low", "low"),
                        SessionModelOption::plain("High", "high"),
                    ],
                }),
                mode: Some(OpenCodeConfigOptionUpdate {
                    selection: "plan".to_owned(),
                    current: Some("plan".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Build", "build"),
                        SessionModelOption::plain("Plan", "plan"),
                    ],
                }),
                notices: Vec::new(),
            },
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
    assert_eq!(session.opencode_effort.as_deref(), Some("high"));
    assert_eq!(session.opencode_current_effort.as_deref(), Some("high"));
    assert_eq!(
        session
            .opencode_effort_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["low", "high"],
        "a model-only update must not reset or clear the absent effort option"
    );
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
        opencode_config_notification_tx: None,
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
        opencode_config_notification_tx: None,
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
        opencode_config_notification_tx: None,
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
        opencode_config_notification_tx: None,
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("opencode/big-pickle".to_owned()),
                    options: vec![SessionModelOption::plain(
                        "GPT-5.6 Sol",
                        "openai/gpt-5.6-sol",
                    )],
                }),
                mode: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("build".to_owned()),
                    options: Vec::new(),
                }),
                ..OpenCodeConfigUpdate::default()
            },
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
        opencode_config_notification_tx: None,
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
            OpenCodeConfigSelections {
                model: Some("openai/gpt-5.6-sol".to_owned()),
                ..OpenCodeConfigSelections::default()
            },
            std::time::Instant::now() + Duration::from_secs(30),
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

// A model acknowledgement is not proof that the old effort list remains
// valid. The writer must wait for OpenCode's post-model config notification
// and validate the dependent selection against that new list.
#[test]
fn opencode_combined_model_and_effort_update_waits_for_model_specific_options() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode model-specific effort".to_owned()),
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("provider/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old", "provider/old-model"),
                        SessionModelOption::plain("New", "provider/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("low".to_owned()),
                    options: vec![SessionModelOption::plain("Low", "low")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("old-model config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-model-effort".to_owned()),
        ..AcpRuntimeState::default()
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
            OpenCodeConfigSelections {
                model: Some("provider/new-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                mode: None,
            },
            std::time::Instant::now() + Duration::from_secs(30),
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
    let (_model_request_id, model_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    model_response_tx
        .send(Ok(json!({})))
        .expect("model acknowledgement should send");

    record_opencode_config_notification(
        &runtime_state,
        &json!({
            "sessionUpdate": "config_options_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "provider/new-model",
                    "options": [
                        { "value": "provider/old-model", "name": "Old" },
                        { "value": "provider/new-model", "name": "New" }
                    ]
                },
                {
                    "id": "effort",
                    "currentValue": "medium",
                    "options": [
                        { "value": "medium", "name": "Medium" },
                        { "value": "xhigh", "name": "Extra High" }
                    ]
                }
            ]
        }),
    );

    let (_effort_request_id, effort_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let written = writer.contents();
    let model_offset = written
        .find("\"configId\":\"model\"")
        .expect("model request should be written");
    let effort_offset = written
        .find("\"configId\":\"effort\"")
        .expect("effort request should be written from refreshed options");
    assert!(model_offset < effort_offset);
    assert!(written.contains("\"value\":\"xhigh\""));
    effort_response_tx
        .send(Ok(json!({})))
        .expect("effort acknowledgement should send");

    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("refreshed combined config should succeed");
    assert_eq!(
        response_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("API response should arrive"),
        Ok(())
    );
    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("provider/new-model")
    );
    assert_eq!(session.opencode_effort.as_deref(), Some("xhigh"));
    assert_eq!(session.opencode_current_effort.as_deref(), Some("xhigh"));
    assert_eq!(
        session
            .opencode_effort_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["medium", "xhigh"]
    );
}

// Once a model acknowledgement has committed, a dependent JSON-RPC rejection
// is partial success: retain the model, adopt the agent-reported dependent
// value, and reset only that dependent authority to auto.
#[test]
fn opencode_dependent_rejection_after_model_change_resets_only_that_selection() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode rejected post-model effort".to_owned()),
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "provider/old-model".to_owned(),
                    current: Some("provider/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old", "provider/old-model"),
                        SessionModelOption::plain("New", "provider/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "xhigh".to_owned(),
                    current: Some("xhigh".to_owned()),
                    options: vec![SessionModelOption::plain("Extra High", "xhigh")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("old-model config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-rejected-post-model-effort".to_owned()),
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
        apply_opencode_config_update(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            OpenCodeConfigSelections {
                model: Some("provider/new-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                mode: None,
            },
            std::time::Instant::now() + Duration::from_secs(30),
        )
    });

    let (_model_request_id, model_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    model_response_tx
        .send(Ok(json!({})))
        .expect("model acknowledgement should send");
    record_opencode_config_notification(
        &runtime_state,
        &json!({
            "sessionUpdate": "config_options_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "provider/new-model",
                    "options": [
                        { "value": "provider/old-model", "name": "Old" },
                        { "value": "provider/new-model", "name": "New" }
                    ]
                },
                {
                    "id": "effort",
                    "currentValue": "medium",
                    "options": [
                        { "value": "medium", "name": "Medium" },
                        { "value": "xhigh", "name": "Extra High" }
                    ]
                }
            ]
        }),
    );

    let (_effort_request_id, effort_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    let oversized_rejection = format!(
        "{}UNBOUNDED_REJECTION_TAIL",
        "x".repeat(MAX_OPENCODE_CONFIG_NOTICE_DETAIL_CHARS + 128)
    );
    effort_response_tx
        .send(Err(AcpResponseError::JsonRpc(AcpJsonRpcError {
            code: Some(-32602),
            message: oversized_rejection,
            data: None,
        })))
        .expect("effort rejection should send");

    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("dependent rejection should remain partial success");
    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("provider/new-model")
    );
    assert_eq!(
        session.opencode_effort.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
    assert_eq!(session.opencode_current_effort.as_deref(), Some("medium"));
    assert_eq!(
        session
            .opencode_effort_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["medium", "xhigh"]
    );
    let reset_notice = session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } if text.contains("rejected the selection") => Some(text),
            _ => None,
        })
        .expect("dependent rejection should append a visible reset notice");
    assert!(reset_notice.contains("effort `xhigh`"));
    assert!(reset_notice.contains("selection to `auto`"));
    assert!(reset_notice.contains('…'));
    assert!(!reset_notice.contains("UNBOUNDED_REJECTION_TAIL"));
    assert!(
        reset_notice.chars().count() <= MAX_OPENCODE_CONFIG_NOTICE_DETAIL_CHARS + 320,
        "bounded rejection notice was unexpectedly large"
    );
    assert_eq!(
        runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .current_session_id
            .as_deref(),
        Some("opencode-rejected-post-model-effort")
    );
}

#[test]
fn opencode_model_change_resets_missing_carried_effort_to_auto() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode missing carried effort".to_owned()),
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "provider/old-model".to_owned(),
                    current: Some("provider/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old", "provider/old-model"),
                        SessionModelOption::plain("New", "provider/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "xhigh".to_owned(),
                    current: Some("xhigh".to_owned()),
                    options: vec![SessionModelOption::plain("Extra High", "xhigh")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("old-model config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-missing-carried-effort".to_owned()),
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
        apply_opencode_config_update(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            OpenCodeConfigSelections {
                model: Some("provider/new-model".to_owned()),
                effort: Some("xhigh".to_owned()),
                mode: None,
            },
            std::time::Instant::now() + Duration::from_secs(30),
        )
    });

    let (_model_request_id, model_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    model_response_tx
        .send(Ok(json!({})))
        .expect("model acknowledgement should send");
    record_opencode_config_notification(
        &runtime_state,
        &json!({
            "sessionUpdate": "config_options_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "provider/new-model",
                    "options": [
                        { "value": "provider/old-model", "name": "Old" },
                        { "value": "provider/new-model", "name": "New" }
                    ]
                },
                {
                    "id": "effort",
                    "currentValue": "medium",
                    "options": [
                        { "value": "medium", "name": "Medium" }
                    ]
                }
            ]
        }),
    );

    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("missing carried effort should degrade without failing the model change");
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty(),
        "missing effort must not emit a second protocol request"
    );
    let written = writer.contents();
    assert_eq!(written.matches("session/set_config_option").count(), 1);

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("provider/new-model")
    );
    assert_eq!(
        session.opencode_effort.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
    assert_eq!(session.opencode_current_effort.as_deref(), Some("medium"));
    assert_eq!(
        session
            .opencode_effort_options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["medium"]
    );
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text {
            author: Author::Assistant,
            text,
            ..
        } if text.contains("no longer offers that selection")
            && text.contains("effort `xhigh`")
            && text.contains("selection to `auto`")
    )));
}

#[test]
fn opencode_post_model_options_timeout_resets_dependents_without_failing_writer() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode post-model timeout".to_owned()),
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "provider/old-model".to_owned(),
                    current: Some("provider/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old", "provider/old-model"),
                        SessionModelOption::plain("New", "provider/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "high".to_owned(),
                    current: Some("high".to_owned()),
                    options: vec![SessionModelOption::plain("High", "high")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("old-model config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-post-model-timeout".to_owned()),
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
        apply_opencode_config_update_with_timeout(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            OpenCodeConfigSelections {
                model: Some("provider/new-model".to_owned()),
                effort: Some("high".to_owned()),
                mode: None,
            },
            std::time::Instant::now() + Duration::from_secs(30),
            Duration::from_millis(20),
        )
    });

    let (_model_request_id, model_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    model_response_tx
        .send(Ok(json!({})))
        .expect("model acknowledgement should send");
    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("missing post-model notification must remain runtime-safe");

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("provider/new-model")
    );
    assert_eq!(
        session.opencode_effort.as_deref(),
        Some(OPENCODE_CONFIG_AUTO)
    );
    assert_eq!(session.opencode_current_effort, None);
    assert!(session.opencode_effort_options.is_empty());
    let timeout_notice = session.messages.iter().find_map(|message| match message {
        Message::Text {
            author: Author::Assistant,
            text,
            ..
        } if text.contains("did not publish model-specific config options") => Some(text),
        _ => None,
    });
    let timeout_notice = timeout_notice.expect("timeout recovery notice should be visible");
    assert!(timeout_notice.contains("selection to `auto`"));
    assert!(timeout_notice.contains("Use `Refresh models`"));
    assert!(!timeout_notice.contains("OpenCode OpenCode"));
    let runtime = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    assert_eq!(
        runtime.current_session_id.as_deref(),
        Some("opencode-post-model-timeout")
    );
    assert!(runtime.opencode_config_notification_tx.is_none());
}

// A split post-model refresh can legitimately omit one dependent list. The
// list that did arrive remains authoritative and is applied; only the missing
// dependent falls back to auto when the bounded wait expires.
#[test]
fn opencode_post_model_timeout_preserves_and_applies_reported_dependent() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("OpenCode partial post-model timeout".to_owned()),
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "provider/old-model".to_owned(),
                    current: Some("provider/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old", "provider/old-model"),
                        SessionModelOption::plain("New", "provider/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "high".to_owned(),
                    current: Some("high".to_owned()),
                    options: vec![SessionModelOption::plain("High", "high")],
                }),
                mode: Some(OpenCodeConfigOptionUpdate {
                    selection: "plan".to_owned(),
                    current: Some("plan".to_owned()),
                    options: vec![SessionModelOption::plain("Plan", "plan")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("old-model config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-partial-post-model-timeout".to_owned()),
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
        apply_opencode_config_update_with_timeout(
            &mut stdin,
            &thread_pending_requests,
            &thread_state,
            &thread_session_id,
            &thread_runtime_state,
            AcpAgent::OpenCode,
            OpenCodeConfigSelections {
                model: Some("provider/new-model".to_owned()),
                effort: Some("high".to_owned()),
                mode: Some("plan".to_owned()),
            },
            std::time::Instant::now() + Duration::from_secs(30),
            Duration::from_millis(30),
        )
    });

    let (_model_request_id, model_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    model_response_tx
        .send(Ok(json!({})))
        .expect("model acknowledgement should send");
    record_opencode_config_notification(
        &runtime_state,
        &json!({
            "sessionUpdate": "config_options_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "provider/new-model",
                    "options": [
                        { "value": "provider/old-model", "name": "Old" },
                        { "value": "provider/new-model", "name": "New" }
                    ]
                },
                {
                    "id": "effort",
                    "currentValue": "medium",
                    "options": [
                        { "value": "medium", "name": "Medium" },
                        { "value": "high", "name": "High" }
                    ]
                }
            ]
        }),
    );

    let (_effort_request_id, effort_response_tx) =
        take_pending_acp_request(&pending_requests, Duration::from_secs(1));
    effort_response_tx
        .send(Ok(json!({})))
        .expect("reported effort acknowledgement should send");
    handle
        .join()
        .expect("OpenCode config writer should finish")
        .expect("partial post-model timeout should remain runtime-safe");

    let session = state
        .full_snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == created.session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(
        session.opencode_model.as_deref(),
        Some("provider/new-model")
    );
    assert_eq!(session.opencode_effort.as_deref(), Some("high"));
    assert_eq!(session.opencode_current_effort.as_deref(), Some("high"));
    assert_eq!(session.opencode_mode.as_deref(), Some(OPENCODE_CONFIG_AUTO));
    assert_eq!(session.opencode_current_mode, None);
    assert!(session.opencode_mode_options.is_empty());
    assert_eq!(
        writer
            .contents()
            .matches("session/set_config_option")
            .count(),
        2
    );
    let notices = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } if text.contains("did not publish model-specific config options") => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("Use `Refresh models`"))
    );
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("mode `plan`"));
    assert!(!notices[0].contains("effort `high`"));
}

#[test]
fn opencode_post_model_options_accumulate_across_repeated_model_notifications() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(OpenCodeConfigNotification {
            model: Some("provider/new-model".to_owned()),
            effort: Some((
                Some("high".to_owned()),
                vec![SessionModelOption::plain("High", "high")],
            )),
            mode: None,
        })
        .expect("effort notification should queue");
    sender
        .send(OpenCodeConfigNotification {
            model: Some("provider/new-model".to_owned()),
            effort: None,
            mode: Some((
                Some("build".to_owned()),
                vec![SessionModelOption::plain("Build", "build")],
            )),
        })
        .expect("mode notification should queue");

    let options = wait_for_opencode_post_model_options(
        &receiver,
        "provider/new-model",
        true,
        true,
        Duration::from_millis(50),
    )
    .expect("split expected-model notifications should accumulate");
    assert_eq!(
        options
            .effort
            .as_ref()
            .and_then(|(current, _)| current.as_deref()),
        Some("high")
    );
    assert_eq!(
        options
            .mode
            .as_ref()
            .and_then(|(current, _)| current.as_deref()),
        Some("build")
    );
}

#[test]
fn acp_config_update_kinds_share_one_classification() {
    assert!(is_acp_config_update_kind("config_options_update"));
    assert!(is_acp_config_update_kind("config_update"));
    assert!(!is_acp_config_update_kind("mode_update"));
}

#[test]
fn opencode_post_model_options_accept_dependent_only_notification_after_ack() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(OpenCodeConfigNotification {
            model: None,
            effort: Some((
                Some("high".to_owned()),
                vec![SessionModelOption::plain("High", "high")],
            )),
            mode: Some((
                Some("build".to_owned()),
                vec![SessionModelOption::plain("Build", "build")],
            )),
        })
        .expect("dependent-only notification should queue");

    let options = wait_for_opencode_post_model_options(
        &receiver,
        "provider/new-model",
        true,
        true,
        Duration::from_millis(50),
    )
    .expect("first dependent-only notification after model ack should be authoritative");
    assert_eq!(
        options
            .effort
            .as_ref()
            .and_then(|(current, _)| current.as_deref()),
        Some("high")
    );
    assert_eq!(
        options
            .mode
            .as_ref()
            .and_then(|(current, _)| current.as_deref()),
        Some("build")
    );
}

#[test]
fn opencode_post_model_options_reject_model_less_followup_after_mismatch() {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(OpenCodeConfigNotification {
            model: Some("provider/other-model".to_owned()),
            effort: None,
            mode: None,
        })
        .expect("mismatched-model notification should queue");
    sender
        .send(OpenCodeConfigNotification {
            model: None,
            effort: Some((
                Some("high".to_owned()),
                vec![SessionModelOption::plain("High", "high")],
            )),
            mode: None,
        })
        .expect("dependent-only notification should queue");
    drop(sender);

    let incomplete = wait_for_opencode_post_model_options(
        &receiver,
        "provider/new-model",
        true,
        false,
        Duration::from_millis(50),
    )
    .expect_err("model-less update after an explicit mismatch must stay untrusted");
    assert!(incomplete.options.effort.is_none());
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
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("opencode/big-pickle".to_owned()),
                    options: vec![SessionModelOption::plain(
                        "GPT-5.6 Sol",
                        "openai/gpt-5.6-sol",
                    )],
                }),
                mode: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("build".to_owned()),
                    options: Vec::new(),
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("OpenCode config fixture should sync");

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("opencode-rejected-config".to_owned()),
        is_loading_history: false,
        opencode_reconcile_fingerprints: VecDeque::new(),
        opencode_config_notification_tx: None,
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
            OpenCodeConfigSelections {
                model: Some("openai/gpt-5.6-sol".to_owned()),
                ..OpenCodeConfigSelections::default()
            },
            std::time::Instant::now() + Duration::from_secs(30),
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
        opencode_config_notification_tx: None,
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
        OpenCodeConfigSelections {
            model: Some("openai/gpt-5.6-sol".to_owned()),
            ..OpenCodeConfigSelections::default()
        },
        std::time::Instant::now() + Duration::from_secs(30),
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
fn opencode_config_command_rejects_expired_execution_after_start_authorization() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("Expired authorized OpenCode config".to_owned()),
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
        current_session_id: Some("expired-authorized-opencode-config".to_owned()),
        ..AcpRuntimeState::default()
    }));
    let writer = SharedBufferWriter::default();
    let mut stdin = writer.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    proceed_tx
        .send(())
        .expect("API scheduling waiter should authorize execution");

    handle_opencode_config_apply_command(
        &mut stdin,
        &pending_requests,
        &state,
        &created.session_id,
        &runtime_state,
        AcpAgent::OpenCode,
        OpenCodeConfigSelections {
            model: Some("openai/gpt-5.6-sol".to_owned()),
            ..OpenCodeConfigSelections::default()
        },
        std::time::Instant::now() - Duration::from_millis(1),
        started_tx,
        proceed_rx,
        response_tx,
    )
    .expect("an expired request should not tear down the runtime");

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("serialized config writer should report its start");
    let detail = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("API response should arrive")
        .expect_err("expired writer execution should be rejected");
    assert!(detail.contains("deadline expired"), "{detail}");
    assert!(writer.contents().is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
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
        opencode_config_notification_tx: None,
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
            OpenCodeConfigSelections {
                model: Some("openai/gpt-5.6-sol".to_owned()),
                ..OpenCodeConfigSelections::default()
            },
            std::time::Instant::now() + Duration::from_secs(30),
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
