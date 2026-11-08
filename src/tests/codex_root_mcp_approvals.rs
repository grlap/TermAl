// Exercises generated thread requests, not a live Codex runtime. Approval
// exceptions belong to the owned root bridge; child and user policies stay put.
use super::shared_codex_thread_setup::retire_pending_codex_thread_setups;
use super::*;

fn setup(home: &FsPath, thread: Option<&str>, policy: CodexApprovalPolicy, child: bool) -> Value {
    super::shared_codex_thread_setup::shared_codex_setup_request_for_policy_test(
        home, thread, true, child, policy,
    )
}

#[test]
fn codex_never_root_start_and_resume_approve_only_coordination_tools() {
    let home = TestTempRoot::create("codex-never-root-mcp");
    fs::write(home.path().join("config.toml"), "").unwrap();
    for (method, thread) in [("thread/start", None), ("thread/resume", Some("existing"))] {
        let request = setup(home.path(), thread, CodexApprovalPolicy::Never, false);
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["approvalPolicy"], "never");
        let server = &request["params"]["config"]["mcp_servers"]["termal-delegation"];
        let approvals = server["tools"]
            .as_object()
            .expect("root coordination must be callable");
        let expected = [
            "termal_acknowledge_mailbox",
            "termal_board_get",
            "termal_board_list",
            "termal_get_session_result",
            "termal_get_session_status",
            "termal_list_delegations",
            "termal_list_mailboxes",
            "termal_list_sessions",
            "termal_read_mailbox",
            "termal_read_mailbox_message",
            "termal_review_identity",
            "termal_send_to_session",
        ];
        assert_eq!(
            approvals.keys().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
        let catalog = mcp_tools_list_result();
        for tool in expected {
            assert_eq!(approvals[tool], json!({"approval_mode": "approve"}));
            assert!(
                catalog["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["name"] == tool)
            );
        }
        assert!(server.get("default_tools_approval_mode").is_none());
        assert!(
            server.get("enabled_tools").is_none(),
            "approval exceptions must not reshape discovery"
        );
        let excluded = catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .filter(|tool| !expected.contains(tool))
            .collect::<Vec<_>>();
        assert!(!excluded.is_empty());
        for tool in excluded {
            assert!(
                approvals.get(tool).is_none(),
                "{tool} must retain its approval behavior"
            );
        }
        // Root tracker stays unrestricted and unapproved, independently of the
        // read-only tracker exceptions already granted to reviewer children.
        let tracker = &request["params"]["config"]["mcp_servers"]["engram"];
        assert!(tracker.get("tools").is_none());
        assert!(tracker.get("default_tools_approval_mode").is_none());
    }
}

#[test]
fn codex_root_policy_crossing_regenerates_bound_thread_approvals_on_next_dispatch() {
    let home = TestTempRoot::create("codex-root-policy-crossing");
    fs::write(home.path().join("config.toml"), "").unwrap();
    for other in [
        CodexApprovalPolicy::AutoApprove,
        CodexApprovalPolicy::OnRequest,
        CodexApprovalPolicy::Untrusted,
        CodexApprovalPolicy::OnFailure,
    ] {
        for (before, after) in [
            (CodexApprovalPolicy::Never, other),
            (other, CodexApprovalPolicy::Never),
        ] {
            let state = test_app_state();
            let session = test_session_id(&state, Agent::Codex);
            let owner = phase_sync::ParkedProcess::spawn();
            let (runtime, commands, _) = test_shared_codex_runtime_with_process(
                "policy-crossing-runtime",
                owner.process.clone(),
            );
            *state
                .shared_codex_runtime_slot(SharedCodexProfile::Default)
                .lock()
                .unwrap() = Some(runtime.clone());
            {
                let mut inner = state.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                let record = &mut inner.sessions[index];
                record.runtime = SessionRuntime::Codex(
                    spawn_codex_runtime(
                        state.clone(),
                        session.clone(),
                        record.session.workdir.clone(),
                        SharedCodexProfile::Default,
                    )
                    .unwrap(),
                );
                record.attach_to_shared_codex_profile(SharedCodexProfile::Default);
                record.codex_approval_policy = before;
                record.session.approval_policy = Some(before);
                record.session.status = SessionStatus::Idle;
                set_record_external_session_id(record, Some("bound-thread".to_owned()));
            }
            runtime.sessions.lock().unwrap().insert(
                session.clone(),
                SharedCodexSessionState {
                    thread_id: Some("bound-thread".to_owned()),
                    ..Default::default()
                },
            );
            runtime
                .thread_sessions
                .lock()
                .unwrap()
                .insert("bound-thread".to_owned(), session.clone());
            runtime.sessions.lock().unwrap().insert(
                "sibling".to_owned(),
                SharedCodexSessionState {
                    thread_id: Some("sibling-thread".to_owned()),
                    ..Default::default()
                },
            );
            let (opening, _) = state
                .termal_delegation_mcp_codex_config_for_shared_runtime(
                    &session,
                    &RuntimeToken::Codex(runtime.runtime_id.clone()),
                    home.path(),
                    before,
                )
                .unwrap();
            assert_eq!(
                opening["mcp_servers"]["termal-delegation"]
                    .get("tools")
                    .is_some(),
                before == CodexApprovalPolicy::Never
            );
            state
                .update_session_settings(
                    &session,
                    serde_json::from_value(json!({"approvalPolicy": after})).unwrap(),
                )
                .unwrap();
            {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                assert!(
                    record.codex_mcp_config_refresh_pending,
                    "{before:?} -> {after:?} must replace thread config"
                );
                assert!(
                    !record.runtime_reset_required,
                    "a deferred thread config change must not block the live turn's approvals"
                );
                assert_eq!(record.external_session_id.as_deref(), Some("bound-thread"));
            }
            // Ordinary reset housekeeping must not consume a settings-only
            // refresh while the same thread is still attached.
            {
                let mut inner = state.inner.lock().unwrap();
                inner.session_mut(&session).unwrap().clear_runtime_reset();
            }
            dispatch_turn_and_snapshot(
                &state,
                &session,
                SendMessageRequest {
                    text: "Use the changed policy".to_owned(),
                    expanded_text: None,
                    attachments: vec![],
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap();
            {
                let inner = state.inner.lock().unwrap();
                assert!(
                    !inner.sessions[inner.find_session_index(&session).unwrap()]
                        .codex_mcp_config_refresh_pending,
                    "the next dispatch consumes the deferred refresh once"
                );
            }
            assert!(
                !runtime.sessions.lock().unwrap().contains_key(&session),
                "old thread config must detach"
            );
            assert_eq!(
                runtime.sessions.lock().unwrap()["sibling"]
                    .thread_id
                    .as_deref(),
                Some("sibling-thread")
            );
            let CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } = phase_sync::receive(&commands, "policy-change successor")
            else {
                panic!("expected prompt")
            };
            assert_eq!(session_id, session);
            assert_eq!(command.approval_policy, after);
            assert_eq!(command.resume_thread_id.as_deref(), Some("bound-thread"));
            let pending: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
            let mut writer = Vec::new();
            handle_shared_codex_prompt_command(
                &mut writer,
                &pending,
                &state,
                &runtime.runtime_id,
                home.path(),
                &runtime.sessions,
                &runtime.thread_sessions,
                &runtime.input_tx,
                None,
                &session,
                command,
            )
            .unwrap();
            let request: Value = serde_json::from_slice(&writer).unwrap();
            assert_eq!(request["method"], "thread/resume");
            assert_eq!(request["params"]["threadId"], "bound-thread");
            assert_eq!(request["params"]["approvalPolicy"], after.as_cli_value());
            let server = &request["params"]["config"]["mcp_servers"]["termal-delegation"];
            assert_eq!(
                server.get("tools").is_some(),
                after == CodexApprovalPolicy::Never
            );
            if after == CodexApprovalPolicy::Never {
                assert_eq!(
                    server["tools"]["termal_send_to_session"]["approval_mode"],
                    "approve"
                );
            }
            assert!(server.get("default_tools_approval_mode").is_none());
            retire_pending_codex_thread_setups(&pending);
        }
    }
}

#[test]
fn codex_policy_reset_preserves_non_crossing_roots_and_writable_children() {
    for (child, before, after) in [
        (
            false,
            CodexApprovalPolicy::AutoApprove,
            CodexApprovalPolicy::OnRequest,
        ),
        (
            false,
            CodexApprovalPolicy::Never,
            CodexApprovalPolicy::Never,
        ),
        (
            true,
            CodexApprovalPolicy::OnRequest,
            CodexApprovalPolicy::Never,
        ),
        (
            true,
            CodexApprovalPolicy::Never,
            CodexApprovalPolicy::OnRequest,
        ),
    ] {
        let state = test_app_state();
        let (project, root) =
            super::evaluator_tool_access::tracker_project(&state, "policy-reset-control");
        let parent = create_test_project_session(&state, Agent::Codex, &project, &root);
        let session = if child {
            let id = super::evaluator_tool_access::delegation_child(
                &state,
                &parent,
                &project,
                &root,
                Agent::Codex,
                DelegationMode::Explorer,
            );
            // Model an already admitted writable child. Read-only children
            // reject settings writes before reaching the reset decision.
            let mut inner = state.inner.lock().unwrap();
            let index = inner
                .find_delegation_index_by_child_session_id(&id)
                .unwrap();
            inner.delegations[index].write_policy = DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["src".to_owned()],
                worktree_path: Some(root.to_string_lossy().into_owned()),
                base: None,
            };
            id
        } else {
            parent
        };
        let (handle, commands) = test_codex_runtime_handle("policy-reset-control");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(handle);
            record.codex_approval_policy = before;
            record.session.approval_policy = Some(before);
            record.session.status = SessionStatus::Idle;
            set_record_external_session_id(record, Some("control-thread".to_owned()));
            assert!(!record.runtime_reset_required);
            assert!(!record.codex_mcp_config_refresh_pending);
        }
        state
            .update_session_settings(
                &session,
                serde_json::from_value(json!({"approvalPolicy": after})).unwrap(),
            )
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(
            !record.runtime_reset_required,
            "child={child}, {before:?} -> {after:?}"
        );
        assert!(!record.codex_mcp_config_refresh_pending);
        assert_eq!(record.codex_approval_policy, after);
        assert_eq!(
            record.external_session_id.as_deref(),
            Some("control-thread")
        );
        assert!(
            matches!(&record.runtime, SessionRuntime::Codex(handle) if handle.runtime_id == "policy-reset-control")
        );
        assert!(commands.try_recv().is_err());
    }
}

