// Session and app-settings update flows across three tiers: global app
// settings (default model per agent, approval mode, effort) persisted and
// applied to freshly created sessions, project-level defaults inherited by
// new sessions, and per-session overrides applied via
// `update_session_settings`.
//
// Update semantics diverge by agent. Codex model + reasoning-effort swaps
// and Cursor mode changes take effect live on the running runtime, so
// `runtime_reset_required` stays false. Claude model + effort changes flip
// `runtime_reset_required` because the Claude CLI does not support hot
// reconfig — the next send restarts the child. Model swaps push a
// `SetModel` command to the live Claude process, but effort changes and
// the `default` sentinel never do.
//
// Codex reasoning-effort normalization guards a subtle edge: when the
// model changes, the current effort must be re-validated against the new
// model's `supported_reasoning_efforts`. Unsupported values fed directly
// in `update_session_settings` are rejected with a specific
// "does not support ... reasoning effort; choose ..." error.
//
// External session ID is independent of the ignored-thread list — setting
// a shared thread ID on a non-Codex session must not prune that thread
// from `ignored_discovered_codex_thread_ids`, and thread import prunes
// stale entries only.
//
// Production surfaces: `update_session_settings`, `update_app_settings`,
// `set_external_session_id`, `sync_claude_model_options_for_session`.

use super::*;

// Remote settings proxy the typed request instead of maintaining a parallel
// field allowlist. Pin the two OpenCode-dependent fields and the transport
// budget required by the acknowledged 55-second owner-side update path.
#[test]
fn remote_session_settings_payload_includes_agent_dependents_and_timeout_slack() {
    let payload = serde_json::to_value(UpdateSessionSettingsRequest {
        name: None,
        model: Some("provider/model".to_owned()),
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
        opencode_effort: Some("xhigh".to_owned()),
        opencode_mode: Some("build".to_owned()),
        codex_fast_mode: Some(true),
    })
    .expect("session settings request should serialize");

    assert_eq!(payload["opencodeEffort"], "xhigh");
    assert_eq!(payload["opencodeMode"], "build");
    assert_eq!(payload["codexFastMode"], true);
    assert!(REMOTE_SESSION_SETTINGS_TIMEOUT > Duration::from_secs(55));
}

// Pins the cross-language app-preference key. `OpenCode` contains an internal
// capital letter, so serde's generic camelCase conversion would otherwise
// produce `defaultOpencodeModel` while the TypeScript contract uses
// `defaultOpenCodeModel`.
#[test]
fn opencode_default_model_uses_explicit_wire_key() {
    let serialized =
        serde_json::to_value(AppPreferences::default()).expect("preferences should serialize");
    assert_eq!(
        serialized
            .get("defaultOpenCodeModel")
            .and_then(Value::as_str),
        Some("default")
    );
    assert!(
        serialized.get("defaultOpencodeModel").is_none(),
        "the accidental serde-derived spelling must not leak onto the wire"
    );

    let request: UpdateAppSettingsRequest = serde_json::from_value(json!({
        "defaultOpenCodeModel": "openai/gpt-5.6-sol"
    }))
    .expect("OpenCode preference request should deserialize");
    assert_eq!(
        request.default_opencode_model.as_deref(),
        Some("openai/gpt-5.6-sol")
    );
}

#[test]
fn non_opencode_sessions_reject_opencode_effort() {
    let cases = [
        (
            Agent::Codex,
            "Claude/OpenCode settings can only be changed for their matching sessions",
        ),
        (
            Agent::Claude,
            "Claude sessions only support model, mode, and effort settings",
        ),
        (
            Agent::Cursor,
            "Cursor sessions only support model and mode settings",
        ),
        (
            Agent::Gemini,
            "Gemini sessions only support model and approval mode settings",
        ),
    ];

    for (agent, expected_message) in cases {
        let state = test_app_state();
        let session_id = test_session_id(&state, agent);
        let error = match state.update_session_settings(
            &session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: Some("high".to_owned()),
                opencode_mode: None,
                codex_fast_mode: None,
            },
        ) {
            Ok(_) => panic!("{agent:?} should reject OpenCode reasoning variants"),
            Err(error) => error,
        };

        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{agent:?}");
        assert_eq!(error.message, expected_message, "{agent:?}");
    }
}

#[test]
fn opencode_model_refresh_restarts_ready_runtime_before_handshake() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::OpenCode);
    let (stale_runtime, _stale_input_rx) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "opencode-stale-refresh");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("OpenCode session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("OpenCode session index should be valid");
        record.session.status = SessionStatus::Idle;
        record.external_session_id = Some("opencode-external-refresh".to_owned());
        record.runtime = SessionRuntime::Acp(stale_runtime);
    }

    let (fresh_runtime, fresh_input_rx) =
        test_acp_runtime_handle(AcpAgent::OpenCode, "opencode-fresh-refresh");
    state.install_test_acp_runtime_override(AcpAgent::OpenCode, fresh_runtime);
    let responder = std::thread::spawn(move || {
        match fresh_input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh runtime should receive config refresh")
        {
            AcpRuntimeCommand::RefreshSessionConfig {
                command,
                response_tx,
            } => {
                assert_eq!(
                    command.resume_session_id.as_deref(),
                    Some("opencode-external-refresh"),
                    "refresh must preserve and resume the external conversation"
                );
                response_tx
                    .send(Ok(()))
                    .expect("refresh result receiver should remain live");
            }
            _ => panic!("expected ACP config refresh command"),
        }
    });

    state
        .refresh_session_model_options(&session_id)
        .expect("OpenCode refresh should use the fresh runtime handshake");
    responder.join().expect("refresh responder should finish");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("OpenCode session should remain present");
    assert!(
        matches!(
            &record.runtime,
            SessionRuntime::Acp(handle)
                if handle.runtime_id == "opencode-fresh-refresh"
        ),
        "the stale ready runtime must be replaced before refresh"
    );
}

