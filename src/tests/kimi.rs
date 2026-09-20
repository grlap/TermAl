// Kimi native discovery and ACP integration regressions. Uses disposable paths
// and scripted JSON-RPC replies, not a provider login or a paid model prompt.
use super::*;

fn kimi_fixture_executable(dir: &FsPath) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(if cfg!(windows) { "kimi.exe" } else { "kimi" });
    fs::write(&path, "fixture path only; never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::canonicalize(path).unwrap()
}

#[test]
fn kimi_discovery_uses_path_then_native_install_and_never_shell_shims() {
    let root = TestTempRoot::create("kimi-discovery");
    let shim = root.path().join("shim");
    fs::create_dir_all(&shim).unwrap();
    for name in ["kimi.cmd", "kimi.bat", "kimi.ps1"] {
        fs::write(shim.join(name), "never execute").unwrap();
    }
    let path = std::env::join_paths([&shim]).unwrap();
    assert!(resolve_kimi_executable_with(Some(&path), None).is_none());
    let installed = kimi_fixture_executable(&root.path().join(".kimi-code/bin"));
    assert_eq!(
        resolve_kimi_executable_with(Some(&path), Some(root.path())),
        Some(installed.clone())
    );
    assert_eq!(
        resolve_kimi_executable_with(None, Some(root.path())),
        Some(installed)
    );
    let native_dir = root.path().join("native path with spaces");
    let native = kimi_fixture_executable(&native_dir);
    let path = std::env::join_paths([&shim, &native_dir]).unwrap();
    let resolved = resolve_kimi_executable_with(Some(&path), Some(root.path()));
    assert_eq!(resolved, Some(native.clone()));
    let command = kimi_acp_command(resolved.clone()).unwrap();
    assert_eq!(command.get_program(), native.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![std::ffi::OsStr::new("acp")]
    );
    let ready = kimi_agent_readiness_with(|| resolved);
    assert_eq!(ready.agent, Agent::Kimi);
    assert_eq!(ready.status, AgentReadinessStatus::Ready);
    assert!(!ready.blocking);
    let missing = kimi_agent_readiness_with(|| None);
    assert!(missing.blocking);
    assert!(missing.detail.contains("kimi login"));
    assert!(kimi_acp_command(None).is_err());
}

#[test]
fn kimi_identity_defaults_and_capability_boundaries() {
    for alias in ["kimi", "kimi-code"] {
        assert_eq!(Agent::from_str(alias).unwrap(), Agent::Kimi);
        assert_eq!(
            Agent::parse(vec![alias.to_owned()].into_iter()).unwrap(),
            Agent::Kimi
        );
    }
    assert_eq!(serde_json::to_value(Agent::Kimi).unwrap(), json!("Kimi"));
    assert_eq!(
        serde_json::from_value::<Agent>(json!("Kimi")).unwrap(),
        Agent::Kimi
    );
    assert_eq!(Agent::Kimi.acp_runtime(), Some(AcpAgent::Kimi));
    assert_eq!(Agent::Kimi.default_model(), "auto");
    assert!(!Agent::Kimi.supports_structured_review_results());
    let mut preferences = AppPreferences::default();
    assert_eq!(preferences.default_model_for_agent(Agent::Kimi), "auto");
    preferences.default_kimi_model = "configured-kimi-model".to_owned();
    let preferences: AppPreferences =
        serde_json::from_value(serde_json::to_value(preferences).unwrap()).unwrap();
    assert_eq!(
        preferences.default_model_for_agent(Agent::Kimi),
        "configured-kimi-model"
    );
    let collected = collect_agent_readiness_with("/tmp", |agent, _| AgentReadiness {
        agent,
        status: AgentReadinessStatus::Ready,
        blocking: false,
        detail: String::new(),
        warning_detail: None,
        command_path: None,
    });
    assert_eq!(
        collected
            .iter()
            .filter(|entry| entry.agent == Agent::Kimi)
            .count(),
        1
    );
}

