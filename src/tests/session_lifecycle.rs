// Claude and Codex session lifecycle: creation, asynchronous Stop, and kill semantics.
//
// Agent processes start lazily when the user sends the first prompt;
// creating a session alone must not start a runtime. Codex sessions can
// share a single runtime, so killing one must not tear down siblings even
// when the `turn/interrupt` JSON-RPC call fails. Local Codex sessions are
// also added to a rediscovery ignore list so they cannot silently return
// after restart.
//
// Production surfaces under test: `StateInner::create_session`,
// `AppState::request_stop_session`, `AppState::kill_session`, and the Axum
// Stop/kill routes in `src/api.rs`.

use super::*;

// pins the claude session defaults applied by
// `StateInner::create_session`: model `"default"`, approval mode
// `Ask`, effort `Default`, and no codex-flavoured policy fields.
// guards against accidental drift in the claude starter profile.
#[test]
fn creates_claude_sessions_with_default_ask_mode() {
    let mut inner = StateInner::new();

    let record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

    assert_eq!(record.session.model, "default");
    assert_eq!(
        record.session.claude_approval_mode,
        Some(ClaudeApprovalMode::Ask)
    );
    assert_eq!(
        record.session.claude_effort,
        Some(ClaudeEffortLevel::Default)
    );
    assert_eq!(record.session.approval_policy, None);
    assert_eq!(record.session.sandbox_mode, None);
}

// A process can exit after persisting Stopping but before its background
// worker records completion. Boot must route that state through the same
// interrupted-turn recovery as Active/Approval rather than leaving a session
// permanently stuck in Stopping.
#[test]
fn boot_recovers_a_persisted_stopping_session_as_interrupted() {
    let mut inner = StateInner::new();
    let session_id = inner
        .create_session(Agent::Claude, None, "/tmp".to_owned(), None, None)
        .session
        .id
        .clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].session.status = SessionStatus::Stopping;
    inner.sessions[index].session.preview = SESSION_STOPPING_MESSAGE.to_owned();

    inner.recover_interrupted_sessions();

    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("recovered session should remain");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(
        record
            .session
            .preview
            .contains("restarted while this session was stopping")
    );
    assert!(record.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. }
            if text.contains("restarted while this session was stopping")
    )));
}

// pins that the sentinel `"default"` model tells the cli layer to
// omit `--model` entirely so Claude Code picks its own default,
// while explicit models are forwarded and claude-specific flags
// (plan mode, effort, resume) serialize correctly. This also pins
// the VS Code-style persistent stdio contract: no `-p` one-shot mode.
#[test]
fn claude_default_model_delegates_to_claude_cli_default() {
    assert_eq!(Agent::Claude.default_model(), "default");
    assert_eq!(claude_cli_model_arg("default"), None);
    assert_eq!(claude_cli_model_arg(" Default "), None);
    assert_eq!(claude_cli_model_arg("opus"), Some("opus"));
    assert_eq!(
        parse_claude_effort_level("xhigh"),
        Some(ClaudeEffortLevel::XHigh)
    );
    assert_eq!(
        serde_json::to_string(&ClaudeEffortLevel::XHigh).unwrap(),
        "\"xhigh\""
    );
    assert_eq!(
        claude_cli_persistent_args(
            "opus",
            ClaudeApprovalMode::Plan,
            ClaudeEffortLevel::High,
            Some("claude-session"),
        ),
        vec![
            "--model",
            "opus",
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--setting-sources",
            "user,project,local",
            "--no-chrome",
            "--input-format",
            "stream-json",
            "--include-hook-events",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--replay-user-messages",
            "--permission-mode",
            "plan",
            "--effort",
            "high",
            "--resume",
            "claude-session",
        ],
    );
    let xhigh_args = claude_cli_persistent_args(
        "opus",
        ClaudeApprovalMode::Ask,
        ClaudeEffortLevel::XHigh,
        None,
    );
    assert!(
        xhigh_args
            .windows(2)
            .any(|pair| pair[0] == "--effort" && pair[1] == "xhigh")
    );
    assert_eq!(
        claude_cli_persistent_args(
            " default ",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            None,
        ),
        vec![
            "--print",
            "--verbose",
            "--output-format",
            "stream-json",
            "--setting-sources",
            "user,project,local",
            "--no-chrome",
            "--input-format",
            "stream-json",
            "--include-hook-events",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--replay-user-messages",
        ],
    );
}

