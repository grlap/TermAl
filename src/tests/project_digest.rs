// Tests for the project digest surfaces in `src/api.rs`.
//
// A "project digest" is TermAl's rollup of a project's current state: a
// headline, a done summary, the live status line, and a short list of
// proposed next actions. It is rendered in the sidebar next to each
// project and also pushed periodically to Telegram. Each proposed action
// is a `ProjectActionId` enum variant (e.g. `Approve`, `ReviewInTermal`,
// `KeepIterating`) that the UI sends back through `dispatch_project_action`,
// which picks the most relevant session and either answers a pending
// approval, sends a prompt, or triggers a stop. These tests exercise the
// two public surfaces: `get_project_digest` (builds a `ProjectDigestResponse`
// off the current session state) and `dispatch_project_action` (routes a
// clicked action into a concrete runtime command).

use super::*;

// Pins that when any session in the project is blocked on an approval
// (Codex waiting on a command-execution decision), the digest promotes
// that session as the primary, reports "Waiting on your decision.", and
// offers `approve` / `reject` / `review-in-termal` as the actions.
// Guards against a regression where a pending approval could be buried
// behind other activity and the user would be unable to unblock the
// agent from the sidebar or Telegram.
#[test]
fn project_digest_surfaces_pending_approval_actions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-digest-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Digest Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implemented the requested fix.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-digest"),
            },
        )
        .unwrap();

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Waiting on your decision.");
    assert_eq!(digest.done_summary, "Implemented the requested fix.");
    assert_eq!(digest.source_message_ids[0], approval_message_id);
    assert_eq!(action_ids, vec!["approve", "reject", "review-in-termal"]);

    fs::remove_dir_all(root).unwrap();
}

// Pins that for an idle project with uncommitted git changes the digest
// reports "Changes are ready for review." and proposes review-first
// actions (`review-in-termal`, `ask-agent-to-commit`, `keep-iterating`)
// rather than approval or stop controls. Guards against a regression
// where a finished-but-dirty session would either show no actions at
// all or surface agent-control actions that make no sense while the
// agent is idle.
#[test]
fn project_digest_prefers_review_actions_for_dirty_idle_project() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-review-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Review Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);

    let digest = state.project_digest(&project_id).unwrap();
    let action_ids = digest
        .proposed_actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(digest.current_status, "Changes are ready for review.");
    assert!(digest.done_summary.contains("1 changed file"));
    assert_eq!(
        action_ids,
        vec!["review-in-termal", "ask-agent-to-commit", "keep-iterating"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}

#[test]
fn project_digest_routes_dirty_project_prompts_to_non_delegation_session() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!(
        "termal-project-delegation-target-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Delegation Target Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);
    let child_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);
    state
        .push_message(
            &child_session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Delegation result should inform the summary but not receive prompts."
                    .to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx) = test_codex_runtime_handle("project-delegation-target");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner.find_session_index(&parent_session_id).unwrap();
        inner.sessions[parent_index].runtime = SessionRuntime::Codex(runtime);
        let child_index = inner.find_session_index(&child_session_id).unwrap();
        inner.sessions[child_index].session.parent_delegation_id =
            Some("delegation-finished".to_owned());
        state.commit_locked(&mut inner).unwrap();
    }

    let digest = state.project_digest(&project_id).unwrap();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(
        digest.deep_link.as_deref(),
        Some(format!("/?projectId={project_id}&sessionId={parent_session_id}").as_str())
    );
    assert!(digest.source_message_ids.is_empty());
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["review-in-termal", "ask-agent-to-commit", "keep-iterating"]
    );

    state
        .execute_project_action(&project_id, "keep-iterating")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert_eq!(
                command.prompt,
                ProjectActionId::KeepIterating.prompt().unwrap()
            );
        }
        _ => panic!("expected parent prompt dispatch"),
    }

    fs::remove_dir_all(repo_root).unwrap();
}

