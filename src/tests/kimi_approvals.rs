//! Tests TermAl's Kimi approval policy, Kimi's own mode and the default
//! reasoning effort (kimi_approvals.rs, kimi.rs, session settings, creation,
//! preferences, orchestrators and delegation children). Fixtures use the
//! request shapes from the Kimi Code 2.0.2 captures, including AskUserQuestion,
//! whose answers are `allow_once` options.
//!
//! Owns these tests only. New module alongside kimi_approvals.rs.

use super::*;

const RUNTIME: &str = "kimi-approvals-runtime";

fn create_kimi(state: &AppState, extra: Value) -> String {
    let mut request = json!({"agent": "Kimi", "workdir": "/tmp"});
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    state
        .create_session_with_agent_setup_validator(
            serde_json::from_value(request).unwrap(),
            |_, _| Ok(()),
        )
        .unwrap()
        .session_id
}

fn set_status(state: &AppState, id: &str, status: SessionStatus) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(id).unwrap();
    inner.sessions[index].session.status = status;
}

fn session(state: &AppState, id: &str) -> Session {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(id).unwrap()]
        .session
        .clone()
}

/// A Kimi session with a live runtime, Active, with the given TermAl policy.
fn running_kimi(policy: &str) -> (AppState, String, mpsc::Receiver<AcpRuntimeCommand>, AcpRuntimeHandle) {
    let state = test_app_state();
    let id = create_kimi(&state, json!({ "kimiApprovalMode": policy }));
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::Kimi, RUNTIME);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    (state, id, rx, runtime)
}

fn permission(title: &str, options: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 9, "method": "session/request_permission",
        "params": { "sessionId": "session_x", "options": options,
            "toolCall": { "toolCallId": "c1", "title": title,
                "content": [{ "type": "content", "content": { "type": "text", "text": "x" } }] } } })
}

fn tool_options() -> Value {
    json!([
        { "optionId": "approve_once", "name": "Approve once", "kind": "allow_once" },
        { "optionId": "approve_always", "name": "Approve for this session", "kind": "allow_always" },
        { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
    ])
}

/// The AskUserQuestion shape from the capture: each answer is `allow_once`.
fn question_options() -> Value {
    json!([
        { "optionId": "q0_opt_0", "name": "Red", "kind": "allow_once" },
        { "optionId": "q0_opt_1", "name": "Blue", "kind": "allow_once" },
        { "optionId": "q0_skip", "name": "Skip", "kind": "reject_once" }
    ])
}

fn deliver(state: &AppState, id: &str, runtime: &AcpRuntimeHandle, message: Value, token: &str) {
    handle_acp_message(
        &message,
        state,
        id,
        &RuntimeToken::Acp(token.to_owned()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        &mut AcpTurnState::default(),
        &mut SessionRecorder::new(state.clone(), id.to_owned()),
        AcpAgent::Kimi,
    )
    .expect("the message should be handled");
}

fn answered(rx: &mpsc::Receiver<AcpRuntimeCommand>) -> Option<Value> {
    rx.try_iter().find_map(|command| match command {
        AcpRuntimeCommand::JsonRpcMessage(message) => Some(message["result"]["outcome"].clone()),
        _ => None,
    })
}

fn manual_cards(state: &AppState, id: &str) -> usize {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(id).unwrap()]
        .pending_acp_approvals
        .len()
}

#[test]
fn exactly_one_allow_once_option_is_required() {
    assert_eq!(
        kimi_single_allow_once_option(tool_options().as_array().unwrap()).as_deref(),
        Some("approve_once")
    );
    assert_eq!(
        kimi_single_allow_once_option(question_options().as_array().unwrap()),
        None,
        "two answers are a choice, never approved"
    );
    let only_always = json!([{ "optionId": "a", "kind": "allow_always" }]);
    assert_eq!(kimi_single_allow_once_option(only_always.as_array().unwrap()), None);
    let empty_id = json!([{ "optionId": "", "kind": "allow_once" }]);
    assert_eq!(kimi_single_allow_once_option(empty_id.as_array().unwrap()), None);
}

#[test]
fn only_allowlisted_tool_titles_are_auto_approvable() {
    for title in ["Bash", "Write", "Edit", "CronCreate", "mcp__termal-delegation__termal_send_to_session", "mcp__a_b__c-d"] {
        assert!(kimi_auto_approvable_title(title), "{title}");
    }
    for title in ["AskUserQuestion", "ExitPlanMode", "Agent", "mcp__", "mcp__server", "mcp____tool", "mcp__a b__c", "bash"] {
        assert!(!kimi_auto_approvable_title(title), "{title}");
    }
}

#[test]
fn auto_approve_answers_an_allowlisted_tool_once() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    deliver(&state, &id, &runtime, permission("Bash", tool_options()), RUNTIME);
    assert_eq!(
        answered(&rx),
        Some(json!({ "outcome": "selected", "optionId": "approve_once" })),
        "allow_once, never allow_always"
    );
    assert_eq!(manual_cards(&state, &id), 0);
}

