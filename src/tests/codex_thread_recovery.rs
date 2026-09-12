//! Lost rollout recovery through the shared writer's real setup handshake.
use super::delegation_support::*;
use super::*;

const EMPTY_ROLLOUT: &str = "failed to read thread: thread-store internal error: failed to read session metadata /fixture/rollout.jsonl: rollout at /fixture/rollout.jsonl is empty";

#[test]
fn shared_codex_empty_rollout_recovers_the_same_prompt_on_a_new_thread() {
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime("lost-rollout");
    let session = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session, "lost-thread".to_owned())
        .unwrap();
    dispatch_turn_and_snapshot(
        &state,
        &session,
        SendMessageRequest {
            text: "Keep this user prompt".to_owned(),
            expanded_text: None,
            attachments: vec![],
            source_session_id: None,
            source_mailbox: None,
        },
    )
    .unwrap();
    let original_messages = state.get_session(&session).unwrap().session.messages;
    let CodexRuntimeCommand::Prompt { command, .. } =
        phase_sync::receive(&input_rx, "accepted prompt")
    else {
        panic!("expected prompt")
    };
    let runtime = state
        .shared_codex_runtime
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
        None,
        &session,
        command,
    )
    .unwrap();
    let (_, response) = take_pending_codex_request(&pending);
    response
        .send(Err(CodexResponseError::JsonRpc(EMPTY_ROLLOUT.to_owned())))
        .unwrap();
    let guard = phase_sync::PollGuard::new();
    let recovery = loop {
        if let Ok(command) = input_rx.try_recv() {
            break Some(command);
        }
        if state.get_session(&session).unwrap().session.status == SessionStatus::Error {
            break None;
        }
        guard.wait(format_args!(
            "resume response must recover or explicitly fail"
        ));
    };
    let snapshot = state.get_session(&session).unwrap().session;
    assert!(
        recovery.is_some(),
        "empty rollout must queue fresh setup, not leave status={:?} external_session_id={:?}",
        snapshot.status,
        snapshot.external_session_id
    );
    let CodexRuntimeCommand::RecoverLostThread {
        session_id,
        request_id,
        thread_id,
        detail,
    } = recovery.unwrap()
    else {
        panic!("expected recovery command")
    };
    handle_shared_codex_lost_thread_recovery(
        &mut writer,
        &pending,
        &state,
        &runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &runtime.sessions,
        &runtime.thread_sessions,
        &runtime.input_tx,
        None,
        &session_id,
        &request_id,
        &thread_id,
        &detail,
    )
    .unwrap();
    let snapshot = state.get_session(&session).unwrap().session;
    assert_eq!(snapshot.external_session_id, None);
    assert_eq!(snapshot.status, SessionStatus::Active);
    let transcript = serde_json::to_string(&snapshot.messages).unwrap();
    assert!(transcript.contains("lost-thread"));
    assert!(transcript.contains("Recovered unreadable Codex thread"));
    assert!(transcript.contains("Keep this user prompt"));
    let requests: Vec<Value> = String::from_utf8(writer.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|v| v["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["thread/resume", "thread/start"]
    );
    assert_eq!(requests[0]["params"]["threadId"], "lost-thread");
    assert_eq!(
        requests[1]["params"]["model"],
        requests[0]["params"]["model"]
    );
    assert_eq!(
        requests[1]["params"]["config"],
        requests[0]["params"]["config"]
    );
    let (_, response) = take_pending_codex_request(&pending);
    response
        .send(Ok(json!({"thread":{"id":"fresh-thread"}})))
        .unwrap();
    let CodexRuntimeCommand::StartTurnAfterSetup {
        session_id,
        thread_id,
        command,
    } = phase_sync::receive(&input_rx, "new thread must deliver original turn")
    else {
        panic!("expected turn after fresh setup")
    };
    assert_eq!(command.resume_thread_id, None);
    assert!(command.prompt.starts_with("<termal-thread-recovery>"));
    assert!(command.prompt.ends_with("\n\nKeep this user prompt"));
    assert_eq!(thread_id, "fresh-thread");
    handle_shared_codex_start_turn(
        &mut writer,
        &pending,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &session_id,
        &thread_id,
        None,
        command,
    )
    .unwrap();
    let (_, response) = take_pending_codex_request(&pending);
    response
        .send(Ok(json!({"turn":{"id":"recovered-turn"}})))
        .unwrap();
    for method in ["turn/started", "turn/completed"] {
        handle_shared_codex_app_server_message(
            &json!({"method":method,
            "params":{"threadId":"fresh-thread","turn":{"id":"recovered-turn"}}}),
            &state,
            &runtime.runtime_id,
            &pending,
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
        )
        .unwrap();
    }
    let snapshot = state.get_session(&session).unwrap().session;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(
        snapshot.external_session_id.as_deref(),
        Some("fresh-thread")
    );
    assert!(
        serde_json::to_string(&snapshot.messages)
            .unwrap()
            .contains("Keep this user prompt")
    );
    assert_eq!(
        serde_json::to_value(&snapshot.messages[..original_messages.len()]).unwrap(),
        serde_json::to_value(&original_messages).unwrap()
    );
    let requests: Vec<Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .filter(|v| v["method"] == "turn/start")
            .count(),
        1
    );
}