#[test]
fn project_digest_routes_clean_continue_to_non_delegation_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-clean-delegation-target-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Clean Delegation Target Project");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let child_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let child_message_id = state.allocate_message_id();
    state
        .push_message(
            &child_session_id,
            Message::Text {
                attachments: Vec::new(),
                id: child_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Delegation found no changes to make.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx) = test_codex_runtime_handle("project-clean-delegation-target");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner.find_session_index(&parent_session_id).unwrap();
        inner.sessions[parent_index].runtime = SessionRuntime::Codex(runtime);
        let child_index = inner.find_session_index(&child_session_id).unwrap();
        inner.sessions[child_index].session.parent_delegation_id =
            Some("delegation-finished".to_owned());
        state.commit_locked(&mut inner).unwrap();
    }

    let digest = state.project_digest(&project_id).unwrap();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(
        digest.deep_link.as_deref(),
        Some(format!("/?projectId={project_id}&sessionId={parent_session_id}").as_str())
    );
    assert_eq!(digest.source_message_ids, vec![child_message_id]);
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["continue", "review-in-termal"]
    );

    state
        .execute_project_action(&project_id, "continue")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert_eq!(command.prompt, ProjectActionId::Continue.prompt().unwrap());
        }
        _ => panic!("expected parent prompt dispatch"),
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_digest_routes_error_fix_it_to_non_delegation_session() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-error-delegation-target-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Error Delegation Target Project");
    let parent_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let child_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let child_message_id = state.allocate_message_id();
    state
        .push_message(
            &child_session_id,
            Message::Text {
                attachments: Vec::new(),
                id: child_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Delegation failed while checking the project.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx) = test_codex_runtime_handle("project-error-delegation-target");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner.find_session_index(&parent_session_id).unwrap();
        inner.sessions[parent_index].runtime = SessionRuntime::Codex(runtime);
        let child_index = inner.find_session_index(&child_session_id).unwrap();
        inner.sessions[child_index].session.parent_delegation_id =
            Some("delegation-failed".to_owned());
        inner.sessions[child_index].session.status = SessionStatus::Error;
        inner.sessions[child_index].session.preview =
            "Delegation child failed after review.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let digest = state.project_digest(&project_id).unwrap();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(parent_session_id.as_str())
    );
    assert_eq!(
        digest.deep_link.as_deref(),
        Some(format!("/?projectId={project_id}&sessionId={parent_session_id}").as_str())
    );
    assert_eq!(digest.current_status, "Delegation child failed after review.");
    assert_eq!(digest.source_message_ids, vec![child_message_id]);
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-it", "review-in-termal"]
    );

    state
        .execute_project_action(&project_id, "fix-it")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert_eq!(command.prompt, ProjectActionId::FixIt.prompt().unwrap());
        }
        _ => panic!("expected parent prompt dispatch"),
    }

    fs::remove_dir_all(root).unwrap();
}

// Pins that dispatching the `approve` action on a project finds the
// session with the pending Codex approval, forwards an accept response
// to that runtime on the correct `request_id`, and then returns a
// refreshed digest that no longer offers `approve`. Guards against the
// action being routed to the wrong session, the wrong request id, or
// staying in the proposed list after dispatch, any of which would let
// the UI double-submit or leave the agent blocked.
#[test]
fn project_action_approve_routes_to_the_live_project_approval() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-project-approve-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Approval Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-approve");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let approval_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: approval_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approve command".to_owned(),
                command: "cargo test".to_owned(),
                command_language: Some(shell_language().to_owned()),
                detail: "Approval required.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .unwrap();
    state
        .register_codex_pending_approval(
            &session_id,
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("req-project-approve"),
            },
        )
        .unwrap();

    let digest = state
        .execute_project_action(&project_id, "approve")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(response.request_id, json!("req-project-approve"));
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({ "decision": "accept" }))
            );
        }
        _ => panic!("expected approval response"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert!(
        !digest
            .proposed_actions
            .iter()
            .any(|action| action.id == "approve")
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins that dispatching `keep-iterating` on a dirty idle project sends
// the canonical `ProjectActionId::KeepIterating.prompt()` text into the
// session runtime, flips the digest status to "Agent is working.", and
// narrows the proposed actions to `stop` / `review-in-termal`. Guards
// against a regression where the shared prompt string drifts out of
// sync with the dispatch path or where a now-running session keeps
// advertising idle-only actions back to the UI.
#[test]
fn project_action_keep_iterating_dispatches_a_follow_up_prompt() {
    let state = test_app_state();
    let repo_root = std::env::temp_dir().join(format!("termal-project-iterate-{}", Uuid::new_v4()));
    fs::create_dir_all(repo_root.join("src")).unwrap();
    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 1 }\n",
    )
    .unwrap();

    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "."]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);

    fs::write(
        repo_root.join("src/lib.rs"),
        "pub fn value() -> u32 { 2 }\n",
    )
    .unwrap();

    let project_id = create_test_project(&state, &repo_root, "Iterate Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &repo_root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-iterate");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    let digest = state
        .execute_project_action(&project_id, "keep-iterating")
        .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id: runtime_session_id,
            command,
        } => {
            assert_eq!(runtime_session_id, session_id);
            assert_eq!(
                command.prompt,
                ProjectActionId::KeepIterating.prompt().unwrap()
            );
        }
        _ => panic!("expected prompt dispatch"),
    }

    assert_eq!(digest.current_status, "Agent is working.");
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["stop", "review-in-termal"]
    );

    fs::remove_dir_all(repo_root).unwrap();
}