#[test]
fn questions_plans_and_ask_policy_stay_manual() {
    for (policy, title, options) in [
        ("auto-approve", "AskUserQuestion", question_options()),
        // One answer plus Skip passes the count rule; the title keeps it manual.
        ("auto-approve", "AskUserQuestion", json!([
            { "optionId": "q0_opt_0", "name": "Yes", "kind": "allow_once" },
            { "optionId": "q0_skip", "name": "Skip", "kind": "reject_once" }
        ])),
        ("auto-approve", "ExitPlanMode", json!([
            { "optionId": "plan_approve", "kind": "allow_once" },
            { "optionId": "plan_revise", "kind": "reject_once" },
            { "optionId": "plan_reject_and_exit", "kind": "reject_once" }
        ])),
        ("auto-approve", "SomeFutureTool", tool_options()),
        ("ask", "Bash", tool_options()),
    ] {
        let (state, id, rx, runtime) = running_kimi(policy);
        deliver(&state, &id, &runtime, permission(title, options), RUNTIME);
        assert_eq!(answered(&rx), None, "{policy} {title}: TermAl answers nothing");
        assert_eq!(manual_cards(&state, &id), 1, "{policy} {title}: a manual card");
    }
}

#[test]
fn a_pending_card_suspends_auto_approve() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    set_status(&state, &id, SessionStatus::Approval);
    deliver(&state, &id, &runtime, permission("Bash", tool_options()), RUNTIME);
    assert_eq!(answered(&rx), None);
    assert_eq!(manual_cards(&state, &id), 1);
}

#[test]
fn a_stopping_or_stale_runtime_is_cancelled_never_approved() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime_stop_in_progress = true;
    }
    deliver(&state, &id, &runtime, permission("Bash", tool_options()), RUNTIME);
    assert_eq!(answered(&rx), Some(json!({ "outcome": "cancelled" })));

    let (state, id, rx, runtime) = running_kimi("auto-approve");
    deliver(&state, &id, &runtime, permission("Bash", tool_options()), "another-runtime");
    assert_eq!(answered(&rx), Some(json!({ "outcome": "cancelled" })));
}

#[test]
fn the_read_only_gate_wins_over_the_session_policy() {
    let (state, id, rx, runtime) = running_kimi("auto-approve");
    {
        let mut inner = state.inner.lock().unwrap();
        let parent = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);
        let parent_id = parent.session.id.clone();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("delegation-gate".to_owned());
        inner.delegations.push(DelegationRecord {
            id: "delegation-gate".to_owned(),
            parent_session_id: parent_id,
            child_session_id: id.clone(),
            mode: DelegationMode::Reviewer,
            status: DelegationStatus::Running,
            title: "gate".to_owned(),
            prompt: "/review-code".to_owned(),
            cwd: "/tmp".to_owned(),
            agent: Agent::Kimi,
            model: None,
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: stamp_now(),
            started_at: Some(stamp_now()),
            completed_at: None,
            result: None,
            submitted_review_result: None,
            post_submission_transport_error: None,
            review_result_recovery_probe_attempt: None,
            review_result_recovery_error: None,
            review_result_schema_version: None,
            queued_followup_prompt_id: None,
            review_result_submission_attempt: 1,
            acceptance_evaluation: None,
        });
        let delegation_index = inner.delegations.len() - 1;
        inner.mark_delegation_mutated(delegation_index);
        inner.sync_running_read_only_delegation_index(delegation_index);
    }
    // A write the auto-approve policy would allow: the gate refuses it.
    deliver(&state, &id, &runtime, permission("Write", tool_options()), RUNTIME);
    assert_eq!(
        answered(&rx),
        Some(json!({ "outcome": "selected", "optionId": "reject" }))
    );
    // And the prompt runs in `default` whatever the session's own mode says.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].session.kimi_mode = Some(KimiMode::Yolo);
    }
    assert_eq!(state.kimi_effective_mode(&id).unwrap(), KimiMode::Default);
}