// Replies on flush after the complete frame has been serialized. No polling,
// sleeps, subprocesses, or dependence on the machine's Kimi credentials.
struct KimiReplyWriter {
    pending: AcpPendingRequestMap,
    buffer: Vec<u8>,
    frames: Vec<Value>,
    reply: Value,
    error: Option<String>,
}

impl Write for KimiReplyWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let frame: Value = serde_json::from_slice(&self.buffer)?;
        self.buffer.clear();
        if let Some(id) = frame["id"].as_str() {
            self.pending
                .lock()
                .unwrap()
                .remove(id)
                .unwrap()
                .send(self.error.clone().map_or_else(
                    || Ok(self.reply.clone()),
                    |error| Err(AcpResponseError::Transport(error)),
                ))
                .unwrap();
        }
        self.frames.push(frame);
        Ok(())
    }
}

#[test]
fn kimi_uses_advertised_auth_live_model_config_and_manual_permissions() {
    // Capability/auth subset observed from installed Kimi Code CLI 2.0.2 via
    // initialize (no login or prompt), 2026-09-20.
    let initialize = json!({
        "protocolVersion": 1,
        "agentCapabilities": {"loadSession": true, "sessionCapabilities": {"resume": {}}},
        "authMethods": [{"id": "login", "type": "terminal", "name": "Login with Kimi account"}]
    });
    assert_eq!(acp_supports_session_load(&initialize), Some(true));
    assert_eq!(acp_supports_session_resume(&initialize), Some(true));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = KimiReplyWriter {
        pending: pending.clone(),
        buffer: vec![],
        frames: vec![],
        reply: json!({}),
        error: None,
    };
    maybe_authenticate_acp_runtime(&mut writer, &pending, &initialize, AcpAgent::Kimi, "/tmp")
        .unwrap();
    assert_eq!(writer.frames[0]["method"], "authenticate");
    assert_eq!(writer.frames[0]["params"]["methodId"], "login");
    assert!(
        select_acp_auth_method(
            &json!({"authMethods":[{"id":"untrusted-command"}]}),
            AcpAgent::Kimi,
            "/tmp"
        )
        .is_none()
    );

    let config = json!({"configOptions": [{
        "id": "model", "category": "model", "type": "select", "name": "Model",
        "currentValue": "configured-default", "options": [
            {"value":"configured-default", "name":"Configured model"},
            {"value":"another-model", "name":"Another model"}
        ]
    }]});
    let options = acp_model_options(&config, AcpAgent::Kimi);
    assert_eq!(options.len(), 2);
    let before = writer.frames.len();
    assert_eq!(
        configure_acp_session(
            &mut writer,
            &pending,
            AcpAgent::Kimi,
            "kimi-session",
            "auto",
            None,
            &config
        )
        .unwrap()
        .as_deref(),
        Some("configured-default")
    );
    assert_eq!(
        writer.frames.len(),
        before,
        "Auto must not override the CLI model"
    );
    let mut acknowledged = config.clone();
    acknowledged["configOptions"][0]["currentValue"] = json!("another-model");
    writer.reply = acknowledged;
    assert_eq!(
        configure_acp_session(
            &mut writer,
            &pending,
            AcpAgent::Kimi,
            "kimi-session",
            "another-model",
            None,
            &config
        )
        .unwrap()
        .as_deref(),
        Some("another-model")
    );
    let request = writer.frames.last().unwrap();
    assert_eq!(request["method"], "session/set_config_option");
    assert_eq!(
        request["params"],
        json!({"sessionId":"kimi-session", "configId":"model", "value":"another-model"})
    );

    let approval = AcpPendingApproval {
        allow_once_option_id: Some("once".into()),
        allow_always_option_id: Some("always".into()),
        reject_option_id: Some("reject".into()),
        request_id: json!(7),
    };
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    assert_eq!(
        acp_permission_response_option_id(AcpAgent::Kimi, &state, &session_id, &approval).unwrap(),
        None
    );
    assert!(pending.lock().unwrap().is_empty());
    assert!(
        configure_acp_session(
            &mut writer,
            &pending,
            AcpAgent::Kimi,
            "kimi-session",
            "unknown-model",
            None,
            &config,
        )
        .is_err()
    );
    writer.reply = config.clone();
    assert!(
        configure_acp_session(
            &mut writer,
            &pending,
            AcpAgent::Kimi,
            "kimi-session",
            "another-model",
            None,
            &config,
        )
        .is_err(),
        "a contradictory model ACK must refuse the turn"
    );
}