#[test]
fn model_refresh_rejects_active_or_runtime_stop_owned_sessions_without_replacing_runtime() {
    for (agent, runtime) in [
        {
            let (runtime, _rx) = test_claude_runtime_handle("refresh-fence-claude");
            (Agent::Claude, SessionRuntime::Claude(runtime))
        },
        {
            let (runtime, _rx) = test_codex_runtime_handle("refresh-fence-codex");
            (Agent::Codex, SessionRuntime::Codex(runtime))
        },
        {
            let (runtime, _rx) = test_acp_runtime_handle(AcpAgent::Cursor, "refresh-fence-cursor");
            (Agent::Cursor, SessionRuntime::Acp(runtime))
        },
    ] {
        let state = test_app_state();
        let session_id = test_session_id(&state, agent);
        let token = runtime
            .runtime_token()
            .expect("test runtime should have a token");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("test session should exist");
            let record = inner
                .session_mut_by_index(index)
                .expect("test session index should be valid");
            record.runtime = runtime;
            record.session.status = SessionStatus::Active;
        }

        let active_error = match state.refresh_session_model_options(&session_id) {
            Ok(_) => panic!("active refresh must not replace a live runtime"),
            Err(error) => error,
        };
        assert_eq!(active_error.status, StatusCode::CONFLICT, "{agent:?}");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("test session should remain");
            let record = inner
                .session_mut_by_index(index)
                .expect("test session index should be valid");
            assert!(record.runtime.matches_runtime_token(&token), "{agent:?}");
            record.session.status = SessionStatus::Idle;
            record.claim_runtime_stop(RuntimeStopOwnerKind::EngramMcpRevocation, token.clone());
        }

        let fenced_error = match state.refresh_session_model_options(&session_id) {
            Ok(_) => panic!("owned runtime stop fence must block model refresh"),
            Err(error) => error,
        };
        assert_eq!(fenced_error.status, StatusCode::CONFLICT, "{agent:?}");
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("test session should remain");
        assert!(record.runtime.matches_runtime_token(&token), "{agent:?}");
        assert!(record.runtime_stop_in_progress, "{agent:?}");
    }
}

#[test]
fn model_refresh_clears_engram_quarantine_before_attaching_successor_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let (stale_runtime, _stale_input_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "quarantined-cursor-refresh-old");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Cursor session index should be valid");
        record.session.status = SessionStatus::Error;
        record.runtime = SessionRuntime::Acp(stale_runtime);
        record.runtime_reset_required = true;
        record.engram_mcp_runtime_quarantined = true;
        record.orchestrator_auto_dispatch_blocked = true;
    }

    let (fresh_runtime, fresh_input_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "quarantined-cursor-refresh-fresh");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, fresh_runtime);
    let responder = std::thread::spawn(move || {
        match fresh_input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("fresh Cursor runtime should receive config refresh")
        {
            AcpRuntimeCommand::RefreshSessionConfig { response_tx, .. } => response_tx
                .send(Ok(()))
                .expect("refresh result receiver should remain live"),
            _ => panic!("expected ACP config refresh command"),
        }
    });

    state
        .refresh_session_model_options(&session_id)
        .expect("model refresh should replace the quarantined runtime");
    responder.join().expect("refresh responder should finish");

    let successor_token = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .find_session_index(&session_id)
            .and_then(|index| inner.sessions.get(index))
            .expect("Cursor session should remain");
        assert!(matches!(
            &record.runtime,
            SessionRuntime::Acp(handle)
                if handle.runtime_id == "quarantined-cursor-refresh-fresh"
        ));
        assert!(!record.runtime_reset_required);
        assert!(!record.engram_mcp_runtime_quarantined);
        record
            .runtime
            .runtime_token()
            .expect("successor runtime should have a token")
    };

    state
        .handle_runtime_exit_if_matches(&session_id, &successor_token, None)
        .expect("successor runtime exit should reconcile normally");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("Cursor session should remain after successor exit");
    assert!(!record.orchestrator_auto_dispatch_blocked);
}

#[test]
fn offline_opencode_effort_and_mode_changes_do_not_claim_agent_effective_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::OpenCode);
    state
        .sync_session_opencode_config(
            &session_id,
            OpenCodeConfigUpdate {
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("medium".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Medium", "medium"),
                        SessionModelOption::plain("High", "high"),
                    ],
                }),
                mode: Some(OpenCodeConfigOptionUpdate {
                    selection: OPENCODE_CONFIG_AUTO.to_owned(),
                    current: Some("build".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Build", "build"),
                        SessionModelOption::plain("Plan", "plan"),
                    ],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("initial agent-reported mode should sync");

    let updated = state
        .update_session_settings(
            &session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: Some("high".to_owned()),
                opencode_mode: Some("plan".to_owned()),
                codex_fast_mode: None,
            },
        )
        .expect("offline OpenCode authority should update");
    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(session.opencode_mode.as_deref(), Some("plan"));
    assert_eq!(session.opencode_effort.as_deref(), Some("high"));
    assert_eq!(
        session.opencode_current_effort.as_deref(),
        Some("medium"),
        "offline authority changes must not impersonate agent acknowledgement"
    );
    assert_eq!(
        session.opencode_current_mode.as_deref(),
        Some("build"),
        "offline authority changes must not impersonate agent acknowledgement"
    );
}

