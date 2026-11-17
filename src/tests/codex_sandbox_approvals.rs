//! Real Codex request/response and outgoing-wire regressions for the active sandbox.
//! Request shapes come from the installed 0.167.1 experimental JSON schema;
//! paths, request IDs and timestamps are synthetic, not a captured provider run.

use super::*;

fn sandbox_session(active: CodexSandboxMode, configured: CodexSandboxMode) -> (AppState, String) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let state = test_app_state();
    let id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Active;
        record.codex_approval_policy = CodexApprovalPolicy::AutoApprove;
        record.session.approval_policy = Some(CodexApprovalPolicy::AutoApprove);
        record.active_codex_approval_policy = Some(CodexApprovalPolicy::AutoApprove);
        record.active_codex_sandbox_mode = Some(active);
        record.codex_sandbox_mode = configured;
        record.session.sandbox_mode = Some(configured);
    }
    (state, id)
}

fn sandbox_request(shape: &str, state: &AppState) -> Value {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/codex-sandbox-approvals.json")).unwrap();
    let mut request = fixtures[shape].clone();
    assert!(request.is_object());
    if request["params"].get("cwd").is_some() {
        request["params"]["cwd"] = json!(state.default_workdir);
    }
    request
}

fn answer_sandbox_request(state: &AppState, id: &str, request: &Value) -> Value {
    let (tx, rx) = mpsc::channel();
    assert!(
        try_auto_respond_codex_approval_request(
            request["method"].as_str().unwrap(),
            request,
            state,
            id,
            None,
            false,
            &tx,
        )
        .unwrap()
    );
    let CodexRuntimeCommand::JsonRpcResponse { response } =
        recv_within_guard(&rx, "sandbox approval must answer without waiting").unwrap()
    else {
        panic!("expected an approval response");
    };
    let wire = codex_json_rpc_response_message(&response);
    assert_eq!(wire["id"], request["id"], "opaque routing ID must survive");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    assert!(record.pending_codex_approvals.is_empty());
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(!record.session.messages.iter().any(|message| matches!(
        message,
        Message::Approval {
            decision: ApprovalDecision::Pending,
            ..
        }
    )));
    wire["result"].clone()
}

fn assert_sandbox_reason(state: &AppState, id: &str, mode: &str) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    let reasons: Vec<_> = record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::System,
                text,
                ..
            } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "a refusal needs a visible session reason");
    let reason = reasons[0].to_lowercase();
    assert!(
        reason.contains(mode),
        "reason must identify the active sandbox: {reason}"
    );
    assert!(reason.contains("sandbox") && reason.contains("request"));
    assert!(reason.contains("setting") || reason.contains("change"));
    assert!(reason.chars().count() <= 2000, "reason must stay bounded");
}

#[test]
fn active_read_only_cap_refuses_network_even_after_full_access_is_selected_for_next_turn() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::ReadOnly,
        CodexSandboxMode::DangerFullAccess,
    );
    let request = sandbox_request("permissions", &state);
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions": {}, "scope": "turn"})
    );
    assert_sandbox_reason(&state, &id, "read-only");
}

#[test]
fn read_only_cap_refuses_explicit_file_write_permissions() {
    let (state, id) = sandbox_session(CodexSandboxMode::ReadOnly, CodexSandboxMode::ReadOnly);
    let mut request = sandbox_request("permissions", &state);
    request["params"]["permissions"] = json!({"fileSystem": {"write": [state.default_workdir]}});
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions": {}, "scope": "turn"})
    );
    assert_sandbox_reason(&state, &id, "read-only");
}

#[test]
fn workspace_cap_refuses_command_additional_network_permissions() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let request = sandbox_request("command", &state);
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"decision": "decline"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}

#[test]
fn workspace_cap_refuses_managed_network_approval_without_a_shell_preview() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let request = sandbox_request("network", &state);
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"decision": "decline"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}

#[test]
fn workspace_cap_refuses_a_file_grant_root_outside_its_workdir() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let outside = TestTempRoot::create("codex-outside-sandbox");
    let mut request = sandbox_request("file", &state);
    request["params"]["grantRoot"] = json!(outside.path().to_string_lossy());
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"decision": "decline"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}