// pins that `AppState::create_session` honours non-default claude
// knobs from the request: `claude_approval_mode = Plan` and
// `claude_effort = High` land on the returned record.
// guards against the dispatcher silently falling back to the
// default ask profile.
#[test]
fn creates_claude_sessions_with_requested_plan_mode() {
    let state = test_app_state();

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();
    let session = &response.session;

    assert_eq!(session.claude_approval_mode, Some(ClaudeApprovalMode::Plan));
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == response.session_id)
        .expect("created Claude session should exist");
    assert!(!record.hidden);
    assert!(
        matches!(record.runtime, SessionRuntime::None),
        "creating a Claude session must not start its runtime before the first prompt",
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| !(record.hidden && record.session.agent == Agent::Claude)),
        "creating a Claude session must not allocate a hidden background session",
    );
}

#[test]
fn lightweight_test_state_rejects_direct_claude_runtime_spawning() {
    let state = test_app_state();
    let result = spawn_claude_runtime(
        state,
        "session-no-real-claude".to_owned(),
        "/tmp".to_owned(),
        Agent::Claude.default_model().to_owned(),
        ClaudeApprovalMode::Ask,
        ClaudeEffortLevel::Default,
        None,
        String::new(),
        None,
    );

    let err = match result {
        Ok(_) => panic!("lightweight test state must not start a real Claude runtime"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        "agent runtime spawning is disabled for this AppState"
    );
}

#[test]
fn claude_control_failure_terminates_the_child_and_preserves_the_waiter_reason() {
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let error_override = Arc::new(Mutex::new(None));
    let detail = "failed to handle Claude control request: unattended question loop";

    terminate_claude_runtime_after_control_failure(&process, &error_override, detail)
        .expect("the reader failure should terminate the Claude child");
    process
        .wait()
        .expect("the terminated Claude child should be reapable");
    assert_eq!(
        error_override
            .lock()
            .expect("Claude runtime-exit override mutex poisoned")
            .take()
            .as_deref(),
        Some(detail)
    );
}

#[test]
fn claude_waiter_collects_the_reader_failure_after_the_reader_finishes() {
    let error_override = Arc::new(Mutex::new(None));
    let reader_error_override = error_override.clone();
    let detail = "failed to handle Claude control request: recorder unavailable";
    let reader = std::thread::spawn(move || {
        *reader_error_override
            .lock()
            .expect("Claude runtime-exit override mutex poisoned") = Some(detail.to_owned());
    });

    assert_eq!(
        take_claude_runtime_exit_error_after_reader(reader, &error_override).as_deref(),
        Some(detail)
    );
}

#[test]
fn private_claude_mcp_config_file_is_exclusive_owner_only_and_removed_on_drop() {
    let temp = TestTempRoot::create("termal-claude-mcp-config-file");
    let dir = temp.path().join("delegations").join("mcp");
    let contents = r#"{"mcpServers":{"engram":{"env":{"ENGRAM_WORK_AUTHORITY_GRANT":"operator-secret-grant"}}}}"#;

    let guard = write_private_claude_mcp_config(&dir, "runtime-a", contents)
        .expect("the private MCP configuration should be written");
    assert_eq!(guard.path, dir.join("claude-mcp-runtime-a.json"));
    assert_eq!(
        fs::read_to_string(&guard.path).expect("the file should be readable"),
        contents
    );
    // Exclusive creation: a second file for the same runtime id must fail
    // instead of silently overwriting or truncating.
    assert!(write_private_claude_mcp_config(&dir, "runtime-a", "{}").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&guard.path)
            .expect("metadata should be readable")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The path handed to Claude's argv never carries the secret itself.
    assert!(
        !guard
            .path
            .to_string_lossy()
            .contains("operator-secret-grant")
    );

    let path = guard.path.clone();
    drop(guard);
    assert!(!path.exists(), "dropping the guard must remove the file");
}

#[test]
fn private_claude_mcp_config_write_refuses_a_symlinked_directory() {
    #[cfg(unix)]
    {
        let temp = TestTempRoot::create("termal-claude-mcp-config-symlink");
        let real = temp.path().join("elsewhere");
        fs::create_dir_all(&real).expect("real directory should exist");
        let link = temp.path().join("mcp");
        std::os::unix::fs::symlink(&real, &link).expect("symlink should be created");
        assert!(write_private_claude_mcp_config(&link, "runtime-b", "{}").is_err());
        assert!(
            fs::read_dir(&real)
                .expect("real directory should be listable")
                .next()
                .is_none()
        );
    }
}

#[test]
fn claude_mcp_config_dir_stays_beside_the_persistence_root_not_under_home() {
    let temp = TestTempRoot::create("termal-claude-mcp-config-dir");
    let persistence_path = temp.path().join("state").join("termal.sqlite");
    let dir = claude_mcp_config_dir(&persistence_path);
    assert_eq!(
        dir,
        temp.path().join("state").join("delegations").join("mcp")
    );
    for home in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        let home = PathBuf::from(home).join(".termal");
        assert!(
            !dir.starts_with(&home),
            "test persistence roots must never resolve into the operator's real data tree"
        );
    }
}