#[test]
fn offline_opencode_model_and_effort_change_defers_effort_membership_validation() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::OpenCode);
    state
        .sync_session_opencode_config(
            &session_id,
            OpenCodeConfigUpdate {
                model: Some(OpenCodeConfigOptionUpdate {
                    selection: "openai/old-model".to_owned(),
                    current: Some("openai/old-model".to_owned()),
                    options: vec![
                        SessionModelOption::plain("Old model", "openai/old-model"),
                        SessionModelOption::plain("New model", "openai/new-model"),
                    ],
                }),
                effort: Some(OpenCodeConfigOptionUpdate {
                    selection: "low".to_owned(),
                    current: Some("low".to_owned()),
                    options: vec![SessionModelOption::plain("Low", "low")],
                }),
                ..OpenCodeConfigUpdate::default()
            },
        )
        .expect("initial model-specific OpenCode options should sync");

    let updated = state
        .update_session_settings(
            &session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("openai/new-model".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: Some("xhigh".to_owned()),
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .expect("new-model effort must not be rejected against old-model options");
    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("OpenCode session should remain visible");
    assert_eq!(session.model, "openai/new-model");
    assert_eq!(session.opencode_model.as_deref(), Some("openai/new-model"));
    assert_eq!(session.opencode_effort.as_deref(), Some("xhigh"));
    assert_eq!(
        session.opencode_current_effort.as_deref(),
        Some("low"),
        "offline authority changes must preserve the last agent-effective effort"
    );
}

// pins that attaching a shared thread ID to a non-Codex (Cursor) session
// leaves the Codex-ignored-thread entry untouched. guards against
// set_external_session_id cross-contaminating the ignored-threads list
// when a non-Codex session happens to reuse the same external ID.
#[test]
fn setting_non_codex_external_session_id_does_not_clear_ignored_codex_thread() {
    let state = test_app_state();
    let killed_codex_session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&killed_codex_session_id, "thread-shared".to_owned())
        .unwrap();
    state.kill_session(&killed_codex_session_id).unwrap();

    let cursor_session_id = test_session_id(&state, Agent::Cursor);
    state
        .set_external_session_id(&cursor_session_id, "thread-shared".to_owned())
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-shared")
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins that import_discovered_codex_threads retains ignored IDs that
// still appear in the discovered list and drops stale ones. guards
// against ignored-thread entries accumulating forever after the
// underlying threads have been deleted from disk.
#[test]
fn import_discovered_codex_threads_prunes_stale_ignored_thread_ids() {
    let mut inner = StateInner::new();
    inner
        .ignored_discovered_codex_thread_ids
        .extend(["thread-live".to_owned(), "thread-stale".to_owned()]);

    inner.import_discovered_codex_threads(
        "/tmp/termal",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/termal".to_owned(),
            id: "thread-live".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Still around".to_owned(),
        }],
    );

    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-live")
    );
    assert!(
        !inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-stale")
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-live"))
    );
}