fn explicit_write_request(shape: &str, state: &AppState, path: &str) -> Value {
    let mut request = sandbox_request(
        match shape {
            "file" => "file",
            "command" => "command",
            _ => "permissions",
        },
        state,
    );
    let profile = match shape {
        "entry" => json!({"fileSystem":{"entries":[{
            "path":{"type":"path","path":path},"access":"write"
        }]}}),
        _ => json!({"fileSystem":{"write":[path]}}),
    };
    match shape {
        "file" => request["params"]["grantRoot"] = json!(path),
        "command" => request["params"]["additionalPermissions"] = profile,
        _ => request["params"]["permissions"] = profile,
    }
    request
}

#[test]
fn restricted_caps_refuse_unestablished_writes_in_every_request_shape() {
    for mode in [CodexSandboxMode::ReadOnly, CodexSandboxMode::WorkspaceWrite] {
        for shape in ["file", "command", "write", "entry"] {
            let (state, id) = sandbox_session(mode, mode);
            // A normal-looking child gets the same no-lookup decision as an
            // alias or an absent path. Native-capped plain asks are separate.
            let path = FsPath::new(&state.default_workdir).join("unconfirmed-child/file.txt");
            let request = explicit_write_request(shape, &state, &path.to_string_lossy());
            let expected = if matches!(shape, "write" | "entry") {
                json!({"permissions":{},"scope":"turn"})
            } else {
                json!({"decision":"decline"})
            };
            assert_eq!(answer_sandbox_request(&state, &id, &request), expected);
            assert_sandbox_reason(&state, &id, mode.as_cli_value());
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
            assert!(
                record
                    .session
                    .messages
                    .iter()
                    .any(|message| matches!(message,
                        Message::Text { author: Author::System, text, .. }
                            if text.contains("unconfirmed-child")
                    ))
            );
        }
    }
}

#[test]
fn full_access_still_answers_explicit_write_grants_in_every_request_shape() {
    for shape in ["file", "command", "write", "entry"] {
        let (state, id) = sandbox_session(
            CodexSandboxMode::DangerFullAccess,
            CodexSandboxMode::ReadOnly,
        );
        let path = FsPath::new(&state.default_workdir).join("ordinary-file.txt");
        let request = explicit_write_request(shape, &state, &path.to_string_lossy());
        let expected = if matches!(shape, "write" | "entry") {
            json!({"permissions":request["params"]["permissions"],"scope":"turn"})
        } else {
            json!({"decision":"accept"})
        };
        assert_eq!(answer_sandbox_request(&state, &id, &request), expected);
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&id).unwrap()]
                .session
                .messages
                .is_empty()
        );
    }
}

#[test]
fn unestablished_write_refusal_bounds_unicode_path_and_removes_controls() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let path = FsPath::new(&state.default_workdir).join(format!(
        "visible-path-{}{}",
        char::from(0x1b),
        "界".repeat(9000)
    ));
    let request = explicit_write_request("file", &state, &path.to_string_lossy());
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"decision":"decline"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
    let inner = state.inner.lock().unwrap();
    let reason = inner.sessions[inner.find_session_index(&id).unwrap()]
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Text {
                author: Author::System,
                text,
                ..
            } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(reason.contains("visible-path-") && reason.contains('…'));
    assert!(!reason.chars().any(char::is_control));
}

#[cfg(windows)]
#[test]
fn restricted_cap_refuses_network_device_and_protected_alias_spellings() {
    for path in [
        r"\\host.invalid\share\file.txt",
        r"\\host.invalid@80\dav\file.txt",
        r"\\?\UNC\host.invalid\share\file.txt",
        r"\\.\C:\workspace\file.txt",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\file.txt",
        r"C:\workspace\.codex.\config.toml",
        r"C:\workspace\.agents \rules",
    ] {
        let (state, id) = sandbox_session(
            CodexSandboxMode::WorkspaceWrite,
            CodexSandboxMode::WorkspaceWrite,
        );
        let request = explicit_write_request("file", &state, path);
        assert_eq!(
            answer_sandbox_request(&state, &id, &request),
            json!({"decision":"decline"})
        );
        assert_sandbox_reason(&state, &id, "workspace-write");
    }
}

#[test]
fn read_only_cap_answers_explicit_read_permissions_without_a_card() {
    let (state, id) = sandbox_session(CodexSandboxMode::ReadOnly, CodexSandboxMode::ReadOnly);
    let mut request = sandbox_request("permissions", &state);
    request["params"]["permissions"] = json!({"fileSystem": {"read": [state.default_workdir]}});
    let expected = json!({"permissions": request["params"]["permissions"], "scope": "turn"});
    assert_eq!(answer_sandbox_request(&state, &id, &request), expected);
}

