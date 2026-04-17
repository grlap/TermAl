// claude and codex session lifecycle: creation, hidden spare-pool, and
// kill semantics.
//
// claude code has a noticeable cold-start cost, so termal pre-spawns
// "hidden claude spares" keyed by (workdir, model, approval_mode,
// effort). when the user creates a real session whose dimensions match
// a spare, `StateInner::create_session` promotes the spare in place
// instead of cold-starting, then `ensure_hidden_claude_spare`
// replenishes the pool. hidden spares carry `hidden = true` and are
// filtered out of `/api/state` snapshots and `PersistedState` so the
// user never sees them.
//
// kill-session semantics diverge by agent. killing the LAST visible
// claude session for a workdir also reaps the matching hidden spare,
// since there is no reason to keep a warm session for a workdir the
// user abandoned; if another visible session remains, the spare stays
// warm. codex sessions can share a single runtime: killing one must
// not tear down siblings even when the `turn/interrupt` jsonrpc call
// fails, and local codex sessions are added to a rediscovery ignore
// list so they cannot silently return after restart.
//
// production surfaces under test: `StateInner::create_session`,
// `AppState::kill_session`, the axum `kill_session` route in
// `src/api.rs`, and `StateInner::ensure_hidden_claude_spare`.

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

// pins that the sentinel `"default"` model tells the cli layer to
// omit `--model` entirely so claude code picks its own default,
// while explicit models are forwarded and claude-specific flags
// (plan mode, effort, resume) serialize correctly. guards against
// regressions that would hard-code sonnet or drop cli args.
#[test]
fn claude_default_model_delegates_to_claude_cli_default() {
    assert_eq!(Agent::Claude.default_model(), "default");
    assert_eq!(claude_cli_model_arg("default"), None);
    assert_eq!(claude_cli_model_arg(" Default "), None);
    assert_eq!(claude_cli_model_arg("opus"), Some("opus"));
    assert_eq!(
        claude_cli_oneshot_args(
            " default ",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            ClaudeCliSessionArg::SessionId("session-a"),
        ),
        vec![
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--session-id",
            "session-a",
        ],
    );
    assert_eq!(
        claude_cli_oneshot_args(
            "opus",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            ClaudeCliSessionArg::SessionId("session-a"),
        ),
        vec![
            "--model",
            "opus",
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--session-id",
            "session-a",
        ],
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
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "plan",
            "--effort",
            "high",
            "--resume",
            "claude-session",
        ],
    );
    assert_eq!(
        claude_cli_persistent_args(
            " default ",
            ClaudeApprovalMode::Ask,
            ClaudeEffortLevel::Default,
            None,
        ),
        vec![
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
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
    let session = response
        .session
        .as_ref()
        .expect("created session should be returned");

    assert_eq!(session.claude_approval_mode, Some(ClaudeApprovalMode::Plan));
    assert_eq!(session.claude_effort, Some(ClaudeEffortLevel::High));
}

// pins the invisibility contract for hidden claude spares: a spare
// created via `ensure_hidden_claude_spare` lives in `inner.sessions`
// with `hidden = true`, but is filtered out of both
// `AppState::snapshot` and `PersistedState::from_inner`.
// guards against the warm pool leaking into the ui or on-disk state.
#[test]
fn hidden_claude_spares_are_filtered_from_snapshots_and_persistence() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir,
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let snapshot = state.snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.id != hidden_session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| record.hidden && record.session.id == hidden_session_id)
    );
    let persisted = PersistedState::from_inner(&inner);
    assert!(
        persisted
            .sessions
            .iter()
            .all(|record| record.session.id != hidden_session_id)
    );
}