// pins that suppress_orphaned_codex_thread ignores a newly-created but
// unbindable Codex thread so the startup discovery scan does not re-import
// it as a fresh top-level session. guards the delegation-child re-import
// leak: a fast-reaped read-only reviewer child orphans its thread when the
// async thread/start binding lands after the session is gone, and without
// this suppression `import_discovered_codex_threads` would mint a visible,
// unlinked session for the orphan on the next boot.
#[test]
fn suppress_orphaned_codex_thread_blocks_discovery_reimport() {
    let state = test_app_state();
    state
        .suppress_orphaned_codex_thread("thread-orphan")
        .unwrap();

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner.import_discovered_codex_threads(
        "/tmp/termal-orphan",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp/termal-orphan".to_owned(),
            id: "thread-orphan".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Orphaned reviewer".to_owned(),
        }],
    );

    assert!(
        inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-orphan"),
        "suppressed thread must stay ignored while it is still discovered"
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-orphan")),
        "orphaned thread must not be re-imported as a session"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins that update_app_settings writes the global preferences to disk
// and that a freshly loaded AppState inherits those defaults when
// creating new Codex, Claude, Cursor, and Gemini sessions. guards against app-settings
// persistence regressions and newly created sessions ignoring the
// configured global defaults.
#[test]
fn persists_app_settings_and_applies_them_to_new_sessions() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some("gpt-5.5".to_owned()),
            default_claude_model: Some("claude-sonnet-4-5".to_owned()),
            default_cursor_model: Some("cursor-premium".to_owned()),
            default_gemini_model: Some("gemini-2.5-pro".to_owned()),
            default_opencode_model: None,
            default_codex_reasoning_effort: Some(CodexReasoningEffort::High),
            default_codex_sandbox_mode: Some(CodexSandboxMode::DangerFullAccess),
            default_codex_approval_policy: Some(CodexApprovalPolicy::OnRequest),
            default_claude_approval_mode: Some(ClaudeApprovalMode::AutoApprove),
            default_claude_effort: Some(ClaudeEffortLevel::Max),
            remotes: None,
        })
        .unwrap();

    assert_eq!(
        updated.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        updated.preferences.default_codex_sandbox_mode,
        CodexSandboxMode::DangerFullAccess
    );
    assert_eq!(
        updated.preferences.default_codex_approval_policy,
        CodexApprovalPolicy::OnRequest
    );
    assert_eq!(updated.preferences.default_codex_model, "gpt-5.5");
    assert_eq!(
        updated.preferences.default_claude_model,
        "claude-sonnet-4-5"
    );
    assert_eq!(updated.preferences.default_cursor_model, "cursor-premium");
    assert_eq!(updated.preferences.default_gemini_model, "gemini-2.5-pro");
    assert_eq!(
        updated.preferences.default_claude_approval_mode,
        ClaudeApprovalMode::AutoApprove
    );
    assert_eq!(
        updated.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(updated.preferences.remotes, default_remote_configs());

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(
        reloaded_inner.preferences.default_codex_reasoning_effort,
        CodexReasoningEffort::High
    );
    assert_eq!(
        reloaded_inner.preferences.default_codex_sandbox_mode,
        CodexSandboxMode::DangerFullAccess
    );
    assert_eq!(
        reloaded_inner.preferences.default_codex_approval_policy,
        CodexApprovalPolicy::OnRequest
    );
    assert_eq!(reloaded_inner.preferences.default_codex_model, "gpt-5.5");
    assert_eq!(
        reloaded_inner.preferences.default_claude_model,
        "claude-sonnet-4-5"
    );
    assert_eq!(
        reloaded_inner.preferences.default_cursor_model,
        "cursor-premium"
    );
    assert_eq!(
        reloaded_inner.preferences.default_gemini_model,
        "gemini-2.5-pro"
    );
    assert_eq!(
        reloaded_inner.preferences.default_claude_approval_mode,
        ClaudeApprovalMode::AutoApprove
    );
    assert_eq!(
        reloaded_inner.preferences.default_claude_effort,
        ClaudeEffortLevel::Max
    );
    assert_eq!(reloaded_inner.preferences.remotes, default_remote_configs());

    let reloaded_state = AppState {
        server_instance_id: state.server_instance_id.clone(),
        default_workdir: "/tmp".to_owned(),
        local_http_base_url: state.local_http_base_url.clone(),
        persistence_path: state.persistence_path.clone(),
        mailbox_store: state.mailbox_store.clone(),
        coordination_board_store: state.coordination_board_store.clone(),
        orchestrator_templates_path: state.orchestrator_templates_path.clone(),
        orchestrator_templates_lock: state.orchestrator_templates_lock.clone(),
        review_documents_lock: state.review_documents_lock.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx: mpsc::channel().0,
        persist_thread_handle: Arc::new(Mutex::new(None)),
        persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
        state_broadcast_mailbox: None,
        telegram_relay_runtime: Arc::new(Mutex::new(TelegramRelayRuntime::default())),
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        shared_codex_exit_claims: Arc::new(Mutex::new(HashSet::new())),
        agent_runtime_spawning_enabled: false,
        test_acp_runtime_overrides: Arc::new(Mutex::new(Vec::new())),
        test_agent_setup_failures: Arc::new(Mutex::new(Vec::new())),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
        remote_lifecycle_actions_in_flight: Arc::new(Mutex::new(HashSet::new())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(StateMutex::new(reloaded_inner)),
        test_temp_root: state.test_temp_root.clone(),
    };

    let codex_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Persisted Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let codex_session = &codex_created.session;
    assert_eq!(
        codex_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );
    assert_eq!(
        codex_session.sandbox_mode,
        Some(CodexSandboxMode::DangerFullAccess)
    );
    assert_eq!(
        codex_session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(codex_session.model, "gpt-5.5");

    let overridden_codex = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Explicit Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Low),
            sandbox_mode: Some(CodexSandboxMode::ReadOnly),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    assert_eq!(
        overridden_codex.session.approval_policy,
        Some(CodexApprovalPolicy::Never)
    );
    assert_eq!(
        overridden_codex.session.reasoning_effort,
        Some(CodexReasoningEffort::Low)
    );
    assert_eq!(
        overridden_codex.session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );

    let claude_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Persisted Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let claude_session = &claude_created.session;
    assert_eq!(
        claude_session.claude_approval_mode,
        Some(ClaudeApprovalMode::AutoApprove)
    );
    assert_eq!(claude_session.claude_effort, Some(ClaudeEffortLevel::Max));
    assert_eq!(claude_session.model, "claude-sonnet-4-5");

    let cursor_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Persisted Cursor".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    assert_eq!(cursor_created.session.model, "cursor-premium");

    let gemini_created = reloaded_state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Gemini),
            name: Some("Persisted Gemini".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    assert_eq!(gemini_created.session.model, "gemini-2.5-pro");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn default_model_preference_canonicalizes_default_sentinel_case() {
    let state = test_app_state();

    state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some("gpt-5.5".to_owned()),
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
            default_opencode_model: None,
            default_codex_reasoning_effort: None,
            default_codex_sandbox_mode: None,
            default_codex_approval_policy: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .unwrap();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some(" DEFAULT ".to_owned()),
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
            default_opencode_model: None,
            default_codex_reasoning_effort: None,
            default_codex_sandbox_mode: None,
            default_codex_approval_policy: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .unwrap();

    assert_eq!(updated.preferences.default_codex_model, "default");
    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert_eq!(reloaded_inner.preferences.default_codex_model, "default");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Default Sentinel".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(created.session.model, Agent::Codex.default_model());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn default_model_preference_validation_covers_agents_and_boundaries() {
    for agent in [Agent::Codex, Agent::Claude, Agent::Cursor, Agent::Gemini] {
        let state = test_app_state();
        let boundary_model = "m".repeat(MAX_DEFAULT_MODEL_CHARS);
        let updated = state
            .update_app_settings(update_app_settings_request_for_agent_model(
                agent,
                boundary_model.clone(),
            ))
            .unwrap();
        assert_eq!(
            default_model_preference_for_agent(&updated.preferences, agent),
            boundary_model
        );

        let unicode_model = "é".repeat(MAX_DEFAULT_MODEL_CHARS);
        let updated = state
            .update_app_settings(update_app_settings_request_for_agent_model(
                agent,
                unicode_model.clone(),
            ))
            .unwrap();
        assert_eq!(
            default_model_preference_for_agent(&updated.preferences, agent),
            unicode_model
        );

        let error = match state.update_app_settings(update_app_settings_request_for_agent_model(
            agent,
            "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1),
        )) {
            Ok(_) => panic!("oversized {agent:?} default model should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            format!(
                "{} default model must be at most {} characters",
                agent.name(),
                MAX_DEFAULT_MODEL_CHARS
            )
        );

        let blank = state
            .update_app_settings(update_app_settings_request_for_agent_model(
                agent,
                "   ".to_owned(),
            ))
            .unwrap();
        assert_eq!(
            default_model_preference_for_agent(&blank.preferences, agent),
            "default"
        );

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn claude_default_model_preference_rejects_cli_option_like_values() {
    for (model, expected_message) in [
        (
            "--help".to_owned(),
            "Claude default model must not start with `-`",
        ),
        (
            "claude\nsonnet".to_owned(),
            "Claude default model must not contain control characters",
        ),
    ] {
        let state = test_app_state();
        let error = match state.update_app_settings(update_app_settings_request_for_agent_model(
            Agent::Claude,
            model,
        )) {
            Ok(_) => panic!("unsafe Claude default model should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, expected_message);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn opencode_model_ingress_rejects_values_that_cannot_round_trip_persistence() {
    for model in [
        "openai/gpt-5.6-sol\nshadow".to_owned(),
        "x".repeat(MAX_OPENCODE_MODEL_CHARS + 1),
    ] {
        let state = test_app_state();
        let create_error = match state.create_session(CreateSessionRequest {
            agent: Some(Agent::OpenCode),
            name: Some("Unsafe OpenCode model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some(model.clone()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        }) {
            Ok(_) => panic!("unsafe OpenCode create model should be rejected"),
            Err(error) => error,
        };
        assert_eq!(create_error.status, StatusCode::BAD_REQUEST);

        let default_error = match state.update_app_settings(
            update_app_settings_request_for_agent_model(Agent::OpenCode, model),
        ) {
            Ok(_) => panic!("unsafe OpenCode default model should be rejected"),
            Err(error) => error,
        };
        assert_eq!(default_error.status, StatusCode::BAD_REQUEST);
    }
}

fn update_app_settings_request_for_agent_model(
    agent: Agent,
    model: String,
) -> UpdateAppSettingsRequest {
    UpdateAppSettingsRequest {
        default_codex_model: (agent == Agent::Codex).then(|| model.clone()),
        default_claude_model: (agent == Agent::Claude).then(|| model.clone()),
        default_cursor_model: (agent == Agent::Cursor).then(|| model.clone()),
        default_gemini_model: (agent == Agent::Gemini).then(|| model.clone()),
        default_opencode_model: (agent == Agent::OpenCode).then_some(model),
        default_codex_reasoning_effort: None,
        default_codex_sandbox_mode: None,
        default_codex_approval_policy: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: None,
    }
}

fn default_model_preference_for_agent(preferences: &AppPreferences, agent: Agent) -> &str {
    match agent {
        Agent::Codex => &preferences.default_codex_model,
        Agent::Claude => &preferences.default_claude_model,
        Agent::Cursor => &preferences.default_cursor_model,
        Agent::Gemini => &preferences.default_gemini_model,
        Agent::OpenCode => &preferences.default_opencode_model,
    }
}

#[test]
fn oversized_persisted_default_model_falls_back_to_agent_default() {
    for agent in [
        Agent::Codex,
        Agent::Claude,
        Agent::Cursor,
        Agent::Gemini,
        Agent::OpenCode,
    ] {
        let state = test_app_state();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            match agent {
                Agent::Codex => {
                    inner.preferences.default_codex_model = "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1);
                }
                Agent::Claude => {
                    inner.preferences.default_claude_model =
                        "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1);
                }
                Agent::Cursor => {
                    inner.preferences.default_cursor_model =
                        "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1);
                }
                Agent::Gemini => {
                    inner.preferences.default_gemini_model =
                        "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1);
                }
                Agent::OpenCode => {
                    inner.preferences.default_opencode_model =
                        "x".repeat(MAX_DEFAULT_MODEL_CHARS + 1);
                }
            }
            state.commit_locked(&mut inner).unwrap();
        }

        let reloaded_inner = load_state(state.persistence_path.as_path())
            .unwrap()
            .expect("persisted state should exist");
        assert_eq!(
            default_model_preference_for_agent(&reloaded_inner.preferences, agent)
                .chars()
                .count(),
            MAX_DEFAULT_MODEL_CHARS + 1,
            "{agent:?}"
        );
        assert_eq!(
            reloaded_inner.preferences.default_model_for_agent(agent),
            agent.default_model(),
            "{agent:?}"
        );

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn unsafe_persisted_claude_default_model_falls_back_to_agent_default() {
    for model in ["--help", "claude\nsonnet"] {
        let state = test_app_state();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            inner.preferences.default_claude_model = model.to_owned();
            state.commit_locked(&mut inner).unwrap();
        }

        let reloaded_inner = load_state(state.persistence_path.as_path())
            .unwrap()
            .expect("persisted state should exist");
        assert_eq!(reloaded_inner.preferences.default_claude_model, model);
        assert_eq!(
            reloaded_inner
                .preferences
                .default_model_for_agent(Agent::Claude),
            Agent::Claude.default_model()
        );

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

// pins that a CreateSessionRequest carrying explicit Codex model,
// approval policy, reasoning effort, and sandbox mode stores those
// overrides verbatim on both the returned Session and the backing
// record. guards against create_session silently discarding
// caller-supplied prompt defaults in favour of global preferences.
#[test]
fn creates_codex_sessions_with_requested_prompt_defaults() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Custom Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-mini".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::OnRequest),
            reasoning_effort: Some(CodexReasoningEffort::High),
            sandbox_mode: Some(CodexSandboxMode::ReadOnly),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = &response.session;

    assert_eq!(
        session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(session.model, "gpt-5-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));
    assert_eq!(session.sandbox_mode, Some(CodexSandboxMode::ReadOnly));
    assert_eq!(session.claude_approval_mode, None);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&response.session_id)
        .map(|index| &inner.sessions[index]);
    let record = record.expect("session record should exist");
    assert_eq!(record.codex_approval_policy, CodexApprovalPolicy::OnRequest);
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert_eq!(record.codex_sandbox_mode, CodexSandboxMode::ReadOnly);
}

// pins that update_session_settings on a Cursor session with a new
// model string propagates through to the snapshot without touching
// other fields. guards against Cursor model swaps being silently
// dropped or mishandled by the shared settings-update path.
#[test]
fn updates_cursor_session_model_settings() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.3-codex".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.model, "gpt-5.3-codex");
}

// pins that changing a Codex session's model via update_session_settings
// updates the snapshot and leaves runtime_reset_required false — Codex
// accepts model switches live. guards against Codex model swaps
// needlessly flagging the runtime for a restart.
#[test]
fn updates_codex_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-mini");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert!(!record.runtime_reset_required);
}

// pins that bumping a Codex session's reasoning_effort updates both
// the snapshot and the backing record while keeping
// runtime_reset_required false. guards against Codex effort changes
// being treated as restart-worthy like their Claude counterparts.
#[test]
fn updates_codex_reasoning_effort_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: Some(CodexReasoningEffort::High),
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::High);
    assert!(!record.runtime_reset_required);
}

// pins that switching to a model whose supported_reasoning_efforts no
// longer includes the current effort (e.g. Minimal) falls back to that
// model's default_reasoning_effort instead of carrying the unsupported
// value forward. guards against leaving Codex sessions stuck on a
// reasoning effort the newly selected model would reject at send time.
#[test]
fn normalizes_codex_reasoning_effort_when_switching_models() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Model Caps".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Minimal),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![
                SessionModelOption {
                    label: "GPT-5".to_owned(),
                    value: "gpt-5".to_owned(),
                    description: Some("Frontier agentic coding model.".to_owned()),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Minimal,
                        CodexReasoningEffort::Low,
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                    service_tiers: Vec::new(),
                },
                SessionModelOption {
                    label: "GPT-5 Codex Mini".to_owned(),
                    value: "gpt-5-codex-mini".to_owned(),
                    description: Some(
                        "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                    ),
                    badges: Vec::new(),
                    supported_claude_effort_levels: Vec::new(),
                    default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                    supported_reasoning_efforts: vec![
                        CodexReasoningEffort::Medium,
                        CodexReasoningEffort::High,
                    ],
                    service_tiers: Vec::new(),
                },
            ],
        )
        .unwrap();

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5-codex-mini".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Codex session should be present");
    assert_eq!(session.model, "gpt-5-codex-mini");
    assert_eq!(session.reasoning_effort, Some(CodexReasoningEffort::Medium));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Codex session should exist");
    assert_eq!(record.codex_reasoning_effort, CodexReasoningEffort::Medium);
}

// pins that supplying a reasoning effort outside the current model's
// supported_reasoning_efforts returns an error whose message names the
// rejected level and the allowed alternatives. guards against invalid
// efforts reaching the Codex runtime and against the error message
// drifting away from the "choose medium or high" enumeration format.
#[test]
fn rejects_unsupported_codex_reasoning_effort_for_selected_model() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Invalid Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5-codex-mini".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption {
                label: "GPT-5 Codex Mini".to_owned(),
                value: "gpt-5-codex-mini".to_owned(),
                description: Some(
                    "Optimized for codex. Cheaper, faster, but less capable.".to_owned(),
                ),
                badges: Vec::new(),
                supported_claude_effort_levels: Vec::new(),
                default_reasoning_effort: Some(CodexReasoningEffort::Medium),
                supported_reasoning_efforts: vec![
                    CodexReasoningEffort::Medium,
                    CodexReasoningEffort::High,
                ],
                service_tiers: Vec::new(),
            }],
        )
        .unwrap();

    let error = match state.update_session_settings(
        &created.session_id,
        UpdateSessionSettingsRequest {
            name: None,
            model: None,
            sandbox_mode: None,
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Low),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
            opencode_effort: None,
            opencode_mode: None,
            codex_fast_mode: None,
        },
    ) {
        Ok(_) => panic!("unsupported Codex effort should be rejected"),
        Err(error) => error,
    };

    assert!(
        error
            .message
            .contains("does not support `low` reasoning effort; choose medium or high")
    );
}