#[test]
fn the_effective_mode_is_the_session_mode_for_an_ordinary_session() {
    let state = test_app_state();
    let id = create_kimi(&state, json!({ "kimiMode": "yolo" }));
    assert_eq!(state.kimi_effective_mode(&id).unwrap(), KimiMode::Yolo);
    let id = create_kimi(&state, json!({}));
    assert_eq!(state.kimi_effective_mode(&id).unwrap(), KimiMode::Default);
}

#[test]
fn creation_starts_from_the_app_defaults_and_a_request_overrides_them() {
    let state = test_app_state();
    state
        .update_app_settings(
            serde_json::from_value(json!({
                "defaultKimiApprovalMode": "auto-approve",
                "defaultKimiEffort": "high"
            }))
            .unwrap(),
        )
        .unwrap();
    let id = create_kimi(&state, json!({}));
    let created = session(&state, &id);
    assert_eq!(created.kimi_approval_mode, Some(KimiApprovalMode::AutoApprove));
    assert_eq!(created.kimi_mode, Some(KimiMode::Default));
    assert_eq!(created.kimi_effort.as_deref(), Some("high"));

    let id = create_kimi(&state, json!({ "kimiApprovalMode": "ask", "kimiMode": "plan" }));
    let created = session(&state, &id);
    assert_eq!(created.kimi_approval_mode, Some(KimiApprovalMode::Ask));
    assert_eq!(created.kimi_mode, Some(KimiMode::Plan));

    // A later Settings change never alters an existing session.
    state
        .update_app_settings(serde_json::from_value(json!({ "defaultKimiEffort": "auto" })).unwrap())
        .unwrap();
    assert_eq!(session(&state, &id).kimi_effort.as_deref(), Some("high"));
    let id = create_kimi(&state, json!({}));
    assert_eq!(session(&state, &id).kimi_effort, None, "auto leaves the CLI's choice");
}

#[test]
fn the_default_effort_must_be_auto_or_one_token() {
    assert_eq!(normalize_default_kimi_effort(" AUTO ").unwrap(), "auto");
    assert_eq!(normalize_default_kimi_effort("").unwrap(), "auto");
    assert_eq!(normalize_default_kimi_effort("max").unwrap(), "max");
    assert!(normalize_default_kimi_effort("very high").is_err());
    assert!(normalize_default_kimi_effort(&"x".repeat(65)).is_err());
}

#[test]
fn a_rejected_default_effort_changes_no_preference() {
    let state = test_app_state();
    let revision = state.inner.lock().unwrap().revision;
    let error = state
        .update_app_settings(
            serde_json::from_value(json!({
                "defaultKimiApprovalMode": "auto-approve",
                "defaultKimiEffort": "very high"
            }))
            .unwrap(),
        )
        .err()
        .expect("an effort with a space is rejected");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.preferences.default_kimi_approval_mode,
            KimiApprovalMode::Ask,
            "the policy in the same request is not applied"
        );
        assert_eq!(inner.revision, revision);
    }
    let id = create_kimi(&state, json!({}));
    assert_eq!(session(&state, &id).kimi_approval_mode, Some(KimiApprovalMode::Ask));
}

#[test]
fn a_model_change_clears_the_observed_mode() {
    let state = test_app_state();
    let id = create_kimi(&state, json!({}));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].session.kimi_current_mode = Some("yolo".to_owned());
    }
    state
        .update_session_settings(&id, serde_json::from_value(json!({ "model": "model-b" })).unwrap())
        .unwrap();
    assert_eq!(session(&state, &id).kimi_current_mode, None);
}