#[test]
fn lost_codex_rollout_classifier_rejects_transient_and_unrelated_failures() {
    for detail in [
        EMPTY_ROLLOUT,
        "no rollout found for thread id missing-thread",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: session metadata is missing",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: failed to parse rollout line: invalid JSON",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: No such file or directory (os error 2)",
    ] {
        assert!(
            is_lost_codex_rollout_error(&CodexResponseError::JsonRpc(detail.to_owned())),
            "{detail}"
        );
        assert!(!is_lost_codex_rollout_error(&CodexResponseError::Timeout(
            detail.to_owned()
        )));
        assert!(!is_lost_codex_rollout_error(
            &CodexResponseError::Transport(detail.to_owned())
        ));
    }
    for detail in [
        "server busy",
        "thread not loaded",
        "MCP initialization failed",
        "invalid model",
        "failed to read thread: thread-store internal error: database is locked",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: temporarily unavailable",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: timeout",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: Access is denied (os error 5)",
        "failed to read thread: thread-store internal error: failed to read session metadata /fixture: unknown failure",
        "rollout at /fixture is empty",
    ] {
        assert!(
            !is_lost_codex_rollout_error(&CodexResponseError::JsonRpc(detail.to_owned())),
            "{detail}"
        );
    }
}

struct ResumeRecoveryFixture {
    state: AppState,
    input_rx: mpsc::Receiver<CodexRuntimeCommand>,
    runtime: SharedCodexRuntime,
    session: String,
    pending: CodexPendingRequestMap,
    writer: Vec<u8>,
}

impl ResumeRecoveryFixture {
    fn new() -> Self {
        Self::with_engram(false)
    }