// pins that the Codex `max` and `ultra` reasoning levels validate and
// persist when the selected model advertises them. guards against the
// effort enum or the model-option validator silently dropping the two
// highest levels the way `parse_codex_reasoning_effort` filters an
// unrecognized level out of a model's `supported_reasoning_levels`.
#[test]
fn accepts_codex_max_and_ultra_reasoning_efforts_for_supporting_model() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Ultra Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.6-sol".to_owned()),
            approval_policy: None,
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_model_options(
            &created.session_id,
            None,
            vec![SessionModelOption {
                label: "GPT-5.6 Sol".to_owned(),
                value: "gpt-5.6-sol".to_owned(),
                description: None,
                badges: Vec::new(),
                supported_claude_effort_levels: Vec::new(),
                default_reasoning_effort: Some(CodexReasoningEffort::Low),
                supported_reasoning_efforts: vec![
                    CodexReasoningEffort::Low,
                    CodexReasoningEffort::Medium,
                    CodexReasoningEffort::High,
                    CodexReasoningEffort::XHigh,
                    CodexReasoningEffort::Max,
                    CodexReasoningEffort::Ultra,
                ],
                service_tiers: Vec::new(),
            }],
        )
        .unwrap();

    for effort in [CodexReasoningEffort::Max, CodexReasoningEffort::Ultra] {
        let updated = state
            .update_session_settings(
                &created.session_id,
                UpdateSessionSettingsRequest {
                    name: None,
                    model: None,
                    sandbox_mode: None,
                    approval_policy: None,
                    reasoning_effort: Some(effort),
                    cursor_mode: None,
                    claude_approval_mode: None,
                    claude_effort: None,
                    gemini_approval_mode: None,
                    opencode_effort: None,
                    opencode_mode: None,
                    codex_fast_mode: None,
                },
            )
            .unwrap_or_else(|error| panic!("{effort:?} should be accepted: {}", error.message));

        let session = updated
            .sessions
            .iter()
            .find(|session| session.id == created.session_id)
            .expect("updated Codex session should be present");
        assert_eq!(session.reasoning_effort, Some(effort));

        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == created.session_id)
            .expect("Codex session should exist");
        assert_eq!(record.codex_reasoning_effort, effort);
    }
}

