use super::delegation_support::{
    finish_delegation_child_with_assistant_text, install_delegation_codex_runtime,
    temp_delegation_state_paths, test_app_state_with_delegation_codex_runtime,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

fn attach_sleeping_claude_runtime_to_delegation_child(
    state: &AppState,
    child_session_id: &str,
) -> Arc<SharedChild> {
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    child.runtime = SessionRuntime::Claude(ClaudeRuntimeHandle {
        runtime_id: format!("test-delegation-child-runtime-{child_session_id}"),
        input_tx,
        process: process.clone(),
    });
    child.session.status = SessionStatus::Active;
    state.commit_locked(&mut inner).unwrap();
    process
}

#[test]
fn delegation_prompt_marker_stays_in_sync_with_review_local_command() {
    let record = DelegationRecord {
        id: "delegation-marker-test".to_owned(),
        parent_session_id: "session-parent".to_owned(),
        child_session_id: "session-child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Marker Test".to_owned(),
        prompt: "/review-local".to_owned(),
        cwd: "/tmp/termal-marker-test".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
    };
    let prompt = build_delegation_prompt(&record);
    let review_local = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.claude/commands/review-local.md"
    ));

    assert!(
        prompt.contains(DELEGATED_CHILD_SESSION_MARKER),
        "delegation runtime prompt must expose the delegated-session marker"
    );
    assert!(
        review_local.contains(DELEGATED_CHILD_SESSION_MARKER),
        "/review-local must key delegated-child mode off the emitted marker"
    );
}

#[test]
fn delegation_prompt_tells_child_to_fail_fast_on_blocking_tooling() {
    let record = DelegationRecord {
        id: "delegation-tooling-test".to_owned(),
        parent_session_id: "session-parent".to_owned(),
        child_session_id: "session-child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Tooling Test".to_owned(),
        prompt: "Review the local changes.".to_owned(),
        cwd: "/tmp/termal-tooling-test".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
    };
    let prompt = build_delegation_prompt(&record);

    assert!(
        prompt.contains("Do not use browser, Chrome DevTools, or other external MCP tools"),
        "delegated review prompt should steer children away from interactive browser MCP tools"
    );
    assert!(
        prompt.contains("Return a failed `## Result` packet explaining the missing tool"),
        "delegated review prompt should make blocked tooling terminal and parseable"
    );
}

fn expected_codex_read_only_delegation_sandbox_mode() -> CodexSandboxMode {
    #[cfg(windows)]
    {
        CodexSandboxMode::DangerFullAccess
    }
    #[cfg(not(windows))]
    {
        CodexSandboxMode::ReadOnly
    }
}

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-test-runtime")
}

#[test]
fn claude_read_only_reviewer_delegation_child_uses_read_only_auto_approve_and_keeps_effort() {
    let state = test_app_state();
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();
    let child_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_claude_approval_mode = ClaudeApprovalMode::Ask;
        inner.preferences.default_claude_effort = ClaudeEffortLevel::Max;
        let child = inner.create_session(
            Agent::Claude,
            Some("Claude Delegation Defaults".to_owned()),
            workdir,
            None,
            None,
        );
        let child_session_id = child.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("child session should be indexed");
        let child_record = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");

        configure_delegation_child_prompt_settings(
            child_record,
            DelegationMode::Reviewer,
            &DelegationWritePolicy::ReadOnly,
        );
        child_session_id
    };

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should exist");
    assert_eq!(
        child.session.claude_approval_mode,
        Some(ClaudeApprovalMode::ReadOnlyAutoApprove)
    );
    assert_eq!(child.session.claude_effort, Some(ClaudeEffortLevel::Max));
}

#[test]
fn claude_non_read_only_delegation_child_keeps_app_default_approval_mode_and_effort() {
    let state = test_app_state();
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();
    let write_policies = [
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec!["src".to_owned()],
        },
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec!["src".to_owned()],
            worktree_path: Some(
                std::env::temp_dir()
                    .join(format!("termal-claude-write-delegation-{}", Uuid::new_v4()))
                    .to_string_lossy()
                    .into_owned(),
            ),
        },
    ];

    for write_policy in write_policies {
        let child_session_id = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            inner.preferences.default_claude_approval_mode = ClaudeApprovalMode::Ask;
            inner.preferences.default_claude_effort = ClaudeEffortLevel::Max;
            let child = inner.create_session(
                Agent::Claude,
                Some("Claude Write Delegation Defaults".to_owned()),
                workdir.clone(),
                None,
                None,
            );
            let child_session_id = child.session.id.clone();
            let child_index = inner
                .find_session_index(&child_session_id)
                .expect("child session should be indexed");
            let child_record = inner
                .session_mut_by_index(child_index)
                .expect("child session index should be valid");

            configure_delegation_child_prompt_settings(
                child_record,
                DelegationMode::Reviewer,
                &write_policy,
            );
            child_session_id
        };

        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_session_id)
            .expect("child session should exist");
        assert_eq!(
            child.session.claude_approval_mode,
            Some(ClaudeApprovalMode::Ask)
        );
        assert_eq!(child.session.claude_effort, Some(ClaudeEffortLevel::Max));
    }
}

#[test]
fn claude_read_only_explorer_delegation_child_keeps_app_default_approval_mode() {
    let state = test_app_state();
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();
    let child_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_claude_approval_mode = ClaudeApprovalMode::Ask;
        let child = inner.create_session(
            Agent::Claude,
            Some("Claude Explorer Delegation Defaults".to_owned()),
            workdir,
            None,
            None,
        );
        let child_session_id = child.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("child session should be indexed");
        let child_record = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");

        configure_delegation_child_prompt_settings(
            child_record,
            DelegationMode::Explorer,
            &DelegationWritePolicy::ReadOnly,
        );
        child_session_id
    };

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should exist");
    assert_eq!(
        child.session.claude_approval_mode,
        Some(ClaudeApprovalMode::Ask)
    );
}

fn push_delegation_child_assistant_text_without_finishing(
    state: &AppState,
    child_session_id: &str,
    text: &str,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let message_id = inner.next_message_id();
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    push_message_on_record(
        child,
        Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: text.to_owned(),
            expanded_text: None,
            source: None,
        },
    );
    child.session.preview = text.lines().last().unwrap_or_default().to_owned();
    state.commit_locked(&mut inner).unwrap();
}

fn runtime_token_for_session(state: &AppState, session_id: &str) -> RuntimeToken {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    inner.sessions[index]
        .runtime
        .runtime_token()
        .expect("session should have an attached runtime")
}

fn queue_delegation_child_prompt(state: &AppState, child_session_id: &str, text: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let message_id = inner.next_message_id();
    inner.sessions[child_index]
        .queued_prompts
        .push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                text: text.to_owned(),
                expanded_text: None,
                source: None,
            },
        });
    sync_pending_prompts(&mut inner.sessions[child_index]);
    state.commit_locked(&mut inner).unwrap();
}

fn shared_codex_runtime_for_state(state: &AppState) -> SharedCodexRuntime {
    state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned")
        .clone()
        .expect("test should install a shared Codex runtime")
}

fn parent_delegation_card_has_status(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
    status: ParallelAgentStatus,
) -> bool {
    inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .map(|parent| {
            parent.session.messages.iter().any(|message| {
                matches!(
                    message,
                    Message::ParallelAgents { agents, .. }
                        if agents
                            .iter()
                            .any(|agent| agent.id == delegation_id
                                && agent.source == ParallelAgentSource::Delegation
                                && agent.status == status)
                )
            })
        })
        .unwrap_or(false)
}

fn parent_delegation_card_agent_snapshot(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
) -> Option<(ParallelAgentStatus, Option<String>)> {
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)?;
    parent.session.messages.iter().rev().find_map(|message| {
        let Message::ParallelAgents { agents, .. } = message else {
            return None;
        };
        agents
            .iter()
            .find(|agent| {
                agent.id == delegation_id && agent.source == ParallelAgentSource::Delegation
            })
            .map(|agent| (agent.status, agent.detail.clone()))
    })
}

fn assert_single_parent_delegation_agent_status(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
    status: ParallelAgentStatus,
) {
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent session should exist");
    let matching_agents: Vec<&ParallelAgentProgress> = parent
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ParallelAgents { agents, .. } => Some(agents),
            _ => None,
        })
        .flat_map(|agents| agents.iter())
        .filter(|agent| agent.id == delegation_id)
        .collect();
    assert_eq!(
        matching_agents.len(),
        1,
        "parent card should have exactly one row for the delegation id"
    );
    let agent = matching_agents[0];
    assert_eq!(
        agent.source,
        ParallelAgentSource::Delegation,
        "parent card row for a delegation id must be delegation-sourced"
    );
    assert_eq!(agent.status, status);
}