#[test]
fn kimi_manual_mode_requires_explicit_acknowledgment() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = KimiReplyWriter {
        pending: pending.clone(),
        buffer: vec![],
        frames: vec![],
        reply: json!({}),
        error: None,
    };
    for value in [json!(null), json!("auto"), json!("yolo"), json!("plan")] {
        writer.reply = json!({"configOptions":[{"id":"mode", "currentValue":value}]});
        assert!(configure_kimi_manual_approvals(&mut writer, &pending, "saved-session").is_err());
    }
    writer.reply = json!({"configOptions":[{"id":"mode", "currentValue":"default"}]});
    configure_kimi_manual_approvals(&mut writer, &pending, "saved-session").unwrap();
    assert_eq!(
        writer.frames.last().unwrap()["params"],
        json!({"sessionId":"saved-session", "configId":"mode", "value":"default"})
    );
    assert!(is_acp_config_update_kind("config_option_update", AcpAgent::Kimi));
}

#[tokio::test]
async fn kimi_read_only_and_reviewer_delegations_refuse_before_child_creation() {
    let tools = mcp_tools_list_result();
    let spawn = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "termal_spawn_session")
        .unwrap();
    assert!(
        spawn
            .pointer("/inputSchema/properties/agent/enum")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("Kimi"))
    );
    for mode in ["explorer", "reviewer"] {
        let state = test_app_state();
        let parent = test_session_id(&state, Agent::Codex);
        let before = state.inner.lock().unwrap().sessions.len();
        let app = app_router(state.clone());
        let (status, response): (StatusCode, ErrorResponse) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{parent}/delegations"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "prompt":"Inspect task", "agent":"Kimi", "mode":mode,
                        "writePolicy":{"kind":"readOnly"}
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response.error.contains(if mode == "explorer" {
            "Kimi delegations do not support"
        } else {
            "reviewer"
        }));
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions.len(), before);
        assert!(inner.delegations.is_empty());
    }
}

