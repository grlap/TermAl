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

// pins that update_app_settings writes the global preferences to disk
// and that a freshly loaded AppState inherits those defaults when
// creating new Codex and Claude sessions. guards against app-settings
// persistence regressions and newly created sessions ignoring the
// configured global defaults.
#[test]
fn persists_app_settings_and_applies_them_to_new_sessions() {
    let state = test_app_state();

    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_reasoning_effort: Some(CodexReasoningEffort::High),
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
        persistence_path: state.persistence_path.clone(),
        orchestrator_templates_path: state.orchestrator_templates_path.clone(),
        orchestrator_templates_lock: state.orchestrator_templates_lock.clone(),
        review_documents_lock: state.review_documents_lock.clone(),
        state_events: broadcast::channel(16).0,
        delta_events: broadcast::channel(16).0,
        file_events: broadcast::channel(16).0,
        file_events_revision: Arc::new(AtomicU64::new(0)),
        persist_tx: mpsc::channel().0,
        persist_thread_handle: Arc::new(Mutex::new(None)),
        shutdown_notify: Arc::new(tokio::sync::Notify::new()),
        state_broadcast_tx: mpsc::channel().0,
        shared_codex_runtime: Arc::new(Mutex::new(None)),
        agent_readiness_cache: Arc::new(RwLock::new(fresh_agent_readiness_cache("/tmp"))),
        agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
        remote_registry: test_remote_registry(),
        remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
        remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
        terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
        )),
        terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
            TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
        )),
        stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
        stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
        inner: Arc::new(Mutex::new(reloaded_inner)),
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

    let _ = fs::remove_file(state.persistence_path.as_path());
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