#[test]
fn workspace_cap_refuses_explicit_workspace_write_permissions_without_a_card() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let mut request = sandbox_request("permissions", &state);
    request["params"]["permissions"] =
        json!({"fileSystem": {"write": [FsPath::new(&state.default_workdir).join("allowed.txt")]}});
    let expected = json!({"permissions": {}, "scope": "turn"});
    assert_eq!(answer_sandbox_request(&state, &id, &request), expected);
    assert_sandbox_reason(&state, &id, "workspace-write");
}

#[test]
fn active_full_access_is_not_replaced_by_a_next_turn_read_only_setting() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::DangerFullAccess,
        CodexSandboxMode::ReadOnly,
    );
    for shape in ["permissions", "command", "network", "file"] {
        let request = sandbox_request(shape, &state);
        let expected = if shape == "permissions" {
            json!({"permissions": request["params"]["permissions"], "scope": "turn"})
        } else {
            json!({"decision": "accept"})
        };
        assert_eq!(answer_sandbox_request(&state, &id, &request), expected);
    }
}

#[test]
fn stopped_or_stale_sandbox_requests_are_declined_without_a_new_refusal_notice() {
    for stopped in [false, true] {
        let (state, id) = sandbox_session(CodexSandboxMode::ReadOnly, CodexSandboxMode::ReadOnly);
        if stopped {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            inner.sessions[index].runtime_stop_in_progress = true;
        }
        let request = sandbox_request("permissions", &state);
        let (tx, rx) = mpsc::channel();
        assert!(
            try_auto_respond_codex_approval_request(
                request["method"].as_str().unwrap(),
                &request,
                &state,
                &id,
                if stopped { None } else { Some("stale-runtime") },
                false,
                &tx
            )
            .unwrap()
        );
        let CodexRuntimeCommand::JsonRpcResponse { response } = rx.try_recv().unwrap() else {
            panic!("expected rejection");
        };
        assert_eq!(
            codex_json_rpc_response_message(&response)["result"],
            json!({"permissions": {}, "scope": "turn"})
        );
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.sessions[inner.find_session_index(&id).unwrap()]
                .session
                .messages
                .is_empty()
        );
    }
}

#[test]
fn genuine_user_input_is_not_answered_by_sandbox_auto_approval() {
    let (state, id) = sandbox_session(CodexSandboxMode::ReadOnly, CodexSandboxMode::ReadOnly);
    let (tx, rx) = mpsc::channel();
    assert!(
        !try_auto_respond_codex_approval_request(
            "item/tool/requestUserInput",
            &json!({"id":"question","params":{"questions":[]}}),
            &state,
            &id,
            None,
            false,
            &tx
        )
        .unwrap()
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn native_capped_auto_approve_answers_plain_command_and_file_asks_inside_the_sandbox() {
    for mode in [CodexSandboxMode::ReadOnly, CodexSandboxMode::WorkspaceWrite] {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ] {
            let (state, id) = sandbox_session(mode, mode);
            let request = json!({
                "id":"opaque-inside-cap", "method":method,
                "params":{"reason":"Approval required by the command rule"}
            });
            assert_eq!(
                answer_sandbox_request(&state, &id, &request),
                json!({"decision":"accept"})
            );
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
            assert_eq!(record.active_codex_sandbox_mode, Some(mode));
            assert!(record.session.messages.is_empty());
        }
    }
}

#[test]
fn plain_asks_from_an_older_native_policy_cannot_inherit_the_next_turn_cap() {
    for active_policy in [None, Some(CodexApprovalPolicy::OnRequest)] {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ] {
            let (state, id) = sandbox_session(
                CodexSandboxMode::ReadOnly,
                CodexSandboxMode::DangerFullAccess,
            );
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&id).unwrap();
                inner.sessions[index].active_codex_approval_policy = active_policy;
            }
            let request = json!({"id":"older-native-ask","method":method,"params":{}});
            assert_eq!(
                answer_sandbox_request(&state, &id, &request),
                json!({"decision":"decline"})
            );
            assert_sandbox_reason(&state, &id, "read-only");
        }
    }
}