#[test]
fn private_claude_mcp_config_is_released_on_first_valid_stdout_line() {
    let temp = TestTempRoot::create("termal-claude-mcp-config-release");
    let dir = temp.path().join("delegations").join("mcp");
    let guard = write_private_claude_mcp_config(&dir, "runtime-c", "{}")
        .expect("the private MCP configuration should be written");
    let path = guard.path.clone();
    let mut slot = Some(guard);

    release_private_claude_mcp_config(&mut slot);
    assert!(slot.is_none());
    assert!(
        !path.exists(),
        "the first valid stdout line must remove the secret file"
    );
    // Idempotent: later lines and the exit fallback find nothing to do.
    release_private_claude_mcp_config(&mut slot);
    assert!(!path.exists());
}

#[test]
fn lightweight_test_state_rejects_direct_shared_codex_runtime_spawning() {
    let state = test_app_state();
    let result = spawn_shared_codex_runtime(state);

    let err = match result {
        Ok(_) => panic!("lightweight test state must not start a real Codex runtime"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        "agent runtime spawning is disabled for this AppState"
    );
}

#[test]
fn lightweight_test_state_rejects_direct_acp_runtime_spawning() {
    let state = test_app_state();
    let result = spawn_acp_runtime(
        state,
        "session-no-real-acp".to_owned(),
        "/tmp".to_owned(),
        AcpAgent::Cursor,
        None,
    );

    let err = match result {
        Ok(_) => panic!("lightweight test state must not start a real ACP runtime"),
        Err(err) => err,
    };
    assert_eq!(
        err.to_string(),
        "agent runtime spawning is disabled for this AppState"
    );
}

// pins that a kill still commits to disk when the shared codex
// `turn/interrupt` jsonrpc send fails (input channel dropped):
// the session is removed from the live snapshot, from the reloaded
// persisted state, and from the shared runtime's session and
// thread maps. guards against zombies surviving a failed rpc.
#[test]
fn killing_session_persists_removal_even_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-1".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-1".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-1".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-1".to_owned(), session_id.clone());

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );
    assert!(
        !shared_runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !shared_runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-1")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the http contract: `POST /api/sessions/{id}/kill` returns
// 200 OK with a session-free `StateResponse` even when the shared
// codex interrupt rpc fails. guards against surfacing a spurious
// 5xx to the client over a best-effort interrupt.
#[tokio::test]
async fn kill_session_route_returns_ok_when_shared_codex_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-route".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("test session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: "runtime-route".to_owned(),
            input_tx,
            process: process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: shared_runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-route".to_owned()),
                turn_id: Some("turn-route".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-route".to_owned(), session_id.clone());

    let app = app_router(state.clone());
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/kill"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        response
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the public Stop contract: the route persists an explicit Stopping
// state and returns well below the runtime's shutdown bound, a repeated Stop
// is idempotent, and the claimed worker finishes the old synchronous cleanup
// after the response is already in the caller's hands.
#[tokio::test]
async fn stop_session_route_returns_stopping_immediately_and_finishes_in_background() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Claude session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "claude-async-stop-route".to_owned(),
            input_tx,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "Streaming reply...".to_owned();
    }

    let gate = install_test_stop_fence_gate(&state, &session_id);
    let app = app_router(state.clone());
    let started_at = std::time::Instant::now();
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let route_elapsed = started_at.elapsed();

    assert_eq!(status, StatusCode::OK);
    assert!(
        route_elapsed < Duration::from_millis(100),
        "Stop route took {route_elapsed:?} before returning"
    );
    let stopping = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopping session should remain visible");
    assert_eq!(stopping.status, SessionStatus::Stopping);
    assert_eq!(stopping.preview, SESSION_STOPPING_MESSAGE);
    gate.wait_until_claimed();

    let second_started_at = std::time::Instant::now();
    let (second_status, second_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(second_status, StatusCode::OK);
    assert!(second_started_at.elapsed() < Duration::from_millis(100));
    assert_eq!(
        second_response
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("idempotent Stop should retain the session")
            .status,
        SessionStatus::Stopping
    );

    gate.release();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let status = state
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should survive Stop")
            .status;
        if status != SessionStatus::Stopping {
            assert_eq!(status, SessionStatus::Idle);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background Stop did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("stopped session should remain");
    assert!(!record.runtime_stop_in_progress);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert_eq!(record.session.preview, SESSION_STOPPED_BY_USER_MESSAGE);
    drop(inner);

    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// A background interrupt failure cannot be returned through the already
// completed HTTP response. It must therefore settle the persisted Stopping
// state to Error and leave an actionable transcript notice.
#[test]
fn asynchronous_stop_surfaces_runtime_interrupt_failure_on_the_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Claude session index should be valid");
        record.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
            runtime_id: "claude-async-stop-failure".to_owned(),
            input_tx,
            process: process.clone(),
        });
        record.session.status = SessionStatus::Active;
    }

    let failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let response = state
        .request_stop_session(&session_id)
        .expect("Stop should return its Stopping snapshot");
    assert_eq!(
        response
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should remain")
            .status,
        SessionStatus::Stopping
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let status = state
            .snapshot()
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("session should remain")
            .status;
        if status != SessionStatus::Stopping {
            assert_eq!(status, SessionStatus::Error);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background interrupt failure was not surfaced"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("failed Stop session should remain");
    assert!(!record.runtime_stop_in_progress);
    assert!(
        record
            .session
            .preview
            .contains("Stop failed in the background")
    );
    assert!(record.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. }
            if text.contains("Stop failed in the background")
                && text.contains("failed to stop session")
    )));
    drop(inner);

    drop(failure_guard);
    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins multi-tenant isolation for a shared codex runtime: killing