#[test]
fn kimi_model_settings_persist_and_preserve_continuation_for_restart() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    state
        .set_external_session_id(&session_id, "saved-kimi-session".to_owned())
        .unwrap();
    let request = serde_json::from_value(json!({"model":"kimi-code/k3"})).unwrap();
    state.update_session_settings(&session_id, request).unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = inner
            .sessions
            .iter()
            .find(|r| r.session.id == session_id)
            .unwrap();
        assert_eq!(record.session.model, "kimi-code/k3");
        assert!(record.runtime_reset_required);
        assert_eq!(
            record.external_session_id.as_deref(),
            Some("saved-kimi-session")
        );
    }
    let reloaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        reloaded
            .sessions
            .iter()
            .find(|r| r.session.id == session_id)
            .unwrap()
            .session
            .model,
        "kimi-code/k3"
    );
    for setting in [
        json!({"cursorMode":"plan"}),
        json!({"geminiApprovalMode":"yolo"}),
        json!({"opencodeApprovalMode":"auto-approve"}),
    ] {
        let request = serde_json::from_value(setting).unwrap();
        assert_eq!(
            state
                .update_session_settings(&session_id, request)
                .err()
                .unwrap()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .sessions
            .iter_mut()
            .find(|r| r.session.id == session_id)
            .unwrap()
            .session
            .status = SessionStatus::Active;
        state.commit_locked(&mut inner).unwrap();
    }
    let request = serde_json::from_value(json!({"model":"other-model"})).unwrap();
    assert_eq!(
        state
            .update_session_settings(&session_id, request)
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn kimi_creation_rejects_unsupported_constraints_without_allocating_session() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let before = state.inner.lock().unwrap().sessions.len();
    for (field, value) in [
        ("sandboxMode", "read-only"),
        ("approvalPolicy", "on-request"),
        ("reasoningEffort", "high"),
        ("cursorMode", "plan"),
        ("claudeApprovalMode", "ask"),
        ("claudeEffort", "high"),
        ("geminiApprovalMode", "yolo"),
        ("opencodeApprovalMode", "auto-approve"),
    ] {
        let mut body = json!({"agent":"Kimi"});
        body[field] = json!(value);
        let (status, response): (StatusCode, ErrorResponse) = request_json(
            &app,
            Request::builder().method("POST").uri("/api/sessions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap(),
        ).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {}", response.error);
        assert!(response.error.contains(if field == "opencodeApprovalMode" {
            "only supported by OpenCode"
        } else { "Kimi sessions only support model" }), "{field}: {}", response.error);
        assert_eq!(state.inner.lock().unwrap().sessions.len(), before);
    }
}

#[test]
fn kimi_repeated_refresh_reconnects_and_preserves_external_conversation() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    state.set_external_session_id(&session_id, "saved-kimi-refresh".to_owned()).unwrap();
    for revision in 1..=2 {
        let runtime_id = format!("kimi-refresh-{revision}");
        let (runtime, input_rx) = test_acp_runtime_handle(AcpAgent::Kimi, &runtime_id);
        state.install_test_acp_runtime_override(AcpAgent::Kimi, runtime);
        let responder_state = state.clone();
        let responder_session = session_id.clone();
        let model = format!("model-{revision}");
        let expected = model.clone();
        let responder = std::thread::spawn(move || {
            match recv_within_guard(&input_rx, "Kimi fresh config handshake").unwrap() {
                AcpRuntimeCommand::RefreshSessionConfig { command, response_tx } => {
                    assert_eq!(command.resume_session_id.as_deref(), Some("saved-kimi-refresh"));
                    let config = json!({"configOptions":[{
                        "id":"model", "currentValue":model,
                        "options":[{"value":model,"name":model}]
                    }]});
                    responder_state.sync_session_model_options(
                        &responder_session, Some(model), acp_model_options(&config, AcpAgent::Kimi),
                    ).unwrap();
                    response_tx.send(Ok(())).unwrap();
                }
                _ => panic!("refresh must use a fresh handshake"),
            }
        });
        state.refresh_session_model_options(&session_id).unwrap();
        responder.join().unwrap();
        let inner = state.inner.lock().unwrap();
        let record = inner.sessions.iter().find(|r| r.session.id == session_id).unwrap();
        assert_eq!(record.session.model_options[0].value, expected);
        assert_eq!(record.external_session_id.as_deref(), Some("saved-kimi-refresh"));
        assert!(matches!(&record.runtime, SessionRuntime::Acp(h) if h.runtime_id == runtime_id));
    }
}

#[test]
fn kimi_partial_config_notifications_preserve_models_but_present_lists_replace_them() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    let (input_tx, _input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn = AcpTurnState::default();
    let full = json!({"sessionUpdate":"config_option_update", "configOptions":[{
        "id":"model", "currentValue":"kimi-code/k3",
        "options":[{"value":"kimi-code/k3","name":"K3"}]
    }]});
    for update in [full, json!({"sessionUpdate":"config_option_update","configOptions":[{
        "id":"mode", "currentValue":"default", "options":[]
    }]})] {
        handle_acp_session_update(&update, &state, &session_id, &input_tx, &mut turn,
            &mut recorder, AcpAgent::Kimi).unwrap();
        let inner = state.inner.lock().unwrap();
        let session = &inner.sessions.iter().find(|r| r.session.id == session_id).unwrap().session;
        assert_eq!(session.model, "auto", "runtime reports must preserve requested Auto");
        assert_eq!(session.model_options.len(), 1);
    }
    handle_acp_session_update(
        &json!({"sessionUpdate":"config_option_update","configOptions":[{"id":"model","options":[]}]}),
        &state, &session_id, &input_tx, &mut turn, &mut recorder, AcpAgent::Kimi,
    ).unwrap();
    assert!(state.inner.lock().unwrap().sessions.iter()
        .find(|r| r.session.id == session_id).unwrap().session.model_options.is_empty());
}

#[test]
fn kimi_user_stop_cancels_before_process_teardown() {
    let fixture = phase_sync::ParkedProcess::spawn();
    let process = fixture.process.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let lifecycle: AcpTurnLifecycle = Arc::new((Mutex::new(true), Condvar::new()));
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::Kimi, runtime_id: "kimi-stop".to_owned(), input_tx,
        process: process.clone(), turn_lifecycle: lifecycle.clone(),
    };
    let responder = std::thread::spawn(move || {
        assert!(matches!(recv_within_guard(&input_rx, "Kimi cancel before kill").unwrap(), AcpRuntimeCommand::Cancel));
        set_acp_turn_active(&lifecycle, false);
    });
    shutdown_stopped_runtime(KillableRuntime::Acp(runtime), "Kimi Stop regression").unwrap();
    responder.join().unwrap();
    phase_sync::process_exit(&process, "Kimi cancelled subprocess reaped");
}