#[test]
fn restricted_auto_approve_keeps_mcp_tools_usable_without_permission_cards() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let home = TestTempRoot::create("codex-sandbox-mcp-home");
    for thread in [None, Some("sandbox-thread")] {
        let setup = super::shared_codex_thread_setup::shared_codex_setup_request_for_policy_test(
            home.path(),
            thread,
            false,
            false,
            CodexApprovalPolicy::AutoApprove,
        );
        assert_eq!(setup["params"]["sandbox"], "workspace-write");
        let policy = &setup["params"]["approvalPolicy"]["granular"];
        assert_eq!(
            policy["mcp_elicitations"], true,
            "native policy must forward MCP asks to the host rather than auto-reject them"
        );
        assert_eq!(policy["sandbox_approval"], false);
        assert_eq!(policy["request_permissions"], true);
        assert_eq!(policy["rules"], true);
        assert_eq!(policy["skill_approval"], true);
    }
    for mode in [CodexSandboxMode::ReadOnly, CodexSandboxMode::WorkspaceWrite] {
        let (state, id) = sandbox_session(mode, mode);
        let request = json!({
            "id": "restricted-mcp-tool",
            "method": "mcpServer/elicitation/request",
            "params": {
                "threadId": "sandbox-thread",
                "turnId": "sandbox-turn",
                "serverName": "routine-tools", "mode": "form",
                "message": "Approve tool call?",
                "requestedSchema": {"type": "object", "properties": {}},
                "_meta": {
                    "codex_approval_kind": "mcp_tool_call",
                    "tool_description": "Inspect work", "tool_params": {}
                }
            }
        });
        let result = answer_sandbox_request(&state, &id, &request);
        assert_eq!(result["action"], "accept");
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert_eq!(record.active_codex_sandbox_mode, Some(mode));
        assert!(record.session.messages.is_empty());
    }
}

#[test]
fn restricted_auto_approve_thread_start_keeps_the_cap_without_native_approval_waits() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let home = TestTempRoot::create("codex-sandbox-start-home");
    let request = super::shared_codex_thread_setup::shared_codex_setup_request_for_policy_test(
        home.path(),
        None,
        false,
        false,
        CodexApprovalPolicy::AutoApprove,
    );
    assert_eq!(request["method"], "thread/start");
    assert_eq!(request["params"]["sandbox"], "workspace-write");
    assert_eq!(
        request["params"]["approvalPolicy"],
        json!({"granular": {"sandbox_approval": false, "rules": true, "mcp_elicitations": true, "request_permissions": true, "skill_approval": true}})
    );
}

#[test]
fn restricted_auto_approve_resume_keeps_the_cap_without_native_approval_waits() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let home = TestTempRoot::create("codex-sandbox-resume-home");
    let request = super::shared_codex_thread_setup::shared_codex_setup_request_for_policy_test(
        home.path(),
        Some("sandbox-thread"),
        false,
        false,
        CodexApprovalPolicy::AutoApprove,
    );
    assert_eq!(request["method"], "thread/resume");
    assert_eq!(request["params"]["sandbox"], "workspace-write");
    assert_eq!(
        request["params"]["approvalPolicy"],
        json!({"granular": {"sandbox_approval": false, "rules": true, "mcp_elicitations": true, "request_permissions": true, "skill_approval": true}})
    );
}

fn sandbox_turn_start_request(mode: CodexSandboxMode) -> Value {
    let (state, id) = sandbox_session(mode, mode);
    let (runtime, _rx, process) = test_shared_codex_runtime("sandbox-turn-policy");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: id.clone(),
            }),
        });
    }
    let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    handle_shared_codex_start_turn(
        &mut writer,
        &pending,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        None,
        &id,
        "sandbox-thread",
        None,
        CodexPromptCommand {
            active_turn_generation: 0,
            approval_policy: CodexApprovalPolicy::AutoApprove,
            attachments: Vec::new(),
            cwd: state.default_workdir.clone(),
            model: "test-model".to_owned(),
            prompt: "work within this sandbox".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            service_tier: None,
            resume_thread_id: None,
            sandbox_mode: mode,
            delivery_receipt: None,
        },
    )
    .unwrap();
    let request: Value = serde_json::from_slice(&writer).unwrap();
    let (_, sender) = take_pending_codex_request(&pending);
    sender
        .send(Ok(json!({"turn":{"id":"sandbox-turn"}})))
        .unwrap();
    request
}