// pins that `max`/`ultra` survive the real model-list string-parse path, not
// only direct enum construction. `codex_model_options` runs each advertised
// `supported_reasoning_levels` string through `parse_codex_reasoning_effort`,
// so a regression that dropped `"max"`/`"ultra"` (or the JSON field names)
// would empty them here, unlike the enum-injection test above.
#[test]
fn codex_model_options_parses_max_and_ultra_reasoning_levels() {
    let model_list = json!({
        "data": [{
            "model": "gpt-5.6-sol",
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [
                { "effort": "low" },
                { "effort": "xhigh" },
                { "effort": "max" },
                { "effort": "ultra" },
            ],
            "serviceTiers": [{
                "id": "priority",
                "name": "Fast",
                "description": "1.5x speed, increased usage"
            }],
        }],
    });

    let options = codex_model_options(&model_list);
    let option = options
        .iter()
        .find(|option| option.value == "gpt-5.6-sol")
        .expect("model entry should parse");

    assert_eq!(
        option.supported_reasoning_efforts,
        vec![
            CodexReasoningEffort::Low,
            CodexReasoningEffort::XHigh,
            CodexReasoningEffort::Max,
            CodexReasoningEffort::Ultra,
        ]
    );
    assert_eq!(
        option.default_reasoning_effort,
        Some(CodexReasoningEffort::Medium)
    );
    assert_eq!(
        option.service_tiers,
        vec![SessionModelServiceTier {
            id: "priority".to_owned(),
            label: "Fast".to_owned(),
            description: Some("1.5x speed, increased usage".to_owned()),
        }]
    );
}