#[test]
fn kimi_prompt_rechecks_manual_mode_and_deactivates_on_refusal() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    let runtime = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("saved-kimi".to_owned()), ..Default::default()
    }));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let lifecycle: AcpTurnLifecycle = Arc::new((Mutex::new(true), Condvar::new()));
    let mut writer = KimiReplyWriter {
        pending: pending.clone(), buffer: vec![], frames: vec![], error: None,
        reply: json!({"configOptions":[{"id":"mode","currentValue":"yolo"}]}),
    };
    for _ in 0..2 {
        set_acp_turn_active(&lifecycle, true);
        let err = handle_acp_prompt_command(
            &mut writer, &pending, &state, &session_id, &runtime, &lifecycle,
            &RuntimeToken::Acp("kimi-mode-test".to_owned()), None, AcpAgent::Kimi,
            AcpPromptCommand { cwd: state.default_workdir.clone(), cursor_mode: None,
                model: "auto".to_owned(), opencode_effort: None, opencode_mode: None,
                prompt: "Never sent".to_owned(), resume_session_id: Some("saved-kimi".to_owned()) },
        ).unwrap_err();
        assert!(err.to_string().contains("manual"));
        assert!(!*lifecycle.0.lock().unwrap());
    }
    assert_eq!(writer.frames.len(), 2);
    assert!(writer.frames.iter().all(|f| f["method"] == "session/set_config_option"));
    writer.reply = json!({"configOptions":[{"id":"mode","currentValue":"default"}],
        "stopReason":"end_turn"});
    for _ in 0..2 {
        let before = writer.frames.len();
        handle_acp_prompt_command(
            &mut writer, &pending, &state, &session_id, &runtime, &lifecycle,
            &RuntimeToken::Acp("kimi-mode-test".to_owned()), None, AcpAgent::Kimi,
            AcpPromptCommand { cwd: state.default_workdir.clone(), cursor_mode: None,
                model: "auto".to_owned(), opencode_effort: None, opencode_mode: None,
                prompt: "Scripted prompt".to_owned(), resume_session_id: Some("saved-kimi".to_owned()) },
        ).unwrap();
        assert_eq!(writer.frames[before]["method"], "session/set_config_option");
        assert_eq!(writer.frames[before + 1]["method"], "session/prompt");
        let (active, _) = lifecycle.1.wait_timeout_while(
            lifecycle.0.lock().unwrap(), phase_sync::DEADLOCK_GUARD, |active| *active,
        ).unwrap();
        assert!(!*active, "scripted prompt must settle");
    }
}