// one session with a failing interrupt leaves siblings on the same
// runtime intact — their runtime handle, status, shared session
// entry, and thread mapping all survive, and the shared process
// is not torn down. guards against a failed rpc cascading into
// collateral resets.
#[test]
fn killing_shared_codex_session_does_not_reset_other_shared_sessions_when_interrupt_fails() {
    let state = test_app_state();
    let first_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Two".to_owned()),
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
    let second_session_id = created.session_id;
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let shared_runtime = SharedCodexRuntime {
        runtime_id: "runtime-shared".to_owned(),
        input_tx: input_tx.clone(),
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };
    drop(input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for session_id in [&first_session_id, &second_session_id] {
            let index = inner
                .find_session_index(session_id)
                .expect("test session should exist");
            inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                runtime_id: "runtime-shared".to_owned(),
                input_tx: input_tx.clone(),
                process: process.clone(),
                shared_session: Some(SharedCodexSessionHandle {
                    runtime: shared_runtime.clone(),
                    session_id: session_id.to_string(),
                }),
            });
            inner.sessions[index].session.status = SessionStatus::Active;
        }
    }

    shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .extend([
            (
                first_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-a".to_owned()),
                    turn_id: Some("turn-a".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
            (
                second_session_id.clone(),
                SharedCodexSessionState {
                    thread_id: Some("thread-b".to_owned()),
                    turn_id: Some("turn-b".to_owned()),
                    ..SharedCodexSessionState::default()
                },
            ),
        ]);
    shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .extend([
            ("thread-a".to_owned(), first_session_id.clone()),
            ("thread-b".to_owned(), second_session_id.clone()),
        ]);

    let killed = state.kill_session(&first_session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != first_session_id)
    );
    assert!(
        killed
            .sessions
            .iter()
            .any(|session| session.id == second_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let second_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == second_session_id)
        .expect("second session should still exist");
    assert!(matches!(second_record.runtime, SessionRuntime::Codex(_)));
    assert_eq!(second_record.session.status, SessionStatus::Active);
    drop(inner);

    let shared_sessions = shared_runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&first_session_id));
    assert!(shared_sessions.contains_key(&second_session_id));
    drop(shared_sessions);
    let thread_sessions = shared_runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    assert!(!thread_sessions.contains_key("thread-a"));
    assert_eq!(
        thread_sessions.get("thread-b").map(String::as_str),
        Some(second_session_id.as_str())
    );
    drop(thread_sessions);
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(50),
            "shared Codex runtime"
        )
        .unwrap()
        .is_none()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the rediscovery ignore list for local codex sessions:
// killing a session with an external thread id adds that id to
// `ignored_discovered_codex_thread_ids`, and a subsequent
// `import_discovered_codex_threads` with the same thread will not
// resurrect it. guards against killed sessions silently returning
// after a restart.
#[test]
fn killing_local_codex_session_prevents_rediscovery_after_restart() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-killed".to_owned())
        .unwrap();

    let killed = state.kill_session(&session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != session_id)
    );

    let mut reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("thread-killed")
    );

    reloaded_inner.import_discovered_codex_threads(
        "/tmp",
        vec![DiscoveredCodexThread {
            approval_policy: Some(CodexApprovalPolicy::Never),
            archived: false,
            cwd: "/tmp".to_owned(),
            id: "thread-killed".to_owned(),
            model: Some("gpt-5-codex".to_owned()),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            title: "Killed thread".to_owned(),
        }],
    );

    assert!(
        reloaded_inner
            .sessions
            .iter()
            .all(|record| record.external_session_id.as_deref() != Some("thread-killed"))
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