#[test]
fn codex_model_options_parses_legacy_fast_tier_fields_and_preserves_live_ids() {
    let model_list = json!({
        "data": [
            {
                "model": "camel-fast",
                "additionalSpeedTiers": ["fast"]
            },
            {
                "model": "snake-fast",
                "additional_speed_tiers": ["FAST"]
            },
            {
                "model": "custom-fast",
                "serviceTiers": [{ "id": "turbo", "name": "Fast" }]
            }
        ]
    });

    let options = codex_model_options(&model_list);
    for model in ["camel-fast", "snake-fast"] {
        let option = codex_model_option(model, &options).expect("legacy model should parse");
        assert_eq!(
            option.service_tiers,
            vec![SessionModelServiceTier {
                id: CODEX_FAST_SERVICE_TIER.to_owned(),
                label: "Fast".to_owned(),
                description: Some("1.5x speed, increased usage".to_owned()),
            }]
        );
    }
    assert_eq!(
        codex_fast_service_tier_value("custom-fast", &options, true).as_deref(),
        Some("turbo"),
        "dispatch must use the catalog-advertised tier id rather than hardcoding priority"
    );
    assert_eq!(
        codex_fast_service_tier_value("unknown-after-restart", &[], true).as_deref(),
        Some(CODEX_FAST_SERVICE_TIER),
        "persisted Fast authority needs the canonical fallback until the catalog refreshes"
    );
    assert_eq!(
        codex_fast_service_tier_value("custom-fast", &options, false),
        None
    );
}