// pins spare promotion on the default-profile path: creating a
// visible claude session whose (workdir, model, approval, effort)
// matches a hidden spare returns that spare's id, flips
// `hidden = false`, and leaves exactly one fresh spare behind.
// guards against losing the warm pool or double-promoting.
#[test]
fn create_session_promotes_matching_hidden_claude_spare_and_replenishes_pool() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                Agent::Claude.default_model().to_owned(),
                ClaudeApprovalMode::Ask,
                ClaudeEffortLevel::Default,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Visible Claude".to_owned()),
            workdir: Some(workdir.clone()),
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

    assert_eq!(response.session_id, hidden_session_id);
    let session = response
        .session
        .as_ref()
        .expect("promoted hidden session should be returned");
    assert_eq!(session.id, hidden_session_id);
    assert_eq!(session.name, "Visible Claude");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let promoted = inner
        .sessions
        .iter()
        .find(|record| record.session.id == hidden_session_id)
        .expect("promoted session record should exist");
    assert!(!promoted.hidden);

    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

// pins that spare matching keys on all four dimensions, not just
// workdir: a `(claude-custom, Plan, High)` spare is promoted by a
// request with the same model/approval/effort, and a new spare is
// respawned for the same non-default profile.
// guards against over-broad matching that would hand out a spare
// with the wrong flags.
#[test]
fn create_session_promotes_matching_non_default_hidden_claude_spare() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let hidden_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .ensure_hidden_claude_spare(
                workdir.clone(),
                None,
                "claude-custom".to_owned(),
                ClaudeApprovalMode::Plan,
                ClaudeEffortLevel::High,
            )
            .expect("hidden Claude spare should be created")
    };

    let response = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Plan Claude".to_owned()),
            workdir: Some(workdir.clone()),
            project_id: None,
            model: Some("claude-custom".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: Some(ClaudeApprovalMode::Plan),
            claude_effort: Some(ClaudeEffortLevel::High),
            gemini_approval_mode: None,
        })
        .unwrap();

    assert_eq!(response.session_id, hidden_session_id);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let hidden_spares = inner
        .sessions
        .iter()
        .filter(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.model == "claude-custom"
                && record.session.claude_approval_mode == Some(ClaudeApprovalMode::Plan)
                && record.session.claude_effort == Some(ClaudeEffortLevel::High)
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_spares.len(), 1);
    assert_ne!(hidden_spares[0].session.id, hidden_session_id);
}

// pins the "last visible session" reap rule: once the final
// visible claude session for a workdir is killed, every claude
// record for that workdir (hidden spares included) is removed.
// guards against stranded warm processes tied to a workdir the
// user has walked away from.
#[test]
fn killing_last_visible_claude_session_reaps_hidden_spare_for_context() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Visible".to_owned()),
            workdir: Some(workdir.clone()),
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

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.sessions.iter().any(|record| {
            record.hidden
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
        }));
    }

    let killed = state.kill_session(&created.session_id).unwrap();
    assert!(
        killed
            .sessions
            .iter()
            .all(|session| session.id != created.session_id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.sessions.iter().all(|record| {
        !(record.session.agent == Agent::Claude && record.session.workdir == workdir)
    }));
}

// pins the complement of the reap rule: with two visible claude
// sessions sharing a workdir, killing one preserves both the
// surviving visible session and the matching hidden spare.
// guards against over-eager reaping that would force the next
// session to cold-start.
#[test]
fn killing_one_visible_claude_session_keeps_hidden_spares_when_another_visible_session_remains() {
    let state = test_app_state();
    let workdir = resolve_session_workdir("/tmp").expect("test workdir should resolve");
    let first = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude A".to_owned()),
            workdir: Some(workdir.clone()),
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
    let second = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude B".to_owned()),
            workdir: Some(workdir.clone()),
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

    state.kill_session(&first.session_id).unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .any(|record| !record.hidden && record.session.id == second.session_id)
    );
    assert!(inner.sessions.iter().any(|record| {
        record.hidden
            && record.session.agent == Agent::Claude
            && record.session.workdir == workdir
            && record.session.project_id.is_none()
    }));
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