#[test]
fn kimi_auth_failure_has_login_remedy_and_model_failure_cannot_cache_ready_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = KimiReplyWriter { pending: pending.clone(), buffer: vec![], frames: vec![],
        reply: json!({}), error: Some("credential refused".to_owned()) };
    let err = maybe_authenticate_acp_runtime(&mut writer, &pending,
        &json!({"authMethods":[{"id":"login"}]}), AcpAgent::Kimi, &state.default_workdir).unwrap_err();
    assert!(format!("{err:#}").contains("kimi login"));
    writer.error = None;
    // A nonconforming continuation without advertised model options must fail
    // closed, not silently substitute the CLI default. Actual 2.0.2 resumes
    // and loads include the full configOptions array (see feature brief).
    for resume in [true, false] {
        let runtime = Arc::new(Mutex::new(AcpRuntimeState {
            capabilities: Some(AcpCapabilities { supports_session_resume: Some(resume),
                supports_session_load: Some(true) }), ..Default::default()
        }));
        let command = AcpPromptCommand { cwd: state.default_workdir.clone(), cursor_mode: None,
            model: "kimi-code/k3".to_owned(), opencode_effort: None, opencode_mode: None,
            prompt: String::new(), resume_session_id: Some("saved-kimi".to_owned()) };
        for _ in 0..2 {
            let err = ensure_acp_session_ready_inner(&mut writer, &pending, &state, &session_id,
                &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi, &command,
                AcpSessionPurpose::Prompt).unwrap_err();
            assert!(err.to_string().contains("did not advertise requested model"));
            assert!(runtime.lock().unwrap().current_session_id.is_none());
        }
        // Observed 2.0.2 continuation shape: configOptions on both resume/load.
        writer.reply = json!({"configOptions":[{
            "id":"model", "currentValue":"kimi-code/k3",
            "options":[{"value":"kimi-code/k3","name":"K3"}]
        }], "modes":{"currentModeId":"default"}});
        ensure_acp_session_ready_inner(&mut writer, &pending, &state, &session_id,
            &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi, &command,
            AcpSessionPurpose::Prompt).unwrap();
        assert_eq!(runtime.lock().unwrap().current_session_id.as_deref(), Some("saved-kimi"));
        writer.reply = json!({});
    }
}

#[test]
fn kimi_orchestrator_auto_approval_is_rejected_explicitly() {
    let template = OrchestratorSessionTemplate {
        id: "kimi".to_owned(), name: "Kimi".to_owned(), agent: Agent::Kimi,
        model: None, instructions: String::new(), auto_approve: true,
        input_mode: OrchestratorSessionInputMode::Queue,
        position: OrchestratorNodePosition { x: 0.0, y: 0.0 },
    };
    let error = normalize_orchestrator_session_template(template.clone()).unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("manual tool approvals"));
    assert!(validate_orchestrator_manual_approval_policy(&template).is_err());
    assert!(normalize_orchestrator_session_template(OrchestratorSessionTemplate {
        auto_approve: false, ..template
    }).is_ok());
}

fn kimi_model_catalog(current: &str) -> Value {
    json!({"sessionId":"saved-kimi-catalog", "configOptions":[{
        "id":"model", "currentValue":current, "options":[
            {"value":"model-a", "name":"Model A"},
            {"value":"model-b", "name":"Model B"}
        ]
    }]})
}

fn kimi_test_command(state: &AppState, model: &str, resume: bool) -> AcpPromptCommand {
    AcpPromptCommand {
        cwd: state.default_workdir.clone(), cursor_mode: None, model: model.to_owned(),
        opencode_effort: None, opencode_mode: None, prompt: String::new(),
        resume_session_id: resume.then(|| "saved-kimi-catalog".to_owned()),
    }
}

fn kimi_test_runtime() -> Arc<Mutex<AcpRuntimeState>> {
    Arc::new(Mutex::new(AcpRuntimeState {
        capabilities: Some(AcpCapabilities {
            supports_session_load: Some(true), supports_session_resume: Some(true),
        }), ..Default::default()
    }))
}