    fn with_engram(engram_enabled: bool) -> Self {
        let (state, input_rx) =
            test_app_state_with_delegation_codex_runtime("resume-recovery-guard");
        let session = if engram_enabled {
            let root = state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join("recovery-project");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
            let project_id = create_test_project(&state, &root, "Recovery project");
            let session = create_test_project_session(&state, Agent::Codex, &project_id, &root);
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project_id)
                .unwrap()
                .engram = Some(EngramProjectSettings {
                enabled: true,
                turn_gated_control: false,
                binary_path: Some("engram".to_owned()),
                home: Some("test-engram-home".to_owned()),
                work_authority_grant: None,
                authority_store_key: None,
                deadline_ms: None,
            });
            inner.engram_declared_project_ids.insert(project_id.clone());
            inner
                .engram_declaration_checked_project_ids
                .insert(project_id);
            session
        } else {
            test_session_id(&state, Agent::Codex)
        };
        state
            .set_external_session_id(&session, "lost-thread".to_owned())
            .unwrap();
        dispatch_turn_and_snapshot(
            &state,
            &session,
            SendMessageRequest {
                text: "retained prompt".to_owned(),
                expanded_text: None,
                attachments: vec![],
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .unwrap();
        let CodexRuntimeCommand::Prompt { command, .. } =
            phase_sync::receive(&input_rx, "prompt delivery")
        else {
            panic!("expected prompt")
        };
        let runtime = state
            .shared_codex_runtime
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let mut writer = Vec::new();
        handle_shared_codex_prompt_command(
            &mut writer,
            &pending,
            &state,
            &runtime.runtime_id,
            &test_missing_shared_codex_home(),
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
            None,
            &session,
            command,
        )
        .unwrap();
        Self {
            state,
            input_rx,
            runtime,
            session,
            pending,
            writer,
        }
    }

    fn fail_resume(&self, error: CodexResponseError) {
        let (_, response) = take_pending_codex_request(&self.pending);
        response.send(Err(error)).unwrap();
    }
}

#[test]
fn shared_codex_lost_rollout_uses_normal_engram_bootstrap_and_does_not_loop_on_start_failure() {
    let mut f = ResumeRecoveryFixture::with_engram(true);
    f.fail_resume(CodexResponseError::JsonRpc(EMPTY_ROLLOUT.to_owned()));
    let CodexRuntimeCommand::RecoverLostThread {
        session_id,
        request_id,
        thread_id,
        detail,
    } = phase_sync::receive(&f.input_rx, "resume recovery")
    else {
        panic!("expected recovery")
    };
    handle_shared_codex_lost_thread_recovery(
        &mut f.writer,
        &f.pending,
        &f.state,
        &f.runtime.runtime_id,
        &test_missing_shared_codex_home(),
        &f.runtime.sessions,
        &f.runtime.thread_sessions,
        &f.runtime.input_tx,
        None,
        &session_id,
        &request_id,
        &thread_id,
        &detail,
    )
    .unwrap();
    let requests: Vec<Value> = std::str::from_utf8(&f.writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|v| v["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["thread/resume", "config/read"]
    );
    let (_, response) = take_pending_codex_request(&f.pending);
    let base = "  preserve user/project instructions\r\nwith whitespace  \n";
    response
        .send(Ok(json!({"config":{"developer_instructions":base}})))
        .unwrap();
    let CodexRuntimeCommand::StartThreadAfterConfig {
        session_id,
        request_id,
        params,
    } = phase_sync::receive(&f.input_rx, "bootstrap continuation")
    else {
        panic!("expected config continuation")
    };
    assert_eq!(
        params["developerInstructions"],
        format!("{base}\n\n{CODEX_ENGRAM_RECOVERY_BOOTSTRAP}")
    );
    assert!(params["config"]["mcp_servers"].get("engram").is_some());
    handle_shared_codex_start_thread_after_config(
        &mut f.writer,
        &f.pending,
        &f.state,
        &f.runtime.runtime_id,
        &f.runtime.sessions,
        &f.runtime.thread_sessions,
        &f.runtime.input_tx,
        None,
        &session_id,
        request_id,
        params,
    )
    .unwrap();
    let (_, response) = take_pending_codex_request(&f.pending);
    response
        .send(Err(CodexResponseError::JsonRpc(EMPTY_ROLLOUT.to_owned())))
        .unwrap();
    let guard = phase_sync::PollGuard::new();
    loop {
        assert!(
            !matches!(
                f.input_rx.try_recv(),
                Ok(CodexRuntimeCommand::RecoverLostThread { .. })
            ),
            "thread/start must never trigger recursive recovery"
        );
        if f.state.get_session(&f.session).unwrap().session.status == SessionStatus::Error {
            break;
        }
        guard.wait(format_args!("fresh start failure must be visible"));
    }
    assert_eq!(
        f.state
            .get_session(&f.session)
            .unwrap()
            .session
            .external_session_id,
        None
    );
    assert!(
        f.runtime
            .sessions
            .lock()
            .unwrap()
            .get(&f.session)
            .unwrap()
            .pending_thread_setup
            .is_none()
    );
    let requests: Vec<Value> = std::str::from_utf8(&f.writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        requests
            .iter()
            .map(|v| v["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["thread/resume", "config/read", "thread/start"]
    );
}

#[test]
fn shared_codex_transient_resume_failures_keep_the_external_thread_id() {
    for error in [
        CodexResponseError::JsonRpc("thread-store internal error: database is locked".to_owned()),
        CodexResponseError::JsonRpc("server busy".to_owned()),
        CodexResponseError::Timeout("resume timed out".to_owned()),
        CodexResponseError::Transport("connection lost".to_owned()),
    ] {
        let fixture = ResumeRecoveryFixture::new();
        fixture.fail_resume(error.clone());
        let guard = phase_sync::PollGuard::new();
        loop {
            assert!(
                !matches!(
                    fixture.input_rx.try_recv(),
                    Ok(CodexRuntimeCommand::RecoverLostThread { .. })
                ),
                "must not recover {error:?}"
            );
            if fixture
                .state
                .get_session(&fixture.session)
                .unwrap()
                .session
                .status
                == SessionStatus::Error
            {
                break;
            }
            guard.wait(format_args!(
                "non-recoverable resume must finish failing: {error:?}"
            ));
        }
        let session = fixture.state.get_session(&fixture.session).unwrap().session;
        assert_eq!(
            session.external_session_id.as_deref(),
            Some("lost-thread"),
            "{error:?}"
        );
        assert!(
            !serde_json::to_string(&session.messages)
                .unwrap()
                .contains("Recovered unreadable")
        );
        assert_eq!(
            String::from_utf8(fixture.writer).unwrap().lines().count(),
            1
        );
    }
}

#[test]
fn shared_codex_lost_rollout_recovery_rechecks_stop_and_turn_ownership() {
    for superseded_by in [
        "stop",
        "generation",
        "thread",
        "setup",
        "runtime",
        "parked-thread",
    ] {
        let mut fixture = ResumeRecoveryFixture::new();
        fixture.fail_resume(CodexResponseError::JsonRpc(EMPTY_ROLLOUT.to_owned()));
        let CodexRuntimeCommand::RecoverLostThread {
            session_id,
            request_id,
            thread_id,
            detail,
        } = phase_sync::receive(&fixture.input_rx, "queued recovery")
        else {
            panic!("expected recovery")
        };
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_visible_session_index(&session_id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            match superseded_by {
                "stop" => record.runtime_stop_in_progress = true,
                "generation" => record.active_turn_generation += 1,
                "thread" | "parked-thread" => {
                    set_record_external_session_id(record, Some("new-owner-thread".to_owned()))
                }
                "runtime" => record.runtime = SessionRuntime::None,
                "setup" => (),
                _ => unreachable!(),
            }
        }
        if superseded_by == "setup" {
            fixture
                .runtime
                .sessions
                .lock()
                .unwrap()
                .get_mut(&session_id)
                .unwrap()
                .pending_thread_setup
                .as_mut()
                .unwrap()
                .request_id = "new-owner-request".to_owned();
        }
        if superseded_by == "parked-thread" {
            fixture
                .runtime
                .sessions
                .lock()
                .unwrap()
                .get_mut(&session_id)
                .unwrap()
                .pending_thread_setup
                .as_mut()
                .unwrap()
                .command
                .resume_thread_id = Some("new-owner-thread".to_owned());
        }
        let before =
            serde_json::to_value(fixture.state.get_session(&session_id).unwrap().session).unwrap();
        handle_shared_codex_lost_thread_recovery(
            &mut fixture.writer,
            &fixture.pending,
            &fixture.state,
            &fixture.runtime.runtime_id,
            &test_missing_shared_codex_home(),
            &fixture.runtime.sessions,
            &fixture.runtime.thread_sessions,
            &fixture.runtime.input_tx,
            None,
            &session_id,
            &request_id,
            &thread_id,
            &detail,
        )
        .unwrap();
        let after = fixture.state.get_session(&session_id).unwrap().session;
        if matches!(superseded_by, "thread" | "parked-thread") {
            assert_eq!(after.status, SessionStatus::Error, "{superseded_by}");
            assert!(
                serde_json::to_string(&after.messages)
                    .unwrap()
                    .contains(EMPTY_ROLLOUT)
            );
            assert_eq!(
                after.external_session_id.as_deref(),
                Some("new-owner-thread")
            );
        } else {
            assert_eq!(
                serde_json::to_value(after).unwrap(),
                before,
                "{superseded_by}"
            );
        }
        assert_eq!(
            String::from_utf8(fixture.writer).unwrap().lines().count(),
            1,
            "{superseded_by}"
        );
        let sessions = fixture.runtime.sessions.lock().unwrap();
        let pending = &sessions.get(&session_id).unwrap().pending_thread_setup;
        if superseded_by == "setup" {
            assert_eq!(pending.as_ref().unwrap().request_id, "new-owner-request");
        } else {
            assert!(
                pending.is_none(),
                "{superseded_by}: rejected recovery must release its failed setup so a later prompt cannot park forever"
            );
        }
    }
}