#[test]
fn codex_never_root_approvals_replace_collision_without_touching_user_servers() {
    let home = TestTempRoot::create("codex-never-root-mcp-seed");
    fs::write(
        home.path().join("config.toml"),
        r#"
[mcp_servers.user-tools]
command = "user-mcp"
default_tools_approval_mode = "prompt"
[mcp_servers.user-tools.tools.termal_send_to_session]
approval_mode = "prompt"
[mcp_servers.termal-delegation]
command = "not-termal"
args = ["--wrong-parent"]
default_tools_approval_mode = "approve"
[mcp_servers.termal-delegation.tools.termal_spawn_session]
approval_mode = "approve"
"#,
    )
    .unwrap();
    for thread in [None, Some("existing")] {
        let request = setup(home.path(), thread, CodexApprovalPolicy::Never, false);
        let servers = &request["params"]["config"]["mcp_servers"];
        assert_eq!(
            servers["user-tools"],
            json!({
                "command": "user-mcp", "default_tools_approval_mode": "prompt",
                "tools": {"termal_send_to_session": {"approval_mode": "prompt"}},
            })
        );
        let owned = &servers["termal-delegation"];
        assert_eq!(
            owned["command"],
            termal_delegation_mcp_current_exe().unwrap()
        );
        assert!(
            owned["args"]
                .as_array()
                .unwrap()
                .contains(&json!("delegation-mcp"))
        );
        assert!(
            !owned["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--wrong-parent"))
        );
        assert_eq!(
            owned["tools"]["termal_send_to_session"]["approval_mode"],
            "approve"
        );
        assert!(owned["tools"].get("termal_spawn_session").is_none());
        assert!(owned.get("default_tools_approval_mode").is_none());
    }
}

#[test]
fn codex_coordination_approval_exceptions_do_not_expand_other_policies_or_children() {
    let home = TestTempRoot::create("codex-root-mcp-policy-isolation");
    fs::write(home.path().join("config.toml"), "").unwrap();
    for thread in [None, Some("existing")] {
        for policy in [
            CodexApprovalPolicy::AutoApprove,
            CodexApprovalPolicy::OnRequest,
            CodexApprovalPolicy::Untrusted,
            CodexApprovalPolicy::OnFailure,
        ] {
            let request = setup(home.path(), thread, policy, false);
            assert_eq!(request["params"]["approvalPolicy"], policy.as_cli_value());
            assert!(
                request["params"]["config"]["mcp_servers"]["termal-delegation"]
                    .get("tools")
                    .is_none()
            );
        }
        let request = setup(home.path(), thread, CodexApprovalPolicy::Never, true);
        assert_eq!(request["params"]["approvalPolicy"], "never");
        let servers = &request["params"]["config"]["mcp_servers"];
        assert!(servers["termal-delegation"].get("tools").is_none());
        assert_eq!(
            servers["engram"]["tools"]["show"]["approval_mode"],
            "approve"
        );
        assert!(servers["engram"]["tools"].get("note").is_none());
    }
}