#[test]
fn delegation_parent_card_update_ignores_tool_source_id_collision() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise parent card source matching.".to_owned(),
                title: Some("Source Collision".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_session_index(&parent_session_id)
            .expect("parent session should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        let parent_agents = parent
            .session
            .messages
            .iter_mut()
            .rev()
            .find_map(|message| match message {
                Message::ParallelAgents { agents, .. } => Some(agents),
                _ => None,
            })
            .expect("parent delegation card should exist");
        parent_agents.insert(
            0,
            ParallelAgentProgress {
                detail: Some("Tool row must not be touched".to_owned()),
                id: created.delegation.id.clone(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Tool collision".to_owned(),
            },
        );

        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_failed_locked(&mut inner, delegation_index, "delegation failed")
            .expect("running delegation should fail");

        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent session should exist");
        let agents: Vec<&ParallelAgentProgress> = parent
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ParallelAgents { agents, .. } => Some(agents),
                _ => None,
            })
            .flat_map(|agents| agents.iter())
            .filter(|agent| agent.id == created.delegation.id)
            .collect();
        let tool_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Tool)
            .expect("tool-source collision row should remain");
        assert_eq!(tool_agent.status, ParallelAgentStatus::Running);
        assert_eq!(
            tool_agent.detail.as_deref(),
            Some("Tool row must not be touched")
        );
        let delegation_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Delegation)
            .expect("delegation-source row should remain");
        assert_eq!(delegation_agent.status, ParallelAgentStatus::Error);
        assert_eq!(
            delegation_agent.detail.as_deref(),
            Some("delegation failed")
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_parent_card_update_ignores_recorder_tool_source_id_collision() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise recorder-sourced parent card matching.".to_owned(),
                title: Some("Recorder Source Collision".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    state
        .upsert_parallel_agents_message(
            &parent_session_id,
            "claude-task-group-source-collision",
            vec![ParallelAgentProgress {
                detail: Some("Recorder tool row must not be touched".to_owned()),
                id: created.delegation.id.clone(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Recorder tool collision".to_owned(),
            }],
        )
        .expect("recorder path should create a tool-sourced parallel-agents row");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_failed_locked(&mut inner, delegation_index, "delegation failed")
            .expect("running delegation should fail");

        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent session should exist");
        let agents: Vec<&ParallelAgentProgress> = parent
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ParallelAgents { agents, .. } => Some(agents),
                _ => None,
            })
            .flat_map(|agents| agents.iter())
            .filter(|agent| agent.id == created.delegation.id)
            .collect();
        assert_eq!(
            agents.len(),
            2,
            "recorder tool row and delegation row should coexist for the same id"
        );
        let tool_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Tool)
            .expect("tool-source collision row should remain");
        assert_eq!(tool_agent.status, ParallelAgentStatus::Running);
        assert_eq!(
            tool_agent.detail.as_deref(),
            Some("Recorder tool row must not be touched")
        );
        let delegation_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Delegation)
            .expect("delegation-source row should remain");
        assert_eq!(delegation_agent.status, ParallelAgentStatus::Error);
        assert_eq!(
            delegation_agent.detail.as_deref(),
            Some("delegation failed")
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn test_mcp_elicitation_message(message_id: &str) -> Message {
    Message::McpElicitationRequest {
        id: message_id.to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: "Codex needs MCP input".to_owned(),
        detail: "MCP server chrome-devtools requested additional structured input. Allow the chrome-devtools MCP server to run tool \"new_page\"?".to_owned(),
        request: McpElicitationRequestPayload {
            thread_id: "thread-mcp".to_owned(),
            turn_id: Some("turn-mcp".to_owned()),
            server_name: "chrome-devtools".to_owned(),
            mode: McpElicitationRequestMode::Form {
                meta: None,
                message: "Allow tool new_page?".to_owned(),
                requested_schema: json!({
                    "type": "object",
                    "properties": {}
                }),
            },
        },
        state: InteractionRequestState::Pending,
        submitted_action: None,
        submitted_content: None,
    }
}

fn test_approval_message(message_id: &str) -> Message {
    Message::Approval {
        id: message_id.to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: "Codex needs approval".to_owned(),
        command: "Edit src/main.rs".to_owned(),
        command_language: None,
        detail: "Allow editing src/main.rs?".to_owned(),
        decision: ApprovalDecision::Pending,
    }
}

fn test_user_input_message(message_id: &str) -> Message {
    Message::UserInputRequest {
        id: message_id.to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: "Codex needs input".to_owned(),
        detail: "Choose the review depth.".to_owned(),
        questions: vec![UserInputQuestion {
            header: "Depth".to_owned(),
            id: "depth".to_owned(),
            is_other: false,
            is_secret: false,
            options: Some(vec![UserInputQuestionOption {
                description: "Run the focused review path.".to_owned(),
                label: "Focused".to_owned(),
            }]),
            question: "How deep should the review go?".to_owned(),
        }],
        state: InteractionRequestState::Pending,
        submitted_answers: None,
    }
}

fn test_codex_app_request_message(message_id: &str) -> Message {
    Message::CodexAppRequest {
        id: message_id.to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: "Codex needs app data".to_owned(),
        detail: "Allow the local app request to continue?".to_owned(),
        method: "termal/test".to_owned(),
        params: json!({ "ok": true }),
        state: InteractionRequestState::Pending,
        submitted_result: None,
    }
}

#[derive(Clone, Copy)]
enum PendingInteractionTestKind {
    Approval,
    UserInput,
    Mcp,
    CodexApp,
}

fn test_pending_interaction_message(kind: PendingInteractionTestKind, message_id: &str) -> Message {
    match kind {
        PendingInteractionTestKind::Approval => test_approval_message(message_id),
        PendingInteractionTestKind::UserInput => test_user_input_message(message_id),
        PendingInteractionTestKind::Mcp => test_mcp_elicitation_message(message_id),
        PendingInteractionTestKind::CodexApp => test_codex_app_request_message(message_id),
    }
}

#[test]
fn delegation_child_pending_mcp_request_updates_parent_card_detail() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review without browser tooling.".to_owned(),
                title: Some("MCP Blocked Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    state
        .push_message(
            &created.delegation.child_session_id,
            test_mcp_elicitation_message("delegation-mcp-message"),
        )
        .expect("pending MCP message should be recorded");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let (status, detail) =
        parent_delegation_card_agent_snapshot(&inner, &parent_session_id, &created.delegation.id)
            .expect("parent delegation card row should exist");
    let detail = detail.expect("running parent card should have detail");
    assert_eq!(status, ParallelAgentStatus::Running);
    assert!(
        detail.contains("Child session is waiting for MCP input"),
        "parent card should expose child pending MCP state: {detail}"
    );
    assert!(
        detail.contains("chrome-devtools"),
        "parent card should preserve the MCP server detail: {detail}"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_child_pending_non_mcp_requests_update_parent_card_detail() {
    let cases = [
        (
            test_approval_message("delegation-approval-message"),
            "Child session is waiting for approval",
            "Allow editing src/main.rs",
        ),
        (
            test_user_input_message("delegation-user-input-message"),
            "Child session is waiting for input",
            "Choose the review depth",
        ),
        (
            test_codex_app_request_message("delegation-app-request-message"),
            "Child session is waiting for a Codex response",
            "Allow the local app request",
        ),
    ];

    for (message, expected_prefix, expected_detail) in cases {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Review pending child interaction.".to_owned(),
                    title: Some(format!("{expected_prefix} review")),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");

        state
            .push_message(&created.delegation.child_session_id, message)
            .expect("pending interaction message should be recorded");

        let inner = state.inner.lock().expect("state mutex poisoned");
        let (status, detail) = parent_delegation_card_agent_snapshot(
            &inner,
            &parent_session_id,
            &created.delegation.id,
        )
        .expect("parent delegation card row should exist");
        let detail = detail.expect("running parent card should have detail");
        assert_eq!(status, ParallelAgentStatus::Running);
        assert!(
            detail.contains(expected_prefix),
            "parent card should expose child pending interaction state: {detail}"
        );
        assert!(
            detail.contains(expected_detail),
            "parent card should preserve child interaction detail: {detail}"
        );
        drop(inner);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn delegated_interaction_submission_returns_post_refresh_state_and_restores_running_detail() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-interaction-refresh");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review with MCP interaction.".to_owned(),
                title: Some("MCP Refresh Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    match input_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(CodexRuntimeCommand::Prompt { .. }) => {}
        Ok(_) => panic!("expected initial delegation prompt"),
        Err(err) => panic!("initial delegation prompt should dispatch: {err}"),
    }

    let message_id = "delegation-response-mcp-message";
    let request = match test_mcp_elicitation_message(message_id) {
        Message::McpElicitationRequest { request, .. } => request,
        _ => unreachable!("test helper should create an MCP message"),
    };
    state
        .push_message(
            &created.delegation.child_session_id,
            test_mcp_elicitation_message(message_id),
        )
        .expect("pending MCP message should be recorded");
    state
        .register_codex_pending_mcp_elicitation(
            &created.delegation.child_session_id,
            message_id.to_owned(),
            CodexPendingMcpElicitation {
                request,
                request_id: json!("delegation-mcp-request"),
            },
        )
        .expect("pending MCP elicitation should be registered");

    let response = state
        .submit_codex_mcp_elicitation(
            &created.delegation.child_session_id,
            message_id,
            McpElicitationAction::Decline,
            None,
        )
        .expect("MCP elicitation response should submit");

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(CodexRuntimeCommand::JsonRpcResponse { response }) => {
            assert_eq!(response.request_id, json!("delegation-mcp-request"));
        }
        Ok(_) => panic!("expected Codex JSON-RPC MCP response"),
        Err(err) => panic!("Codex MCP response should arrive: {err}"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        response.revision, inner.revision,
        "interaction submission response should include the post-refresh revision"
    );
    let (status, detail) =
        parent_delegation_card_agent_snapshot(&inner, &parent_session_id, &created.delegation.id)
            .expect("parent delegation card row should exist");
    assert_eq!(status, ParallelAgentStatus::Running);
    assert_eq!(
        detail.as_deref(),
        Some("Delegated session is running."),
        "parent card should return to the generic running detail after the interaction resolves"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn interaction_message_update_returns_post_refresh_state_for_each_request_variant() {
    let cases = [
        PendingInteractionTestKind::Approval,
        PendingInteractionTestKind::UserInput,
        PendingInteractionTestKind::Mcp,
        PendingInteractionTestKind::CodexApp,
    ];

    for (index, kind) in cases.into_iter().enumerate() {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Review with pending interaction.".to_owned(),
                    title: Some(format!("Interaction Refresh {index}")),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        let message_id = format!("delegation-refresh-message-{index}");
        state
            .push_message(
                &created.delegation.child_session_id,
                test_pending_interaction_message(kind, &message_id),
            )
            .expect("pending interaction message should be recorded");

        let response = state
            .commit_interaction_message_update(
                &created.delegation.child_session_id,
                &message_id,
                |record| {
                    let message_index = match kind {
                        PendingInteractionTestKind::Approval => {
                            let message_index = set_approval_decision_on_record(
                                record,
                                &message_id,
                                ApprovalDecision::Accepted,
                            )
                            .expect("approval should update");
                            let preview = approval_preview_text(
                                record.session.agent.name(),
                                ApprovalDecision::Accepted,
                            );
                            sync_session_interaction_state(record, preview);
                            message_index
                        }
                        PendingInteractionTestKind::UserInput => {
                            let message_index = set_user_input_request_state_on_record(
                                record,
                                &message_id,
                                InteractionRequestState::Submitted,
                                Some(BTreeMap::from([(
                                    "depth".to_owned(),
                                    vec!["Focused".to_owned()],
                                )])),
                            )
                            .expect("user input should update");
                            let preview = user_input_request_preview_text(
                                record.session.agent.name(),
                                InteractionRequestState::Submitted,
                            );
                            sync_session_interaction_state(record, preview);
                            message_index
                        }
                        PendingInteractionTestKind::Mcp => {
                            let message_index = set_mcp_elicitation_request_state_on_record(
                                record,
                                &message_id,
                                InteractionRequestState::Submitted,
                                Some(McpElicitationAction::Decline),
                                None,
                            )
                            .expect("MCP elicitation should update");
                            let preview = mcp_elicitation_request_preview_text(
                                record.session.agent.name(),
                                InteractionRequestState::Submitted,
                                Some(McpElicitationAction::Decline),
                            );
                            sync_session_interaction_state(record, preview);
                            message_index
                        }
                        PendingInteractionTestKind::CodexApp => {
                            let message_index = set_codex_app_request_state_on_record(
                                record,
                                &message_id,
                                InteractionRequestState::Submitted,
                                Some(json!({ "ok": true })),
                            )
                            .expect("Codex app request should update");
                            let preview = codex_app_request_preview_text(
                                record.session.agent.name(),
                                InteractionRequestState::Submitted,
                            );
                            sync_session_interaction_state(record, preview);
                            message_index
                        }
                    };
                    Ok(message_index)
                },
            )
            .expect("interaction message update should commit");

        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(
            response.revision, inner.revision,
            "interaction update response should include the post-refresh revision"
        );
        let (status, detail) = parent_delegation_card_agent_snapshot(
            &inner,
            &parent_session_id,
            &created.delegation.id,
        )
        .expect("parent delegation card row should exist");
        assert_eq!(status, ParallelAgentStatus::Running);
        assert_eq!(detail.as_deref(), Some("Delegated session is running."));
        drop(inner);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn canceling_delegation_preserves_pending_child_interaction_reason() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review with a blocked MCP request.".to_owned(),
                title: Some("Canceled MCP Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    state
        .push_message(
            &created.delegation.child_session_id,
            test_mcp_elicitation_message("delegation-cancel-mcp-message"),
        )
        .expect("pending MCP message should be recorded");

    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("delegation cancel should succeed");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let summary = response
        .delegation
        .result
        .as_ref()
        .map(|result| result.summary.as_str())
        .expect("canceled delegation should store a result summary");
    assert!(
        summary.contains("Last child state: Child session is waiting for MCP input"),
        "canceled delegation should explain the child blocker: {summary}"
    );
    assert!(
        summary.contains("chrome-devtools"),
        "canceled delegation should preserve the MCP server detail: {summary}"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_records_persist_and_reload_with_child_link() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let delegation_id;
    let child_session_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        install_delegation_codex_runtime(&state, "delegation-persist-runtime");
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Inspect the backend persistence shape.".to_owned(),
                    title: Some("Persistence Review".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        delegation_id = created.delegation.id.clone();
        child_session_id = created.delegation.child_session_id.clone();
        assert_eq!(created.delegation.parent_session_id, parent_session_id);
        assert_eq!(created.delegation.status, DelegationStatus::Running);
        assert_eq!(
            created.child_session.parent_delegation_id.as_deref(),
            Some(delegation_id.as_str())
        );
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should reload");
    assert_eq!(delegation.child_session_id, child_session_id);
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .expect("recovered delegation should have a result")
            .summary
            .contains("TermAl restarted")
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should reload");
    assert_eq!(
        child.session.parent_delegation_id.as_deref(),
        Some(delegation_id.as_str())
    );
    drop(inner);
    restarted.shutdown_persist_blocking();

    let state_root = persistence_path
        .parent()
        .expect("persistence path should have a parent")
        .to_path_buf();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn persisted_delegation_records_repair_missing_child_parent_link_on_reload() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let delegation_id;
    let child_session_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        install_delegation_codex_runtime(&state, "delegation-persist-runtime");
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Inspect the backend persistence shape.".to_owned(),
                    title: Some("Persistence Review".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        delegation_id = created.delegation.id.clone();
        child_session_id = created.delegation.child_session_id.clone();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let child_index = inner
                .find_session_index(&child_session_id)
                .expect("child session should exist");
            inner.sessions[child_index].session.parent_delegation_id = None;
            persist_state(&persistence_path, &inner)
                .expect("corrupted persisted state should be written");
        }
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should reload");
    assert_eq!(
        child.session.parent_delegation_id.as_deref(),
        Some(delegation_id.as_str())
    );
    drop(inner);
    restarted.shutdown_persist_blocking();

    let state_root = persistence_path
        .parent()
        .expect("persistence path should have a parent")
        .to_path_buf();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[tokio::test]
async fn delegation_routes_create_status_and_unavailable_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Read-only route check",
        "title": "Route Delegation",
        "mode": "reviewer",
        "writePolicy": { "kind": "readOnly" }
    }))
    .expect("delegation request should serialize");

    let (create_status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    assert_eq!(created.delegation.parent_session_id, parent_session_id);
    assert_eq!(created.delegation.status, DelegationStatus::Running);
    assert_eq!(
        created.child_session.parent_delegation_id.as_deref(),
        Some(created.delegation.id.as_str())
    );

    let (get_status, fetched): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(fetched.delegation, created.delegation);

    let (result_status, result_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/result",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(result_status, StatusCode::CONFLICT);
    assert!(
        result_body["error"]
            .as_str()
            .expect("error response should include a message")
            .contains("result is not available yet")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_result_route_uses_camel_case_json_shape() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return route result metadata.".to_owned(),
                title: Some("Route Result Shape".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let command_id = inner.next_message_id();
        let files_id = inner.next_message_id();
        let result_id = inner.next_message_id();
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::Command {
                id: command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo test delegations".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
            },
        );
        push_message_on_record(
            child,
            Message::FileChanges {
                id: files_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Agent changed 1 file".to_owned(),
                files: vec![FileChangeSummaryEntry {
                    path: "src/main.rs".to_owned(),
                    kind: WorkspaceFileChangeKind::Modified,
                }],
            },
        );
        push_message_on_record(
            child,
            Message::Text {
                attachments: Vec::new(),
                id: result_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "## Result\n\nStatus: completed\n\nSummary:\nRoute shape pinned.\n\nFindings:\n- Medium src/main.rs:42 - Route result carries review findings.\n\nNotes:\n- Result packet metadata inspected.\n\nFiles Inspected:\n- src/main.rs".to_owned(),
                expanded_text: None,
                source: None,
            },
        );
        child.session.status = SessionStatus::Idle;
        child.session.preview = "Route shape pinned.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/result",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["revision"].is_u64());
    assert!(body["serverInstanceId"].is_string());
    let result = &body["result"];
    assert_eq!(result["delegationId"], created.delegation.id);
    assert_eq!(
        result["childSessionId"],
        created.delegation.child_session_id
    );
    assert_eq!(result["status"], "completed");
    assert_eq!(result["summary"], "Route shape pinned.");
    assert_eq!(
        result["findings"],
        json!([{
            "severity": "Medium",
            "file": "src/main.rs",
            "line": 42,
            "message": "Route result carries review findings."
        }])
    );
    assert_eq!(result["changedFiles"], json!(["src/main.rs"]));
    assert_eq!(
        result["commandsRun"],
        json!([{
            "command": "cargo test delegations",
            "status": "success"
        }])
    );
    assert_eq!(
        result["notes"],
        json!(["Result packet metadata inspected.", "Inspected src/main.rs"])
    );
    assert!(result.get("delegation_id").is_none());
    assert!(result.get("child_session_id").is_none());
    assert!(result.get("changed_files").is_none());
    assert!(result.get("commands_run").is_none());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_routes_reject_wrong_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let unrelated_parent_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Parent-scoped route check",
        "title": "Scoped Delegation",
        "mode": "reviewer",
        "writePolicy": { "kind": "readOnly" }
    }))
    .expect("delegation request should serialize");

    let (create_status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);

    let wrong_parent_status_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(wrong_parent_status_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let wrong_parent_result_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}/result",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(wrong_parent_result_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let wrong_parent_cancel_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}/cancel",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(wrong_parent_cancel_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let (status, fetched): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched.delegation.status, DelegationStatus::Running);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_creation_dispatches_child_prompt_through_runtime_channel() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-dispatch-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Inspect dispatch wiring.".to_owned(),
                title: Some("Dispatch Wiring".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered to runtime")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, created.delegation.child_session_id);
            assert_eq!(command.approval_policy, CodexApprovalPolicy::Never);
            assert_eq!(
                command.sandbox_mode,
                expected_codex_read_only_delegation_sandbox_mode()
            );
            assert_eq!(command.cwd, created.delegation.cwd);
            assert!(command.prompt.contains("Inspect dispatch wiring."));
        }
        _ => panic!("delegation should dispatch a Codex prompt command"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Active);
    assert!(matches!(child.runtime, SessionRuntime::Codex(_)));
    assert!(
        child.session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Inspect dispatch wiring.")
        )),
        "production dispatch should append the child prompt message"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_delegation_materializes_dirty_state_and_uses_workspace_write() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("isolated-delegation-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-source-{unique}"));
    let worktree_root = std::env::temp_dir().join(format!("termal-isolated-child-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(repo_root.join("README.md"), "staged\n").expect("staged content should write");
    run_git_test_command(&repo_root, &["add", "README.md"]);
    fs::write(repo_root.join("README.md"), "unstaged\n").expect("unstaged content should write");

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Isolated Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Run build-gated review.".to_owned(),
                title: Some("Isolated Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                    owned_paths: vec!["README.md".to_owned()],
                    worktree_path: Some(worktree_root.to_string_lossy().into_owned()),
                }),
            },
        )
        .expect("isolated worktree delegation should be created");

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered to runtime")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, created.delegation.child_session_id);
            assert_eq!(command.approval_policy, CodexApprovalPolicy::Never);
            assert_eq!(command.sandbox_mode, CodexSandboxMode::WorkspaceWrite);
            assert_eq!(command.cwd, created.delegation.cwd);
            assert!(command.prompt.contains("Write policy: isolated worktree"));
            assert!(command.prompt.contains("README.md"));
        }
        _ => panic!("isolated delegation should dispatch a Codex prompt command"),
    }

    assert_eq!(created.child_session.project_id, None);
    assert_eq!(
        created.child_session.sandbox_mode,
        Some(CodexSandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "unstaged\n"
    );
    let staged_diff =
        run_git_test_command_output(&worktree_root, &["diff", "--cached", "--", "README.md"]);
    assert!(staged_diff.contains("-base"));
    assert!(staged_diff.contains("+staged"));
    let unstaged_diff = run_git_test_command_output(&worktree_root, &["diff", "--", "README.md"]);
    assert!(unstaged_diff.contains("-staged"));
    assert!(unstaged_diff.contains("+unstaged"));

    let _ = git_command()
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_root)
        .output();
    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(&worktree_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn isolated_worktree_delegation_route_generates_termal_owned_path_when_omitted() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("generated-isolated-delegation-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-auto-source-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(repo_root.join("README.md"), "changed\n").expect("changed file should write");

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Auto Isolated Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Run generated isolated review.",
        "title": "Generated Isolated Review",
        "mode": "reviewer",
        "writePolicy": {
            "kind": "isolatedWorktree",
            "ownedPaths": []
        }
    }))
    .expect("delegation request should serialize");
    let (status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let _ = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered to runtime");

    let DelegationWritePolicy::IsolatedWorktree { worktree_path, .. } =
        &created.delegation.write_policy
    else {
        panic!("created delegation should preserve isolated write policy");
    };
    let worktree_path = worktree_path
        .as_ref()
        .expect("generated worktree path should be stored");
    assert!(worktree_path.contains(&created.delegation.id));
    assert_eq!(
        fs::read_to_string(FsPath::new(worktree_path).join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "changed\n"
    );

    let _ = git_command()
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .output();
    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(
        FsPath::new(worktree_path)
            .parent()
            .unwrap_or_else(|| FsPath::new(worktree_path)),
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_setup_failure_does_not_leave_worktree() {
    let state = test_app_state();
    state
        .test_agent_setup_failures
        .lock()
        .expect("test agent setup failures mutex poisoned")
        .push((
            Agent::Cursor,
            "forced Cursor setup failure before isolated worktree".to_owned(),
        ));
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-setup-source-{unique}"));
    let worktree_root = std::env::temp_dir().join(format!("termal-isolated-setup-child-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Setup Failure Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "This isolated reviewer should be rejected before worktree creation."
                .to_owned(),
            title: Some("Setup Failure Isolated Reviewer".to_owned()),
            cwd: None,
            agent: Some(Agent::Cursor),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["README.md".to_owned()],
                worktree_path: Some(worktree_root.to_string_lossy().into_owned()),
            }),
        },
    ) {
        Ok(_) => panic!("forced setup failure should reject isolated delegation"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        err.message,
        "forced Cursor setup failure before isolated worktree"
    );
    assert!(
        !worktree_root.exists(),
        "setup failure must not leave a worktree"
    );

    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(&worktree_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_max_fanout_rejection_does_not_leave_worktree() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("isolated-fanout-rollback-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-fanout-source-{unique}"));
    let worktree_root = std::env::temp_dir().join(format!("termal-isolated-fanout-child-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Fanout Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    for index in 0..MAX_RUNNING_DELEGATIONS_PER_PARENT {
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: format!("Read-only reviewer {index}."),
                    title: Some(format!("Read-only Reviewer {index}")),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("read-only delegation should be admitted");
        match input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("delegation child prompt should be delivered to runtime")
        {
            CodexRuntimeCommand::Prompt { session_id, .. } => {
                assert_eq!(session_id, created.delegation.child_session_id);
            }
            _ => panic!("delegation should dispatch a Codex prompt command"),
        }
    }

    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "This isolated reviewer should be rejected before worktree creation."
                .to_owned(),
            title: Some("Rejected Isolated Reviewer".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["README.md".to_owned()],
                worktree_path: Some(worktree_root.to_string_lossy().into_owned()),
            }),
        },
    ) {
        Ok(_) => panic!("max fanout should reject the isolated delegation"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("active delegations"),
        "unexpected max fanout error: {}",
        err.message
    );
    assert!(
        !worktree_root.exists(),
        "rejected isolated delegation must not leave a worktree"
    );

    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(&worktree_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_rejects_untracked_files_without_leaving_worktree() {
    let (state, _input_rx) =
        test_app_state_with_delegation_codex_runtime("isolated-untracked-rollback-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-untracked-source-{unique}"));
    let worktree_root =
        std::env::temp_dir().join(format!("termal-isolated-untracked-child-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(repo_root.join("new-file.txt"), "untracked\n").expect("untracked file should write");

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Untracked Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "This isolated reviewer should reject untracked files.".to_owned(),
            title: Some("Untracked Isolated Reviewer".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                owned_paths: vec!["README.md".to_owned()],
                worktree_path: Some(worktree_root.to_string_lossy().into_owned()),
            }),
        },
    ) {
        Ok(_) => panic!("untracked files should reject isolated delegation"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message.contains("untracked file"),
        "unexpected untracked-file error: {}",
        err.message
    );
    assert!(
        err.message.contains("new-file.txt"),
        "untracked-file error should list the blocking file: {}",
        err.message
    );
    assert!(
        !worktree_root.exists(),
        "untracked-file rejection must not leave a worktree"
    );

    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(&worktree_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_patch_size_limit_rejects_oversized_dirty_state() {
    validate_isolated_worktree_patch_size_bytes(MAX_ISOLATED_WORKTREE_PATCH_BYTES, 0)
        .expect("exact patch limit should be accepted");

    let err =
        match validate_isolated_worktree_patch_size_bytes(MAX_ISOLATED_WORKTREE_PATCH_BYTES, 1) {
            Ok(_) => panic!("patch one byte over the limit should be rejected"),
            Err(err) => err,
        };
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(
        err.message.contains("isolated worktree dirty-state patch"),
        "unexpected patch-size error: {}",
        err.message
    );
    assert!(
        err.message
            .contains(&MAX_ISOLATED_WORKTREE_PATCH_BYTES.to_string()),
        "patch-size error should include the configured limit: {}",
        err.message
    );

    let overflow_err = match validate_isolated_worktree_patch_size_bytes(usize::MAX, 1) {
        Ok(_) => panic!("overflowing patch size should be rejected"),
        Err(err) => err,
    };
    assert_eq!(overflow_err.status, StatusCode::BAD_REQUEST);
    assert!(
        overflow_err.message.contains("too large"),
        "overflow should use a size-limit error: {}",
        overflow_err.message
    );
}

#[test]
fn terminal_delegation_child_dispatch_is_blocked_before_runtime_start() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-terminal-dispatch");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let (delegation_id, child_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_id = inner.next_delegation_id();
        let child_record = inner.create_session(
            Agent::Codex,
            Some("Canceled Delegation Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        let child_session_id = child_record.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("just-created child session should be indexed");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.codex_approval_policy = CodexApprovalPolicy::Never;
        child.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
        child.session.approval_policy = Some(CodexApprovalPolicy::Never);
        child.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
        child.session.parent_delegation_id = Some(delegation_id.clone());

        let now = stamp_now();
        inner.delegations.push(DelegationRecord {
            id: delegation_id.clone(),
            parent_session_id: parent_session_id.clone(),
            child_session_id: child_session_id.clone(),
            mode: DelegationMode::Reviewer,
            status: DelegationStatus::Canceled,
            title: "Canceled delegation".to_owned(),
            prompt: "Do not dispatch".to_owned(),
            cwd: "/tmp".to_owned(),
            agent: Agent::Codex,
            model: Some(Agent::Codex.default_model().to_owned()),
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: now.clone(),
            started_at: Some(now.clone()),
            completed_at: Some(now),
            result: None,
        });
        state.commit_locked(&mut inner).unwrap();
        (delegation_id, child_session_id)
    };

    let err = match state.dispatch_turn(
        &child_session_id,
        SendMessageRequest {
            text: "This prompt should not reach the runtime.".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
        },
    ) {
        Ok(_) => panic!("terminal delegated child dispatch should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.message, DELEGATION_NO_LONGER_STARTABLE_MESSAGE);
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "blocked terminal delegation dispatch must not reach the runtime"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child should still exist");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.is_empty());
    assert!(inner
        .delegations
        .iter()
        .any(|record| record.id == delegation_id
            && record.status == DelegationStatus::Canceled));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// tm-5tu: a peer message delivered via `termal_send_to_session` carries the
// sender session's identity so the receiver's transcript labels it with the
// sender's name instead of "You". The backend resolves source_session_id to a
// display name while holding the state lock, so a caller cannot spoof another
// session's name.
#[test]
fn peer_message_dispatch_attributes_resolved_sender_name() {
    let state = test_app_state();

    // Sender session whose NAME the receiver should see.
    let sender_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .create_session(
                Agent::Claude,
                Some("Kadry".to_owned()),
                "/tmp".to_owned(),
                None,
                None,
            )
            .session
            .id
            .clone()
    };

    // Idle target with a mock Claude runtime so dispatch never spawns a real
    // process. The receiver channel is kept alive for the whole test.
    let (target_id, _input_rx) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_id = inner
            .create_session(
                Agent::Claude,
                Some("Receiver".to_owned()),
                "/tmp".to_owned(),
                None,
                None,
            )
            .session
            .id
            .clone();
        let (runtime, input_rx) = test_claude_runtime_handle("peer-attribution-runtime");
        let index = inner
            .find_session_index(&target_id)
            .expect("target session should be indexed");
        inner
            .session_mut_by_index(index)
            .expect("target session index should be valid")
            .runtime = SessionRuntime::Claude(runtime);
        (target_id, input_rx)
    };

    state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ping from a peer".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some(sender_id.clone()),
            },
        )
        .expect("peer message should dispatch to the idle target");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target session should exist");
    let last = record
        .session
        .messages
        .last()
        .expect("dispatch should append the peer message");
    match last {
        Message::Text {
            author: Author::You,
            text,
            source: Some(source),
            ..
        } => {
            assert_eq!(text, "ping from a peer");
            assert_eq!(source.session_id, sender_id);
            assert_eq!(source.name, "Kadry");
        }
        other => panic!("expected an attributed peer text message, got {other:?}"),
    }
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// tm-5tu: when the target is mid-turn the peer message is queued, so the
// attribution must ride on the queued prompt and be applied when it later
// becomes a `Message::Text`. An unknown sender id resolves to no attribution,
// leaving the message as an ordinary "You" prompt.
#[test]
fn peer_message_queued_while_busy_preserves_source_and_ignores_unknown_sender() {
    let state = test_app_state();

    let sender_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .create_session(
                Agent::Claude,
                Some("Kadry".to_owned()),
                "/tmp".to_owned(),
                None,
                None,
            )
            .session
            .id
            .clone()
    };

    // Busy target: dispatch queues instead of starting a turn, so no runtime is
    // required and the attribution must survive on the queued prompt.
    let target_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_id = inner
            .create_session(
                Agent::Claude,
                Some("Receiver".to_owned()),
                "/tmp".to_owned(),
                None,
                None,
            )
            .session
            .id
            .clone();
        let index = inner
            .find_session_index(&target_id)
            .expect("target session should be indexed");
        inner
            .session_mut_by_index(index)
            .expect("target session index should be valid")
            .session
            .status = SessionStatus::Active;
        target_id
    };

    state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "queued peer ping".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some(sender_id.clone()),
            },
        )
        .expect("busy target should queue the known-sender peer message");
    state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ping from a ghost".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some("session-does-not-exist".to_owned()),
            },
        )
        .expect("busy target should queue the unknown-sender peer message");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target session should exist");
    assert_eq!(record.queued_prompts.len(), 2);

    let known = &record.queued_prompts[0].pending_prompt;
    assert_eq!(known.text, "queued peer ping");
    let source = known
        .source
        .as_ref()
        .expect("a known sender must be attributed on the queued prompt");
    assert_eq!(source.session_id, sender_id);
    assert_eq!(source.name, "Kadry");

    let unknown = &record.queued_prompts[1].pending_prompt;
    assert_eq!(unknown.text, "ping from a ghost");
    assert!(
        unknown.source.is_none(),
        "an unknown sender id must not fabricate attribution"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn create_delegation_terminalized_before_start_does_not_dispatch_child_prompt() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-create-start-race");
    let parent_session_id = test_session_id(&state, Agent::Codex);

    let response = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Simulate race. {TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT}"),
                title: Some("Create Start Race".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("terminalized delegation create should return current state");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminalized create/start race must not dispatch a child runtime prompt"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == response.delegation.child_session_id)
        .expect("child session should remain visible");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.is_empty());
    assert_eq!(child.session.status, SessionStatus::Idle);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn followup_delegation_rejects_unknown_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = match state.followup_delegation(
        &parent_session_id,
        "delegation-does-not-exist",
        "hi".to_owned(),
    ) {
        Ok(_) => panic!("unknown delegation must fail"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn followup_delegation_rejects_empty_message() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do a thing.".to_owned(),
                title: Some("Empty Message".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let err = match state.followup_delegation(
        &parent_session_id,
        &created.delegation.id,
        "   ".to_owned(),
    ) {
        Ok(_) => panic!("empty follow-up message must fail"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// The user's rule: a follow-up on a still-running delegation fails (wait first),
// it does not queue behind the active turn.
#[test]
fn followup_delegation_rejects_running_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Long running.".to_owned(),
                title: Some("Still Running".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    assert_eq!(created.delegation.status, DelegationStatus::Running);
    let err = match state.followup_delegation(
        &parent_session_id,
        &created.delegation.id,
        "more".to_owned(),
    ) {
        Ok(_) => panic!("follow-up on a running delegation must fail"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// The happy path: a completed delegation resumes on follow-up and re-arms to Running
// with the stale terminal result cleared.
#[test]
fn followup_delegation_resumes_completed_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review.".to_owned(),
                title: Some("Followup Resume".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let child_id = created.delegation.child_session_id.clone();
    let child_token = runtime_token_for_session(&state, &child_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &child_id,
        "## Result\n\nStatus: completed\n\nSummary:\nFirst pass done.",
    );
    state
        .finish_turn_ok_if_runtime_matches(&child_id, &child_token)
        .expect("completion should succeed");

    let child_messages_before = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_id)
            .expect("child session should exist")
            .session
            .messages
            .len()
    };

    let response = state
        .followup_delegation(
            &parent_session_id,
            &created.delegation.id,
            "one more thing".to_owned(),
        )
        .expect("follow-up on a completed delegation should resume it");
    assert_eq!(response.delegation.status, DelegationStatus::Running);
    assert!(
        response.delegation.result.is_none(),
        "re-arm must clear the stale terminal result"
    );
    assert!(
        response.delegation.completed_at.is_none(),
        "re-arm must clear completed_at"
    );
    // The follow-up prompt must actually be delivered + dispatched to the child, not just
    // the delegation record re-armed (a dropped-dispatch regression must fail here).
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_id)
        .expect("child session should exist");
    assert!(
        child.session.messages.len() > child_messages_before,
        "the follow-up prompt must be delivered to the child transcript"
    );
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// A canceled delegation is terminal but must NOT be resumable (re-arming would silently
// undo the cancellation).
#[test]
fn followup_delegation_rejects_canceled_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel me.".to_owned(),
                title: Some("Canceled".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancel should succeed");
    let err = match state.followup_delegation(
        &parent_session_id,
        &created.delegation.id,
        "resume?".to_owned(),
    ) {
        Ok(_) => panic!("follow-up on a canceled delegation must fail"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("canceled"),
        "message should mention canceled: {}",
        err.message
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// A completed delegation whose child session was removed is unresumable.
#[test]
fn followup_delegation_rejects_removed_child() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete then vanish.".to_owned(),
                title: Some("Removed Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let child_id = created.delegation.child_session_id.clone();
    let child_token = runtime_token_for_session(&state, &child_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &child_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDone.",
    );
    state
        .finish_turn_ok_if_runtime_matches(&child_id, &child_token)
        .expect("completion should succeed");
    state
        .kill_session(&child_id)
        .expect("killing the child session should succeed");
    let err = match state.followup_delegation(
        &parent_session_id,
        &created.delegation.id,
        "resume?".to_owned(),
    ) {
        Ok(_) => panic!("follow-up on a delegation with a removed child must fail"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message.contains("no longer exists"),
        "message should mention the missing child: {}",
        err.message
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_completion_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete through lifecycle.".to_owned(),
                title: Some("Lifecycle Completion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nLifecycle hook completed.",
    );

    state
        .finish_turn_ok_if_runtime_matches(&created.delegation.child_session_id, &child_token)
        .expect("production completion lifecycle should succeed");

    let mut saw_completed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_completed_delta |= matches!(
            &event,
            DeltaEvent::DelegationCompleted {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("Lifecycle hook completed")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.source == ParallelAgentSource::Delegation
                        && agent.status == ParallelAgentStatus::Completed
                )
        );
    }
    assert!(
        saw_completed_delta,
        "completion hook should publish delegation completion"
    );
    assert!(
        saw_parent_update,
        "completion hook should update the parent card"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Lifecycle hook completed.")
    );
    assert_single_parent_delegation_agent_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Completed,
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_failure_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail through lifecycle.".to_owned(),
                title: Some("Lifecycle Failure".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .fail_turn_if_runtime_matches(
            &created.delegation.child_session_id,
            &child_token,
            "delegated child failed through lifecycle",
        )
        .expect("production failure lifecycle should succeed");

    let mut saw_failed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_failed_delta |= matches!(
            &event,
            DeltaEvent::DelegationFailed {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("delegated child failed")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        );
    }
    assert!(
        saw_failed_delta,
        "failure hook should publish delegation failure"
    );
    assert!(
        saw_parent_update,
        "failure hook should update the parent card"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .is_some_and(|result| result.summary.contains("delegated child failed"))
    );
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_runtime_exit_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exit through lifecycle.".to_owned(),
                title: Some("Lifecycle Exit".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .handle_runtime_exit_if_matches(
            &created.delegation.child_session_id,
            &child_token,
            Some("delegated child runtime exited"),
        )
        .expect("production runtime-exit lifecycle should succeed");

    let mut saw_failed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_failed_delta |= matches!(
            &event,
            DeltaEvent::DelegationFailed {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("delegated child runtime exited")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        );
    }
    assert!(
        saw_failed_delta,
        "runtime-exit hook should publish delegation failure"
    );
    assert!(
        saw_parent_update,
        "runtime-exit hook should update the parent card"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .is_some_and(|result| result.summary.contains("delegated child runtime exited"))
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn production_completion_clears_queued_child_prompt_before_dispatch() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-queued-complete");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete with queued child prompt.".to_owned(),
                title: Some("Queued Completion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    queue_delegation_child_prompt(
        &state,
        &created.delegation.child_session_id,
        "queued prompt must not run after terminal result",
    );
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nQueued completion done.",
    );

    state
        .finish_turn_ok_if_runtime_matches(&created.delegation.child_session_id, &child_token)
        .expect("production completion lifecycle should succeed");
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminal completion should clear queued child prompts before dispatch"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn production_failure_clears_queued_child_prompt_before_dispatch() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-queued-failure");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail with queued child prompt.".to_owned(),
                title: Some("Queued Failure".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    queue_delegation_child_prompt(
        &state,
        &created.delegation.child_session_id,
        "queued prompt must not run after failed result",
    );
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .fail_turn_if_runtime_matches(
            &created.delegation.child_session_id,
            &child_token,
            "queued failure should terminalize",
        )
        .expect("production failure lifecycle should succeed");
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminal failure should clear queued child prompts before dispatch"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_json_rejections_use_api_error_shape() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    for body in [
        r#"{}"#,
        r#"{"prompt":"valid shape but no content-type"}"#,
        r#"{"prompt":"unterminated"#,
    ] {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"));
        if body != r#"{"prompt":"valid shape but no content-type"}"# {
            builder = builder.header("content-type", "application/json");
        }
        let (status, response): (StatusCode, Value) =
            request_json(&app, builder.body(Body::from(body)).unwrap()).await;

        if body == r#"{"prompt":"valid shape but no content-type"}"# {
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        } else {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert!(
            response["error"]
                .as_str()
                .expect("error response should include a message")
                .contains("invalid delegation request JSON")
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_is_derived_from_completed_child_session() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Summarize the current test shape.".to_owned(),
                title: Some("Result Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let running_command_id = inner.next_message_id();
        let success_command_id = inner.next_message_id();
        let error_command_id = inner.next_message_id();
        let message_id = inner.next_message_id();
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::Command {
                id: running_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo check".to_owned(),
                command_language: Some("shell".to_owned()),
                output: String::new(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Running,
            },
        );
        push_message_on_record(
            child,
            Message::Command {
                id: success_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo test delegations".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
            },
        );
        push_message_on_record(
            child,
            Message::Command {
                id: error_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "false".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "failed".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Error,
            },
        );
        push_message_on_record(
            child,
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "## Result\n\nStatus: completed\n\nSummary:\nNo issues found.".to_owned(),
                expanded_text: None,
                source: None,
            },
        );
        child.session.status = SessionStatus::Idle;
        child.session.preview = "No issues found.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("result polling should refresh completed child state");
    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert!(response.result.summary.contains("No issues found"));
    assert_eq!(
        response
            .result
            .commands_run
            .iter()
            .map(|command| command.status.as_str())
            .collect::<Vec<_>>(),
        ["running", "success", "error"]
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should still exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::ParallelAgents { agents, .. }
                if agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.source == ParallelAgentSource::Delegation
                        && agent.status == ParallelAgentStatus::Completed
                )
        )),
        "parent delegation card should reflect completion"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_completion_clears_child_follow_up_queue() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Finish before queued child follow-up.".to_owned(),
                title: Some("Queued Follow-up".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "unrelated follow-up".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDelegated turn done.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete despite queued follow-up");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Delegated turn done.")
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child should still exist");
    assert!(
        child.queued_prompts.is_empty(),
        "terminal delegation refresh should not leave queued child prompts dispatchable"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_failed_result_clears_child_follow_up_queue() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail before queued child follow-up.".to_owned(),
                title: Some("Queued Failed Follow-up".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "do not dispatch after failure".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: failed\n\nSummary:\nDelegated turn failed.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should fail delegation despite queued follow-up");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child should still exist");
    assert!(
        child.queued_prompts.is_empty(),
        "failed delegation refresh should not leave queued child prompts dispatchable"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_parent_card_changes_emit_transcript_deltas() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Summarize parent-card live updates.".to_owned(),
                title: Some("Parent Card Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut parent_message_id = None;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::MessageCreated {
            session_id,
            message_id,
            message,
            session_mutation_stamp,
            ..
        } = event
        {
            if session_id == parent_session_id {
                assert!(session_mutation_stamp.is_some());
                match message {
                    Message::ParallelAgents { agents, .. } => {
                        assert!(agents.iter().any(|agent| {
                            agent.id == created.delegation.id
                                && agent.status == ParallelAgentStatus::Running
                        }));
                    }
                    _ => panic!("parent card should be a parallel-agents message"),
                }
                parent_message_id = Some(message_id);
                break;
            }
        }
    }
    let parent_message_id =
        parent_message_id.expect("parent MessageCreated delta should be published");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReady.",
    );
    let _ = state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                message_id,
                agents,
                session_mutation_stamp,
                ..
            } if session_id == parent_session_id
                && message_id == parent_message_id
                && session_mutation_stamp.is_some()
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Completed
                )
        );
    }
    assert!(
        saw_parent_update,
        "parent ParallelAgentsUpdate delta should be published"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_limit_allows_new_children_after_terminal_states() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut created = Vec::new();
    for index in 0..MAX_RUNNING_DELEGATIONS_PER_PARENT {
        created.push(
            state
                .create_read_only_delegation(
                    &parent_session_id,
                    CreateDelegationRequest {
                        prompt: format!("Create child {index}."),
                        title: Some(format!("Limited Child {index}")),
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Reviewer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
                .expect("delegation under limit should be created"),
        );
    }

    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "This should exceed the active limit.".to_owned(),
            title: Some("Too Many".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("fifth active delegation should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message
            .contains("parent session already has 4 active delegations")
    );

    finish_delegation_child_with_assistant_text(
        &state,
        &created[0].delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nFreed capacity.",
    );
    state
        .refresh_delegation_for_child_session(&created[0].delegation.child_session_id)
        .expect("completed delegation should refresh");
    state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Completion should free one slot.".to_owned(),
                title: Some("After Completion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("completed delegation should not count against active limit");

    state
        .cancel_delegation(&parent_session_id, &created[1].delegation.id)
        .expect("running delegation should cancel");
    state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancellation should free one slot.".to_owned(),
                title: Some("After Cancellation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("canceled delegation should not count against active limit");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_nesting_depth_rejects_fourth_generation_child() {
    let state = test_app_state();
    let root_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &root_session_id,
            CreateDelegationRequest {
                prompt: "Create depth one.".to_owned(),
                title: Some("Depth One".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first generation should be created");
    let second = state
        .create_read_only_delegation(
            &first.delegation.child_session_id,
            CreateDelegationRequest {
                prompt: "Create depth two.".to_owned(),
                title: Some("Depth Two".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second generation should be created");
    let third = state
        .create_read_only_delegation(
            &second.delegation.child_session_id,
            CreateDelegationRequest {
                prompt: "Create depth three.".to_owned(),
                title: Some("Depth Three".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("third generation should be created");

    let err = match state.create_read_only_delegation(
        &third.delegation.child_session_id,
        CreateDelegationRequest {
            prompt: "Create depth four.".to_owned(),
            title: Some("Depth Four".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("fourth generation should exceed nesting depth"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message
            .contains("delegation nesting depth is limited to 3")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_delegation_start_keeps_child_session_as_error_transcript() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nforce failure"),
                title: Some("Failing Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("start failure should return a failed delegation response");

    assert_eq!(created.delegation.status, DelegationStatus::Failed);
    assert_eq!(created.child_session.status, SessionStatus::Error);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("failed start response should reference a durable child session");
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(child.queued_prompts.is_empty());
    let stored = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("failed delegation record should remain");
    assert_eq!(stored.status, DelegationStatus::Failed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should still exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::ParallelAgents { agents, .. }
                if agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        )),
        "parent delegation card should show the failed start"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_failed_start_response_matches_durable_child_state() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let (status, response): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "prompt": format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nforce failure"),
                    "title": "Route Failed Start",
                    "mode": "reviewer",
                    "writePolicy": { "kind": "readOnly" }
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.delegation.status, DelegationStatus::Failed);
    assert_eq!(response.child_session.status, SessionStatus::Error);
    assert_eq!(
        response.child_session.id,
        response.delegation.child_session_id
    );
    assert!(
        response
            .child_session
            .preview
            .contains("child session failed to start")
    );
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("child session failed to start")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(response.revision, inner.revision);
    let stored_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == response.delegation.id)
        .expect("failed delegation should be durable");
    assert_eq!(stored_delegation, &response.delegation);
    let stored_child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == response.delegation.child_session_id)
        .expect("failed child session should be durable");
    assert_eq!(stored_child.session.status, response.child_session.status);
    assert_eq!(stored_child.session.preview, response.child_session.preview);
    assert_eq!(
        stored_child.session.parent_delegation_id.as_deref(),
        Some(response.delegation.id.as_str())
    );
    assert!(stored_child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_creation_publishes_parent_card_message_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Publish parent card delta.".to_owned(),
                title: Some("Parent Delta".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut saw_parent_card_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::MessageCreated {
            session_id,
            message,
            ..
        } = event
        {
            // OR-coalesce so an unrelated MessageCreated arriving after the
            // parent-card delta does not flip the assertion back to false.
            saw_parent_card_delta = saw_parent_card_delta
                || (session_id == parent_session_id
                    && matches!(
                        message,
                        Message::ParallelAgents { agents, .. }
                            if agents.iter().any(|agent| agent.id == created.delegation.id)
                    ));
        }
    }

    assert!(saw_parent_card_delta);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_get_refreshes_child_state() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Read current state only.".to_owned(),
                title: Some("Read-only status".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReady.",
    );
    let revision_before_get = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.revision
    };

    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status should be readable");

    assert!(
        response.revision > revision_before_get,
        "polling status should persist the terminal child refresh"
    );
    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    assert!(response.delegation.result.is_some());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_and_unavailable_result_poll_preserve_revision_without_refresh() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Stay running.".to_owned(),
                title: Some("No-op Poll".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let revision_before_poll = state.snapshot().revision;

    let status_response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status should be readable");

    assert_eq!(status_response.revision, revision_before_poll);
    assert_eq!(state.snapshot().revision, revision_before_poll);

    let err = match state.get_delegation_result(&parent_session_id, &created.delegation.id) {
        Ok(_) => panic!("running delegation should not expose a result"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(state.snapshot().revision, revision_before_poll);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_consumes_satisfied_wait_for_busy_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Polling Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Poll fan-in".to_owned()),
            },
        )
        .expect("running delegation wait should be accepted");
    assert!(!wait.resume_prompt_queued);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nPolling should observe this result.",
    );

    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status polling should refresh completed child state");

    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.session.status, SessionStatus::Active);
    assert_eq!(parent.queued_prompts.len(), 1);
    let resume_prompt = &parent.queued_prompts[0].pending_prompt.text;
    assert!(resume_prompt.contains("Poll fan-in"));
    assert!(resume_prompt.contains("Polling should observe this result."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_poll_consumes_satisfied_wait_for_busy_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Result Polling Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Result poll fan-in".to_owned()),
            },
        )
        .expect("running delegation wait should be accepted");
    assert!(!wait.resume_prompt_queued);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nResult polling should observe this result.",
    );

    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("result polling should refresh completed child state");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.session.status, SessionStatus::Active);
    assert_eq!(parent.queued_prompts.len(), 1);
    let resume_prompt = &parent.queued_prompts[0].pending_prompt.text;
    assert!(resume_prompt.contains("Result poll fan-in"));
    assert!(resume_prompt.contains("Result polling should observe this result."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_any_wait_uses_cached_sibling_state_until_polled() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let polled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Poll this running delegation first.".to_owned(),
                title: Some("Polled Running Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("polled delegation should be created");
    let sibling = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Finish before the wait is evaluated.".to_owned(),
                title: Some("Sibling Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("sibling delegation should be created");

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![polled.delegation.id.clone(), sibling.delegation.id.clone()],
                mode: DelegationWaitMode::Any,
                title: Some("Any-mode sibling fan-in".to_owned()),
            },
        )
        .expect("running any-mode wait should be accepted");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &sibling.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nSibling completed first.",
    );

    let response = state
        .get_delegation(&parent_session_id, &polled.delegation.id)
        .expect("polling the running delegation should succeed");

    assert_eq!(response.delegation.status, DelegationStatus::Running);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.delegation_waits.len(), 1);
        assert_eq!(inner.delegation_waits[0].id, wait.wait.id);
        let sibling_record = inner
            .delegations
            .iter()
            .find(|delegation| delegation.id == sibling.delegation.id)
            .expect("sibling delegation should exist");
        assert_eq!(sibling_record.status, DelegationStatus::Running);
        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent should exist");
        assert!(parent.queued_prompts.is_empty());
    }

    let sibling_response = state
        .get_delegation(&parent_session_id, &sibling.delegation.id)
        .expect("polling the sibling should refresh it");

    assert_eq!(
        sibling_response.delegation.status,
        DelegationStatus::Completed
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains("Any-mode sibling fan-in")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_reconciles_missing_child_and_consumes_wait() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "The child will disappear before polling.".to_owned(),
                title: Some("Missing Child Poll".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Missing child poll fan-in".to_owned()),
            },
        )
        .expect("wait should be accepted");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        inner.remove_session_at(child_index);
        state
            .commit_locked(&mut inner)
            .expect("manual child removal should persist");
    }

    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status polling should reconcile the missing child");

    assert_eq!(response.delegation.status, DelegationStatus::Failed);
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .expect("missing child should produce a result")
            .summary,
        "delegation child session no longer exists"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains("Missing child poll fan-in")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_poll_conflict_still_publishes_wait_side_effects() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let polled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remain running while another child satisfies any-mode wait.".to_owned(),
                title: Some("Running Result Poll".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("polled delegation should be created");
    let completed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete before result polling.".to_owned(),
                title: Some("Completed Sibling".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("completed delegation should be created");
    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![
                    polled.delegation.id.clone(),
                    completed.delegation.id.clone(),
                ],
                mode: DelegationWaitMode::Any,
                title: Some("Result-conflict fan-in".to_owned()),
            },
        )
        .expect("any-mode wait should be accepted");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
    }
    finish_delegation_child_with_assistant_text(
        &state,
        &completed.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nCompleted sibling satisfies any-mode wait.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&completed.delegation.id)
            .expect("completed sibling should exist");
        assert!(refresh_delegation_from_child_locked(&mut inner, index).is_some());
        state
            .commit_locked(&mut inner)
            .expect("manual sibling refresh should persist");
    }
    let mut delta_events = state.subscribe_delta_events();

    let err = match state.get_delegation_result(&parent_session_id, &polled.delegation.id) {
        Ok(_) => panic!("running delegation result should remain unavailable"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains("Result-conflict fan-in")
    );
    drop(inner);

    let mut saw_wait_consumed = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if matches!(
            event,
            DeltaEvent::DelegationWaitConsumed {
                wait_id,
                reason: DelegationWaitConsumedReason::Completed,
                ..
            } if wait_id == wait.wait.id
        ) {
            saw_wait_consumed = true;
        }
    }
    assert!(
        saw_wait_consumed,
        "result polling should publish wait consumption before returning 409"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_does_not_consume_unrelated_satisfied_wait() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let polled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Poll this delegation.".to_owned(),
                title: Some("Polled Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("polled delegation should be created");
    let unrelated = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Leave this wait pending.".to_owned(),
                title: Some("Unrelated Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("unrelated delegation should be created");

    let unrelated_wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![unrelated.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Unrelated fan-in".to_owned()),
            },
        )
        .expect("running unrelated wait should be accepted");

    finish_delegation_child_with_assistant_text(
        &state,
        &unrelated.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nUnrelated wait is satisfied.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&unrelated.delegation.id)
            .expect("unrelated delegation should exist");
        assert!(refresh_delegation_from_child_locked(&mut inner, index).is_some());
        state
            .commit_locked(&mut inner)
            .expect("manual refresh should persist");
    }
    finish_delegation_child_with_assistant_text(
        &state,
        &polled.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nPolled delegation is complete.",
    );

    let response = state
        .get_delegation(&parent_session_id, &polled.delegation.id)
        .expect("status polling should refresh the polled delegation");

    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.delegation_waits.len(), 1);
    assert_eq!(inner.delegation_waits[0].id, unrelated_wait.wait.id);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(parent.queued_prompts.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_public_result_summary_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a long result.".to_owned(),
                title: Some("Long Summary".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let long_summary = "x".repeat(MAX_DELEGATION_PUBLIC_SUMMARY_CHARS + 128);
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        &format!("## Result\n\nStatus: completed\n\nSummary:\n{long_summary}"),
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");

    let result = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("full result should be available");
    assert_eq!(result.result.summary, long_summary);

    let snapshot = state.snapshot();
    let public_summary = snapshot
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .and_then(|delegation| delegation.result.as_ref())
        .map(|result| result.summary.as_str())
        .expect("public result summary should be present");
    assert_eq!(
        public_summary.chars().count(),
        MAX_DELEGATION_PUBLIC_SUMMARY_CHARS + 3
    );
    assert!(public_summary.ends_with("..."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn cancel_preserves_completed_delegation_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Finish before cancel.".to_owned(),
                title: Some("Cancel Race".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nAlready done.",
    );
    let mut delta_events = state.subscribe_delta_events();
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should terminalize delegation before cancel");
    let mut saw_completed_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let Ok(event) = serde_json::from_str::<DeltaEvent>(&payload) else {
            continue;
        };
        if matches!(
            event,
            DeltaEvent::DelegationCompleted {
                delegation_id,
                ..
            } if delegation_id == created.delegation.id
        ) {
            saw_completed_delta = true;
        }
    }
    assert!(
        saw_completed_delta,
        "refresh should publish the completed delegation delta before cancel"
    );
    let pre_cancel_revision = state.snapshot().revision;
    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancel should preserve completed state");

    assert_eq!(response.revision, pre_cancel_revision);
    assert_eq!(state.snapshot().revision, pre_cancel_revision);
    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .expect("completed result should be retained")
            .summary,
        "Already done."
    );
    while let Ok(payload) = delta_events.try_recv() {
        let Ok(event) = serde_json::from_str::<DeltaEvent>(&payload) else {
            continue;
        };
        assert!(
            !matches!(
                event,
                DeltaEvent::DelegationCreated { .. }
                    | DeltaEvent::DelegationUpdated { .. }
                    | DeltaEvent::DelegationCompleted { .. }
                    | DeltaEvent::DelegationFailed { .. }
                    | DeltaEvent::DelegationCanceled { .. }
            ),
            "terminal cancel should not publish delegation deltas"
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn completed_delegation_refresh_detaches_and_kills_child_runtime() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete and clean up runtime.".to_owned(),
                title: Some("Runtime Cleanup".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let process = attach_sleeping_claude_runtime_to_delegation_child(
        &state,
        &created.delegation.child_session_id,
    );

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDone.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete delegation");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should remain for transcript inspection");
    assert!(
        matches!(child.runtime, SessionRuntime::None),
        "terminal delegation child should not keep a runtime handle"
    );
    drop(inner);
    assert!(
        wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_secs(1),
            "delegation child runtime"
        )
        .expect("runtime wait should succeed")
        .is_some(),
        "terminal delegation refresh should reap the child runtime process"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn cancel_preserves_failed_delegation_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail before cancel.".to_owned(),
                title: Some("Failed Cancel".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: failed\n\nSummary:\nAlready failed.",
    );
    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("failed delegation cancel should return current terminal status");

    assert_eq!(response.delegation.status, DelegationStatus::Failed);
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Already failed.")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_cancel_unknown_id_returns_not_found() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/missing-delegation/cancel"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_cancel_running_runtime_route_interrupts_child() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-route-cancel-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel while running.".to_owned(),
                title: Some("Running Cancel".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    let runtime = shared_codex_runtime_for_state(&state);
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            created.delegation.child_session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("delegation-cancel-thread".to_owned()),
                turn_id: Some("delegation-cancel-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "delegation-cancel-thread".to_owned(),
            created.delegation.child_session_id.clone(),
        );

    let command_thread = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("delegation cancel should interrupt the running child");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "delegation-cancel-thread");
                assert_eq!(turn_id, "delegation-cancel-turn");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected child turn interrupt command"),
        }
    });

    let (status, response): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/cancel",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    command_thread
        .join()
        .expect("delegation cancel command thread should join cleanly");

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    drop(inner);
    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&created.delegation.child_session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("delegation-cancel-thread")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_cancel_conflicted_stop_interrupts_before_detaching_child() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-cancel-stop-conflict");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel while stop is already in progress.".to_owned(),
                title: Some("Cancel Stop Conflict".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    let runtime = shared_codex_runtime_for_state(&state);
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            created.delegation.child_session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("delegation-cancel-conflict-thread".to_owned()),
                turn_id: Some("delegation-cancel-conflict-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "delegation-cancel-conflict-thread".to_owned(),
            created.delegation.child_session_id.clone(),
        );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.runtime_stop_in_progress = true;
    }

    let command_thread = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("conflicted delegation cancel should still interrupt the child");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "delegation-cancel-conflict-thread");
                assert_eq!(turn_id, "delegation-cancel-conflict-turn");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected child turn interrupt command"),
        }
    });

    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("delegation cancel should succeed");
    command_thread
        .join()
        .expect("delegation cancel command thread should join cleanly");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(!child.runtime_stop_in_progress);
    drop(inner);
    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&created.delegation.child_session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("delegation-cancel-conflict-thread")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn mark_delegation_canceled_sets_child_session_idle() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel through locked marker.".to_owned(),
                title: Some("Locked Cancel".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    let child_index = inner
        .find_session_index(&created.delegation.child_session_id)
        .expect("child session should exist");
    inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid")
        .session
        .status = SessionStatus::Active;
    mark_delegation_canceled_locked(&mut inner, delegation_index, None)
        .expect("running delegation should transition to canceled");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should still exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_start_cleanup_is_noop_for_already_terminal_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete before a late start error.".to_owned(),
                title: Some("Late Start Error".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nAlready done.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete delegation");
    let mut delta_events = state.subscribe_delta_events();
    let pre_error_revision = state.snapshot().revision;

    state
        .mark_delegation_failed_after_start_error(
            &created.delegation.id,
            &created.delegation.child_session_id,
            "late start error",
        )
        .expect("late failed-start cleanup should be a no-op");

    assert_eq!(state.snapshot().revision, pre_error_revision);
    assert!(
        delta_events.try_recv().is_err(),
        "no delegation delta should be published for terminal failed-start cleanup"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let stored = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation record should remain");
    assert_eq!(stored.status, DelegationStatus::Completed);
    assert_eq!(
        stored
            .result
            .as_ref()
            .expect("completed result should remain")
            .summary,
        "Already done."
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn terminal_read_only_delegations_do_not_keep_child_session_write_blocked() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let completed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete and unblock.".to_owned(),
                title: Some("Completed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("completed delegation should be created");
    let canceled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel and unblock.".to_owned(),
                title: Some("Canceled Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("canceled delegation should be created");
    let failed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nfail and unblock"),
                title: Some("Failed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("failed delegation should still return a child session");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let running_ids = inner
            .running_read_only_delegations
            .iter()
            .filter_map(|index| inner.delegations.get(*index))
            .map(|delegation| delegation.id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(running_ids.contains(completed.delegation.id.as_str()));
        assert!(running_ids.contains(canceled.delegation.id.as_str()));
        assert!(!running_ids.contains(failed.delegation.id.as_str()));
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &completed.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDone.",
    );
    state
        .refresh_delegation_for_child_session(&completed.delegation.child_session_id)
        .expect("refresh should complete delegation");
    state
        .cancel_delegation(&parent_session_id, &canceled.delegation.id)
        .expect("cancel should terminalize delegation");

    for (child_session_id, name) in [
        (
            completed.delegation.child_session_id.as_str(),
            "completed child rename",
        ),
        (
            canceled.delegation.child_session_id.as_str(),
            "canceled child rename",
        ),
        (
            failed.delegation.child_session_id.as_str(),
            "failed child rename",
        ),
    ] {
        state
            .ensure_read_only_delegation_allows_session_write_action(
                Some(child_session_id),
                "session settings",
            )
            .expect("terminal delegation should not block child writes");
        state
            .update_session_settings(
                child_session_id,
                UpdateSessionSettingsRequest {
                    name: Some(name.to_owned()),
                    model: None,
                    approval_policy: None,
                    reasoning_effort: None,
                    sandbox_mode: None,
                    cursor_mode: None,
                    claude_approval_mode: None,
                    claude_effort: None,
                    gemini_approval_mode: None,
                },
            )
            .expect("terminal child session settings should update");
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.running_read_only_delegations.is_empty(),
        "terminal delegation transitions should leave no running read-only index entries"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_cancel_clears_queued_child_prompts() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Queue then cancel.".to_owned(),
                title: Some("Cancel Queue".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "do not dispatch after cancel".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancel should terminalize delegation");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should remain visible");
    assert!(
        child.queued_prompts.is_empty(),
        "cancel should not leave queued child prompts available for dispatch"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_child_or_parent_terminalizes_records() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let child_removed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove child.".to_owned(),
                title: Some("Removed Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .kill_session(&child_removed.delegation.child_session_id)
        .expect("child removal should succeed");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let delegation = inner
            .delegations
            .iter()
            .find(|delegation| delegation.id == child_removed.delegation.id)
            .expect("delegation record should remain");
        assert_eq!(delegation.status, DelegationStatus::Failed);
    }

    let mut delta_events = state.subscribe_delta_events();
    let parent_removed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove parent.".to_owned(),
                title: Some("Removed Parent".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    while delta_events.try_recv().is_ok() {}
    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");
    while let Ok(payload) = delta_events.try_recv() {
        let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        assert!(
            !matches!(
                delta,
                DeltaEvent::ParallelAgentsUpdate { session_id, .. }
                    if session_id == parent_session_id
            ),
            "parent removal should not publish a parent-card delta for the removed session"
        );
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == parent_removed.delegation.id)
        .expect("delegation record should remain");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != parent_removed.delegation.child_session_id),
        "parent removal should cascade-delete its delegated child session"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_cascades_to_nested_delegation_children() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let child = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegate first-level review.".to_owned(),
                title: Some("Cascade Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first-level delegation should be created");
    let child_session_id = child.delegation.child_session_id.clone();
    let grandchild = state
        .create_read_only_delegation(
            &child_session_id,
            CreateDelegationRequest {
                prompt: "Delegate nested review.".to_owned(),
                title: Some("Cascade Grandchild".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("nested delegation should be created");
    let grandchild_session_id = grandchild.delegation.child_session_id.clone();
    state
        .set_external_session_id(&child_session_id, "cascade-child-thread".to_owned())
        .expect("child Codex thread id should be recorded");
    state
        .set_external_session_id(
            &grandchild_session_id,
            "cascade-grandchild-thread".to_owned(),
        )
        .expect("grandchild Codex thread id should be recorded");

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.sessions.iter().all(|record| {
            record.session.id != parent_session_id
                && record.session.id != child_session_id
                && record.session.id != grandchild_session_id
        }),
        "parent removal should delete the full delegated child tree"
    );
    let child_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == child.delegation.id)
        .expect("child delegation record should remain");
    assert_eq!(child_delegation.status, DelegationStatus::Failed);
    let grandchild_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == grandchild.delegation.id)
        .expect("grandchild delegation record should remain");
    assert_eq!(grandchild_delegation.status, DelegationStatus::Failed);
    drop(inner);

    let mut reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("cascade-child-thread")
    );
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("cascade-grandchild-thread")
    );
    reloaded_inner.import_discovered_codex_threads(
        "/tmp",
        vec![
            DiscoveredCodexThread {
                approval_policy: Some(CodexApprovalPolicy::Never),
                archived: false,
                cwd: "/tmp".to_owned(),
                id: "cascade-child-thread".to_owned(),
                model: Some("gpt-5-codex".to_owned()),
                reasoning_effort: Some(CodexReasoningEffort::Medium),
                sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
                title: "Cascade child thread".to_owned(),
            },
            DiscoveredCodexThread {
                approval_policy: Some(CodexApprovalPolicy::Never),
                archived: false,
                cwd: "/tmp".to_owned(),
                id: "cascade-grandchild-thread".to_owned(),
                model: Some("gpt-5-codex".to_owned()),
                reasoning_effort: Some(CodexReasoningEffort::Medium),
                sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
                title: "Cascade grandchild thread".to_owned(),
            },
        ],
    );
    assert!(
        reloaded_inner.sessions.iter().all(|record| {
            !matches!(
                record.external_session_id.as_deref(),
                Some("cascade-child-thread" | "cascade-grandchild-thread")
            )
        }),
        "cascade-deleted Codex child threads should not be rediscovered"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_reaps_hidden_claude_spare_for_child_profile() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegate Claude-profile review.".to_owned(),
                title: Some("Claude Profile Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let child_session_id = created.delegation.child_session_id.clone();
    let spare_profile = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.agent = Agent::Claude;
        child.session.model = "claude-profile-child".to_owned();
        child.session.claude_approval_mode = Some(ClaudeApprovalMode::Plan);
        child.session.claude_effort = Some(ClaudeEffortLevel::High);
        let profile = claude_spare_profile(child);
        let spare_id = inner
            .ensure_hidden_claude_spare(
                profile.0.clone(),
                profile.1.clone(),
                profile.2.clone(),
                profile.3,
                profile.4,
            )
            .expect("hidden Claude spare should be reserved");
        assert!(
            inner.sessions.iter().any(|record| {
                record.session.id == spare_id
                    && record.hidden
                    && record.session.agent == Agent::Claude
            }),
            "test setup should create a matching hidden Claude spare"
        );
        state.commit_locked(&mut inner).unwrap();
        profile
    };

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != child_session_id),
        "child session should be deleted"
    );
    assert!(
        inner.sessions.iter().all(|record| {
            !(record.hidden
                && record.session.agent == Agent::Claude
                && claude_spare_profile(record) == spare_profile)
        }),
        "hidden Claude spare for a cascade-deleted child should be reaped"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_deletes_child_runtime_and_session() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-parent-remove-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove parent while child is running.".to_owned(),
                title: Some("Parent Removed Runtime".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    let runtime = shared_codex_runtime_for_state(&state);
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            created.delegation.child_session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("delegation-parent-remove-thread".to_owned()),
                turn_id: Some("delegation-parent-remove-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "delegation-parent-remove-thread".to_owned(),
            created.delegation.child_session_id.clone(),
        );

    let _pending_message_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let pending_message_id = inner.next_message_id();
        let queued_prompt_id = inner.next_message_id();
        let questions = vec![UserInputQuestion {
            header: "Choice".to_owned(),
            id: "choice".to_owned(),
            is_other: false,
            is_secret: false,
            options: None,
            question: "Continue?".to_owned(),
        }];
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::UserInputRequest {
                id: pending_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Need input".to_owned(),
                detail: "Waiting for user input".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                submitted_answers: None,
            },
        );
        child.pending_codex_user_inputs.insert(
            pending_message_id.clone(),
            CodexPendingUserInput {
                questions,
                request_id: json!("pending-user-input"),
            },
        );
        child.queued_prompts.push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: queued_prompt_id,
                timestamp: stamp_now(),
                text: "do not dispatch after parent removal".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
        sync_pending_prompts(child);
        child.deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
        pending_message_id
    };
    while delta_events.try_recv().is_ok() {}

    let command_thread = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("parent removal should interrupt the running child");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "delegation-parent-remove-thread");
                assert_eq!(turn_id, "delegation-parent-remove-turn");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected child turn interrupt command"),
        }
    });

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");
    command_thread
        .join()
        .expect("parent removal command thread should join cleanly");

    let mut saw_child_transcript_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        match delta {
            DeltaEvent::MessageUpdated { session_id, .. }
            | DeltaEvent::MessageCreated { session_id, .. }
                if session_id == created.delegation.child_session_id =>
            {
                saw_child_transcript_delta = true;
            }
            DeltaEvent::ParallelAgentsUpdate { session_id, .. }
                if session_id == parent_session_id =>
            {
                panic!(
                    "parent removal should not publish a parent-card delta for the removed session"
                )
            }
            _ => {}
        }
    }
    assert!(
        !saw_child_transcript_delta,
        "deleted child sessions should not receive transcript deltas"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != created.delegation.child_session_id),
        "parent removal should delete the child session record"
    );
    drop(inner);

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&created.delegation.child_session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("delegation-parent-remove-thread")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