// A dropped field would silently turn a remote auto-approve or yolo request
// into ask/default, or hide the remote session's policy in the UI.
#[test]
fn a_remote_kimi_session_keeps_its_policy_and_modes() {
    let captured_body = Arc::new(Mutex::new(None::<Value>));
    let captured_body_for_server = captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();

    // What the remote returns: its own Kimi session with the requested settings.
    let remote_state = test_app_state();
    let remote_id = create_kimi(
        &remote_state,
        json!({ "kimiApprovalMode": "auto-approve", "kimiMode": "yolo" }),
    );
    let mut remote_session = session(&remote_state, &remote_id);
    remote_session.id = "remote-kimi-session".to_owned();
    remote_session.workdir = "/remote/repo".to_owned();
    remote_session.project_id = Some("remote-project-kimi".to_owned());
    remote_session.kimi_current_mode = Some("yolo".to_owned());
    let remote_response = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 7,
        server_instance_id: "remote-server".to_owned(),
    })
    .unwrap();
    let _ = fs::remove_file(remote_state.persistence_path.as_path());

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-kimi".to_owned(),
        name: "SSH Kimi".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    state
        .update_app_settings(
            serde_json::from_value(json!({
                "remotes": [RemoteConfig::local(), remote.clone()]
            }))
            .unwrap(),
        )
        .unwrap();
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Kimi",
        "remote-project-kimi",
    );
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let server = std::thread::spawn(move || loop {
        let mut stream = accept_test_connection(&listener, "remote Kimi create listener");
        let request = read_test_http_request(&mut stream);
        if request.request_line.starts_with("GET /api/health ") {
            write_test_http_response(
                &mut stream,
                StatusCode::OK,
                "application/json",
                r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
            );
            continue;
        }
        if request.request_line.starts_with("POST /api/sessions ") {
            *captured_body_for_server.lock().unwrap() =
                Some(serde_json::from_str(&request.body).expect("create body should decode"));
            write_test_http_response(&mut stream, StatusCode::OK, "application/json", &remote_response);
            break;
        }
        panic!("unexpected request: {}", request.request_line);
    });

    let created = state
        .create_session(
            serde_json::from_value(json!({
                "agent": "Kimi",
                "projectId": local_project_id,
                "kimiApprovalMode": "auto-approve",
                "kimiMode": "yolo"
            }))
            .unwrap(),
        )
        .unwrap();

    let body = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("the remote create request should be captured");
    assert_eq!(body["kimiApprovalMode"], "auto-approve");
    assert_eq!(body["kimiMode"], "yolo");
    assert_eq!(created.session.kimi_approval_mode, Some(KimiApprovalMode::AutoApprove));
    assert_eq!(created.session.kimi_mode, Some(KimiMode::Yolo));
    assert_eq!(created.session.kimi_current_mode.as_deref(), Some("yolo"));

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn kimi_fields_belong_to_kimi_sessions_only() {
    let state = test_app_state();
    for field in ["kimiApprovalMode", "kimiMode"] {
        let mut request = json!({ "agent": "Claude", "workdir": "/tmp" });
        request[field] = json!(if field == "kimiMode" { "yolo" } else { "auto-approve" });
        let error = state
            .create_session_with_agent_setup_validator(
                serde_json::from_value(request).unwrap(),
                |_, _| Ok(()),
            )
            .err()
            .expect("rejected for Claude");
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{field}");
    }
    let claude = test_session_id(&state, Agent::Claude);
    let error = state
        .update_session_settings(
            &claude,
            serde_json::from_value(json!({ "kimiApprovalMode": "auto-approve" })).unwrap(),
        )
        .err()
        .expect("rejected for Claude");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[test]
fn the_policy_is_changed_alone_and_never_while_busy() {
    let state = test_app_state();
    let id = create_kimi(&state, json!({}));
    for mixed in [
        json!({ "kimiApprovalMode": "auto-approve", "kimiMode": "yolo" }),
        json!({ "kimiApprovalMode": "auto-approve", "kimiEffort": "auto" }),
        json!({ "kimiApprovalMode": "auto-approve", "model": "kimi-code/k3" }),
    ] {
        let error = state
            .update_session_settings(&id, serde_json::from_value(mixed.clone()).unwrap())
            .err()
            .expect("mixed payload rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{mixed}");
    }
    assert_eq!(session(&state, &id).kimi_approval_mode, Some(KimiApprovalMode::Ask));

    for status in [SessionStatus::Active, SessionStatus::Approval, SessionStatus::Stopping] {
        set_status(&state, &id, status);
        for payload in [json!({ "kimiApprovalMode": "auto-approve" }), json!({ "kimiMode": "yolo" })] {
            let error = state
                .update_session_settings(&id, serde_json::from_value(payload.clone()).unwrap())
                .err()
                .expect("busy session rejected");
            assert_eq!(error.status, StatusCode::CONFLICT, "{status:?} {payload}");
        }
    }
    set_status(&state, &id, SessionStatus::Idle);
    state
        .update_session_settings(&id, serde_json::from_value(json!({ "kimiApprovalMode": "auto-approve" })).unwrap())
        .unwrap();
    state
        .update_session_settings(&id, serde_json::from_value(json!({ "kimiMode": "auto" })).unwrap())
        .unwrap();
    let updated = session(&state, &id);
    assert_eq!(updated.kimi_approval_mode, Some(KimiApprovalMode::AutoApprove));
    assert_eq!(updated.kimi_mode, Some(KimiMode::Auto));
    let persisted = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let stored = &persisted.sessions[persisted.find_session_index(&id).unwrap()].session;
    assert_eq!(stored.kimi_approval_mode, Some(KimiApprovalMode::AutoApprove));
    assert_eq!(stored.kimi_mode, Some(KimiMode::Auto));
}

#[test]
fn a_delegation_child_never_inherits_auto_approve_or_a_non_default_mode() {
    let state = test_app_state();
    state
        .update_app_settings(serde_json::from_value(json!({ "defaultKimiApprovalMode": "auto-approve" })).unwrap())
        .unwrap();
    let mut record = {
        let mut inner = state.inner.lock().unwrap();
        inner.create_session(Agent::Kimi, None, "/tmp".to_owned(), None, None)
    };
    assert_eq!(record.session.kimi_approval_mode, Some(KimiApprovalMode::AutoApprove));
    record.session.kimi_mode = Some(KimiMode::Yolo);
    configure_delegation_child_prompt_settings(
        &mut record,
        DelegationMode::Explorer,
        &DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec![],
            worktree_path: None,
        },
    );
    assert_eq!(record.session.kimi_approval_mode, Some(KimiApprovalMode::Ask));
    assert_eq!(record.session.kimi_mode, Some(KimiMode::Default));
}