#[test]
fn kimi_discovery_recovers_invalid_initial_and_removed_models_without_admitting_prompts() {
    for (model, resume) in [("typo-model", false), ("removed-model", true)] {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Kimi);
        state.update_session_settings(&session_id,
            serde_json::from_value(json!({"model":model})).unwrap()).unwrap();
        if resume {
            state.set_external_session_id(&session_id, "saved-kimi-catalog".to_owned()).unwrap();
            state.sync_session_model_options(&session_id, None,
                vec![SessionModelOption::plain("Removed model", model)]).unwrap();
        }
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut writer = KimiReplyWriter { pending: pending.clone(), buffer: vec![], frames: vec![],
            reply: kimi_model_catalog("model-a"), error: None };
        let runtime = kimi_test_runtime();
        handle_acp_session_config_refresh_inner(&mut writer, &pending, &state, &session_id,
            &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi,
            kimi_test_command(&state, model, resume)).unwrap();
        assert!(runtime.lock().unwrap().current_session_id.is_none(), "discovery is not prompt admission");
        assert_eq!(writer.frames.len(), 1, "discovery must not apply the invalid selection");
        {
            let inner = state.inner.lock().unwrap();
            let record = inner.sessions.iter().find(|r| r.session.id == session_id).unwrap();
            assert_eq!(record.session.model, model);
            assert_eq!(record.session.model_options.len(), 2);
            assert_eq!(record.session.model_options[0].value, "model-a");
        }
        let error = ensure_acp_session_ready_inner(&mut writer, &pending, &state, &session_id,
            &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi,
            &kimi_test_command(&state, model, true), AcpSessionPurpose::Prompt).unwrap_err();
        assert!(error.to_string().contains("did not advertise requested model"));
        assert!(runtime.lock().unwrap().current_session_id.is_none());
        state.update_session_settings(&session_id,
            serde_json::from_value(json!({"model":"model-a"})).unwrap()).unwrap();
        ensure_acp_session_ready_inner(&mut writer, &pending, &state, &session_id,
            &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi,
            &kimi_test_command(&state, "model-a", true), AcpSessionPurpose::Prompt).unwrap();
        assert_eq!(runtime.lock().unwrap().current_session_id.as_deref(), Some("saved-kimi-catalog"));
    }
}

// Pauses one RPC response at a deterministic boundary, then supplies per-RPC
// replies. The shared deadlock guard bounds failed handshakes without sleeps.
struct KimiSequencedWriter {
    inner: KimiReplyWriter,
    replies: VecDeque<Value>,
    before_reply: Option<Box<dyn FnOnce() + Send>>,
}

impl Write for KimiSequencedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.inner.write(bytes) }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(before_reply) = self.before_reply.take() { before_reply(); }
        self.inner.reply = self.replies.pop_front().expect("scripted RPC reply");
        self.inner.flush()
    }
}