#[test]
fn codex_fast_mode_is_catalog_gated_and_clears_on_unsupported_model_switch() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Fast Codex".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("Codex session should be created");
    let empty_catalog_error = match state.update_session_settings(
        &created.session_id,
        UpdateSessionSettingsRequest {
            name: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            codex_fast_mode: Some(true),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
            opencode_effort: None,
            opencode_mode: None,
        },
    ) {
        Ok(_) => panic!("a new Fast enable should require a loaded catalog"),
        Err(error) => error,
    };
    assert_eq!(empty_catalog_error.status, StatusCode::BAD_REQUEST);
    assert!(empty_catalog_error.message.contains("refresh models"));

    let model_options = vec![
        SessionModelOption {
            label: "GPT-5.5".to_owned(),
            value: "gpt-5.5".to_owned(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: Some(CodexReasoningEffort::Medium),
            supported_reasoning_efforts: vec![CodexReasoningEffort::Medium],
            service_tiers: vec![SessionModelServiceTier {
                id: CODEX_FAST_SERVICE_TIER.to_owned(),
                label: "Fast".to_owned(),
                description: Some("1.5x speed, increased usage".to_owned()),
            }],
        },
        SessionModelOption {
            label: "GPT-5.4 mini".to_owned(),
            value: "gpt-5.4-mini".to_owned(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: Some(CodexReasoningEffort::Medium),
            supported_reasoning_efforts: vec![CodexReasoningEffort::Medium],
            service_tiers: Vec::new(),
        },
    ];
    state
        .sync_session_model_options(&created.session_id, None, model_options.clone())
        .expect("Codex model catalog should sync");

    let fast = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: Some(true),
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
            },
        )
        .expect("advertised Fast mode should enable");
    assert!(
        fast.sessions
            .iter()
            .find(|session| session.id == created.session_id)
            .expect("Codex session should remain visible")
            .codex_fast_mode
    );

    state
        .sync_session_model_options(&created.session_id, None, Vec::new())
        .expect("an empty refresh should preserve unknown Fast authority");
    let unrelated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: Some(true),
                sandbox_mode: Some(CodexSandboxMode::ReadOnly),
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
            },
        )
        .expect("unrelated settings must preserve Fast while the catalog is unknown");
    let unrelated_session = unrelated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("Codex session should remain visible");
    assert!(unrelated_session.codex_fast_mode);
    assert_eq!(
        unrelated_session.sandbox_mode,
        Some(CodexSandboxMode::ReadOnly)
    );

    state
        .sync_session_model_options(&created.session_id, None, model_options)
        .expect("Codex model catalog should refresh again");

    let stale_carry = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.4-mini".to_owned()),
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: Some(true),
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
            },
        )
        .expect("a stale carried Fast value must not reject the model switch");
    assert!(
        !stale_carry
            .sessions
            .iter()
            .find(|session| session.id == created.session_id)
            .expect("Codex session should remain visible")
            .codex_fast_mode,
        "the server catalog must clear stale carried Fast authority"
    );

    state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.5".to_owned()),
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: Some(true),
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
            },
        )
        .expect("returning to a supporting model should re-enable Fast");

    let standard = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("gpt-5.4-mini".to_owned()),
                approval_policy: None,
                reasoning_effort: None,
                codex_fast_mode: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
            },
        )
        .expect("unsupported model switch should safely disable Fast mode");
    assert!(
        !standard
            .sessions
            .iter()
            .find(|session| session.id == created.session_id)
            .expect("Codex session should remain visible")
            .codex_fast_mode
    );

    let error = match state.update_session_settings(
        &created.session_id,
        UpdateSessionSettingsRequest {
            name: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            codex_fast_mode: Some(true),
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
            opencode_effort: None,
            opencode_mode: None,
        },
    ) {
        Ok(_) => panic!("unsupported model should reject Fast mode"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("does not advertise Codex Fast mode"));
}

// pins that changing the model on a running Claude session with a
// concrete target (e.g. opus) dispatches a SetModel command over the
// runtime's input channel and leaves runtime_reset_required false.
// guards against Claude concrete-model swaps incorrectly requiring a
// restart or skipping the live SetModel notification.
#[test]
fn updates_claude_session_model_settings_without_restarting_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Model".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-model-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("opus".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.model, "opus");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_reset_required);

    let command = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("Claude model update should arrive");
    match command {
        ClaudeRuntimeCommand::SetModel(model) => assert_eq!(model, "opus"),
        _ => panic!("expected Claude model update command"),
    }
}

// pins that switching a running Claude session to the "default"
// sentinel flips runtime_reset_required to true and sends no live
// SetModel command — the Claude CLI cannot apply the default sentinel
// hot. guards against the default sentinel being forwarded as a
// literal model string or skipping the required restart flag.
#[test]
fn updating_running_claude_session_to_default_model_requires_restart() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Default".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-default-model-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: Some("default".to_owned()),
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.model, "default");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(record.runtime_reset_required);
    drop(inner);

    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("default sentinel should not be sent to Claude"),
        Err(err) => panic!("unexpected Claude command channel error: {err}"),
    }
}

// pins that changing claude_effort on a running Claude session writes
// the new level into the snapshot, flips runtime_reset_required to
// true, and dispatches no live runtime command — Claude effort only
// takes effect on restart. guards against Claude effort changes
// silently being treated as a hot update like Codex effort swaps.
#[test]
fn updates_claude_effort_and_marks_runtime_for_restart() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Effort".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("sonnet".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Ask),
            claude_effort: Some(ClaudeEffortLevel::Default),
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-effort-update".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: Some(ClaudeEffortLevel::High),
                gemini_approval_mode: None,
                opencode_effort: None,
                opencode_mode: None,
                codex_fast_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Claude session should exist");
    assert!(record.runtime_reset_required);

    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("Claude effort changes should not send a live runtime command"),
        Err(err) => panic!("unexpected channel error: {err}"),
    }
}

// pins that sync_session_model_options populates a Claude session's
// model_options list and promotes the first option's value as the
// active model when the session was created without one. guards
// against Claude session snapshots losing their model-option catalog
// or failing to default to the first discovered entry.
#[test]
fn syncs_claude_model_options_into_session_state() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Default (recommended)", "default"),
        SessionModelOption::plain("Sonnet", "sonnet"),
    ];

    state
        .sync_session_model_options(&created.session_id, None, model_options.clone())
        .expect("Claude model options should sync");

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("synced Claude session should be present");

    assert_eq!(session.model, "default");
    assert_eq!(session.model_options, model_options);
}