#[test]
fn a_reported_mode_is_shown_but_never_becomes_the_chosen_mode() {
    let (state, id, _rx, runtime) = running_kimi("ask");
    deliver(
        &state,
        &id,
        &runtime,
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "session_x",
            "update": { "sessionUpdate": "current_mode_update", "currentModeId": "plan" } } }),
        RUNTIME,
    );
    let shown = session(&state, &id);
    assert_eq!(shown.kimi_current_mode.as_deref(), Some("plan"));
    assert_eq!(shown.kimi_mode, Some(KimiMode::Default));
}

#[test]
fn only_kimi_sessions_may_persist_kimi_fields() {
    let state = test_app_state();
    let claude = test_session_id(&state, Agent::Claude);
    let mut session = session(&state, &claude);
    session.kimi_mode = Some(KimiMode::Yolo);
    let external = session.external_session_id.clone();
    assert!(validate_persisted_session_fields(&session, external.as_deref()).is_err());

    // A legacy Kimi record with no policy or mode loads, and reads as ask/default.
    let kimi = test_session_id(&state, Agent::Kimi);
    let mut legacy = self::session(&state, &kimi);
    legacy.kimi_approval_mode = None;
    legacy.kimi_mode = None;
    let external = legacy.external_session_id.clone();
    assert!(validate_persisted_session_fields(&legacy, external.as_deref()).is_ok());
    assert_eq!(legacy.kimi_approval_mode.unwrap_or_default(), KimiApprovalMode::Ask);
    assert_eq!(legacy.kimi_mode.unwrap_or_default(), KimiMode::Default);
}