#[test]
fn kimi_refresh_and_notifications_cannot_overwrite_a_concurrent_model_patch() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Kimi);
    state.update_session_settings(&session_id,
        serde_json::from_value(json!({"model":"model-a"})).unwrap()).unwrap();
    state.set_external_session_id(&session_id, "saved-kimi-catalog".to_owned()).unwrap();
    let (runtime, input_rx) = test_acp_runtime_handle(AcpAgent::Kimi, "kimi-interleaved-refresh");
    state.install_test_acp_runtime_override(AcpAgent::Kimi, runtime);
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker_state = state.clone();
    let worker_session = session_id.clone();
    let worker = std::thread::spawn(move || {
        let AcpRuntimeCommand::RefreshSessionConfig { command, response_tx } =
            recv_within_guard(&input_rx, "Kimi refresh command").unwrap()
        else { panic!("expected refresh command") };
        assert_eq!(command.model, "model-a");
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let runtime = kimi_test_runtime();
        let mut writer = KimiSequencedWriter {
            inner: KimiReplyWriter { pending: pending.clone(), buffer: vec![], frames: vec![],
                reply: json!({}), error: None },
            replies: VecDeque::from([kimi_model_catalog("model-a")]),
            before_reply: Some(Box::new(move || {
                paused_tx.send(()).unwrap();
                recv_within_guard(&release_rx, "release Kimi refresh response").unwrap();
            })),
        };
        handle_acp_session_config_refresh_inner(&mut writer, &pending, &worker_state,
            &worker_session, &runtime, AcpEngramMcpSource::LiveState, AcpAgent::Kimi, command).unwrap();
        // A late notification from the same discovery must not undo the PATCH either.
        let mut update = kimi_model_catalog("model-a");
        update["sessionUpdate"] = json!("config_option_update");
        let (tx, _rx) = mpsc::channel();
        handle_acp_session_update(&update, &worker_state, &worker_session, &tx,
            &mut AcpTurnState::default(), &mut SessionRecorder::new(worker_state.clone(), worker_session.clone()),
            AcpAgent::Kimi).unwrap();
        response_tx.send(Ok(())).unwrap();
    });
    let refresh_state = state.clone();
    let refresh_session = session_id.clone();
    let refresh = std::thread::spawn(move || refresh_state.refresh_session_model_options(&refresh_session));
    recv_within_guard(&paused_rx, "Kimi response paused after snapshot of A").unwrap();
    state.update_session_settings(&session_id,
        serde_json::from_value(json!({"model":"model-b"})).unwrap()).unwrap();
    release_tx.send(()).unwrap();
    refresh.join().unwrap().unwrap();
    worker.join().unwrap();
    {
        let inner = state.inner.lock().unwrap();
        let record = inner.sessions.iter().find(|r| r.session.id == session_id).unwrap();
        assert_eq!(record.session.model, "model-b");
        assert!(record.runtime_reset_required);
    }
    // The fresh prompt handshake actually applies B rather than trusting the
    // provider's still-effective A or a ready-session cache left by discovery.
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = KimiSequencedWriter {
        inner: KimiReplyWriter { pending: pending.clone(), buffer: vec![], frames: vec![],
            reply: json!({}), error: None },
        replies: VecDeque::from([kimi_model_catalog("model-a"), kimi_model_catalog("model-b")]),
        before_reply: None,
    };
    ensure_acp_session_ready_inner(&mut writer, &pending, &state, &session_id,
        &kimi_test_runtime(), AcpEngramMcpSource::LiveState, AcpAgent::Kimi,
        &kimi_test_command(&state, "model-b", true), AcpSessionPurpose::Prompt).unwrap();
    assert_eq!(writer.inner.frames[1]["method"], "session/set_config_option");
    assert_eq!(writer.inner.frames[1]["params"]["value"], "model-b");
    assert_eq!(state.inner.lock().unwrap().sessions.iter().find(|r| r.session.id == session_id)
        .unwrap().session.model, "model-b");
}

#[test]
fn kimi_notification_extensions_do_not_change_existing_acp_adapters() {
    for (agent, acp) in [(Agent::Cursor, AcpAgent::Cursor), (Agent::Gemini, AcpAgent::Gemini),
        (Agent::OpenCode, AcpAgent::OpenCode)] {
        let state = test_app_state();
        let session_id = test_session_id(&state, agent);
        state.sync_session_model_options(&session_id, Some("model-a".to_owned()),
            acp_model_options(&kimi_model_catalog("model-a"), acp)).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut turn = AcpTurnState::default();
        let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
        let mut update = kimi_model_catalog("model-b");
        update["sessionUpdate"] = json!("config_option_update");
        handle_acp_session_update(&update, &state, &session_id, &tx, &mut turn, &mut recorder, acp).unwrap();
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)), "no OpenCode reconciliation queued");
        assert_eq!(state.inner.lock().unwrap().sessions.iter().find(|r| r.session.id == session_id)
            .unwrap().session.model, "model-a");
        if acp != AcpAgent::OpenCode {
            handle_acp_session_update(&json!({"sessionUpdate":"config_update", "configOptions":[
                {"id":"mode", "currentValue":"default"}
            ]}), &state, &session_id, &tx, &mut turn, &mut recorder, acp).unwrap();
            assert!(state.inner.lock().unwrap().sessions.iter().find(|r| r.session.id == session_id)
                .unwrap().session.model_options.is_empty(), "legacy partial update behavior unchanged");
        }
        if acp == AcpAgent::Cursor {
            let before = state.inner.lock().unwrap().sessions.iter().find(|r| r.session.id == session_id)
                .unwrap().session.cursor_mode;
            handle_acp_session_update(&json!({"sessionUpdate":"current_mode_update", "currentModeId":"plan"}),
                &state, &session_id, &tx, &mut turn, &mut recorder, acp).unwrap();
            assert_eq!(state.inner.lock().unwrap().sessions.iter().find(|r| r.session.id == session_id)
                .unwrap().session.cursor_mode, before);
        }
    }
}