#[test]
fn restricted_auto_approve_turn_start_keeps_the_cap_without_native_approval_waits() {
    for (mode, native_type) in [
        (CodexSandboxMode::ReadOnly, "readOnly"),
        (CodexSandboxMode::WorkspaceWrite, "workspaceWrite"),
    ] {
        let request = sandbox_turn_start_request(mode);
        assert_eq!(request["method"], "turn/start");
        assert_eq!(request["params"]["sandboxPolicy"]["type"], native_type);
        assert_eq!(
            request["params"]["approvalPolicy"],
            json!({"granular": {"sandbox_approval": false, "rules": true, "mcp_elicitations": true, "request_permissions": true, "skill_approval": true}})
        );
    }
}

#[test]
fn full_access_turn_start_keeps_native_requests_enabled_for_host_answers() {
    let request = sandbox_turn_start_request(CodexSandboxMode::DangerFullAccess);
    assert_eq!(
        request["params"]["sandboxPolicy"]["type"],
        "dangerFullAccess"
    );
    assert_eq!(request["params"]["approvalPolicy"], "on-request");
}

#[test]
fn workspace_cap_refuses_writes_to_protected_metadata() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let mut request = sandbox_request("permissions", &state);
    request["params"]["permissions"] = json!({"fileSystem":{"entries":[{
        "path":{"type":"path","path":FsPath::new(&state.default_workdir).join(".git/config")},
        "access":"write"
    }]}});
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions":{},"scope":"turn"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}

#[test]
fn workspace_cap_does_not_grant_its_entire_root_including_protected_metadata() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let mut request = sandbox_request("permissions", &state);
    request["params"]["permissions"] = json!({"fileSystem":{"write":[state.default_workdir]}});
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions":{},"scope":"turn"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}

#[test]
fn unknown_active_sandbox_cannot_inherit_a_configured_full_access_grant() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::ReadOnly,
        CodexSandboxMode::DangerFullAccess,
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].active_codex_sandbox_mode = None;
    }
    let request = sandbox_request("permissions", &state);
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions":{},"scope":"turn"})
    );
    assert_sandbox_reason(&state, &id, "unconfirmed");
}

#[test]
fn native_human_command_checkpoint_stays_an_explicit_decision() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::DangerFullAccess,
        CodexSandboxMode::DangerFullAccess,
    );
    let mut request = sandbox_request("command", &state);
    request["params"]["kind"] = json!("humanCommand");
    let (tx, rx) = mpsc::channel();
    assert!(
        !try_auto_respond_codex_approval_request(
            request["method"].as_str().unwrap(),
            &request,
            &state,
            &id,
            None,
            false,
            &tx
        )
        .unwrap()
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn sandbox_refusal_reaches_clients_with_its_session_mutation_stamp() {
    let (state, id) = sandbox_session(CodexSandboxMode::ReadOnly, CodexSandboxMode::ReadOnly);
    let mut events = state.subscribe_delta_events();
    let stamp_before = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&id).unwrap()].mutation_stamp
    };
    let request = sandbox_request("permissions", &state);
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"permissions":{},"scope":"turn"})
    );
    let mut published = false;
    while let Ok(payload) = events.try_recv() {
        if let DeltaEvent::MessageCreated {
            session_id,
            message:
                Message::Text {
                    author: Author::System,
                    text,
                    ..
                },
            status,
            session_mutation_stamp: Some(stamp),
            ..
        } = serde_json::from_str(&payload).unwrap()
        {
            if session_id == id && text.contains("network access") {
                assert_eq!(status, SessionStatus::Active);
                assert!(stamp > stamp_before);
                assert!(text.chars().count() <= 2000);
                published = true;
            }
        }
    }
    assert!(
        published,
        "the refusal must be published, not only retained in memory"
    );
}

#[cfg(unix)]
#[test]
fn workspace_cap_refuses_a_symlink_target_outside_the_workspace() {
    let (state, id) = sandbox_session(
        CodexSandboxMode::WorkspaceWrite,
        CodexSandboxMode::WorkspaceWrite,
    );
    let outside = TestTempRoot::create("codex-sandbox-symlink-target");
    let link = FsPath::new(&state.default_workdir).join("linked");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();
    let mut request = sandbox_request("file", &state);
    request["params"]["grantRoot"] = json!(link.join("new-file.txt"));
    assert_eq!(
        answer_sandbox_request(&state, &id, &request),
        json!({"decision":"decline"})
    );
    assert_sandbox_reason(&state, &id, "workspace-write");
}
