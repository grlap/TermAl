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
                source: None,
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
                supported_decisions: None,
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

// Project digests run frequently (including from Telegram polling), so their
// lock-held snapshot must remain independent of transcript payload size. This
// regression builds a deliberately large active transcript and verifies that
// the snapshot contains only bounded summary metadata rather than a cloned
// `SessionRecord` with every message body.
#[test]
fn project_digest_inputs_project_large_transcripts_to_bounded_metadata() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-digest-large-transcript-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Large Transcript Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[session_index];
        record.session.status = SessionStatus::Active;
        for index in 0..1_024 {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: format!("large-transcript-message-{index}"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "recent user message".to_owned(),
                expanded_text: None,
                source: None,
            });
        }
    }

    let inputs = state.project_digest_inputs(&project_id).unwrap();
    let projected = inputs
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("large session should be included in digest inputs");
    let (_, latest_summary) = projected
        .latest_progress_summary
        .as_ref()
        .expect("assistant transcript should produce a progress summary");

    assert_eq!(projected.status, SessionStatus::Active);
    assert!(projected.has_messages);
    assert!(latest_summary.len() < 1_024);
    assert_eq!(projected.pending_prompt_count, 0);

    fs::remove_dir_all(root).unwrap();
}

// Pins the CPU side of the bounded projection contract. The only progress
// message is deliberately older than the retained-tail-sized scan window; a
// full reverse scan would find it, while the production projection must stop
// after a fixed amount of work under the global state mutex.
#[test]
fn project_digest_inputs_bound_worst_case_transcript_scan() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-digest-bounded-scan-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Bounded Scan Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[session_index];
        record.session.status = SessionStatus::Active;
        record.session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: "old-progress-message".to_owned(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "This summary is outside the bounded digest window.".to_owned(),
            expanded_text: None,
            source: None,
        });
        for index in 0..1_024 {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: format!("recent-user-message-{index}"),
                timestamp: stamp_now(),
                author: Author::You,
                text: "x".repeat(16 * 1_024),
                expanded_text: None,
                source: None,
            });
        }
    }

    let inputs = state.project_digest_inputs(&project_id).unwrap();
    let projected = inputs
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("large session should be included in digest inputs");

    assert!(projected.has_messages);
    assert_eq!(projected.latest_progress_summary, None);

    fs::remove_dir_all(root).unwrap();
}

// Live routing registries, not transcript depth, own approval and interaction
// liveness. Both backing cards are deliberately pushed outside the digest's
// bounded text-scan window and must still survive in the projection.
#[test]
fn project_digest_inputs_keep_deep_live_requests_from_routing_registries() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-digest-deep-live-requests-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Deep Live Requests Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let approval_message_id = "deep-live-approval".to_owned();
    let interaction_message_id = "deep-live-interaction".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[session_index];
        record.session.status = SessionStatus::Active;
        record.session.messages.push(Message::Approval {
            id: approval_message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: "Approve deep command".to_owned(),
            command: "cargo test".to_owned(),
            command_language: Some(shell_language().to_owned()),
            detail: "Approval remains live outside the digest scan window.".to_owned(),
            decision: ApprovalDecision::Pending,
            supported_decisions: None,
        });
        record.session.messages.push(Message::UserInputRequest {
            id: interaction_message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: "Choose an option".to_owned(),
            detail: "Interaction remains live outside the digest scan window.".to_owned(),
            questions: Vec::new(),
            state: InteractionRequestState::Pending,
            submitted_answers: None,
        });
        for index in 0..1_024 {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: format!("deep-live-tail-{index}"),
                timestamp: stamp_now(),
                author: Author::You,
                text: "recent user message".to_owned(),
                expanded_text: None,
                source: None,
            });
        }
        record.message_positions = build_message_positions(&record.session.messages);
        record.pending_codex_approvals.insert(
            approval_message_id.clone(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("deep-live-approval-request"),
            },
        );
        record.pending_codex_user_inputs.insert(
            interaction_message_id.clone(),
            CodexPendingUserInput {
                questions: Vec::new(),
                request_id: json!("deep-live-interaction-request"),
            },
        );
    }

    let inputs = state.project_digest_inputs(&project_id).unwrap();
    let projected = inputs
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should be included in digest inputs");
    assert_eq!(
        projected.pending_approval_message_id.as_deref(),
        Some(approval_message_id.as_str())
    );
    assert_eq!(
        projected.pending_interaction_message_id.as_deref(),
        Some(interaction_message_id.as_str())
    );

    fs::remove_dir_all(root).unwrap();
}

// ACP approvals resolve FIFO within their protocol, but project digests must
// still choose the newest renderable candidate across protocol families. If
// the ACP queue head is no longer resident, fall back to a retained ACP card.
#[test]
fn project_digest_orders_mixed_protocol_approvals_and_skips_trimmed_acp_head() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-digest-mixed-approvals-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Mixed Approval Project");
    let session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let approval_message = |id: &str| Message::Approval {
        id: id.to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: format!("Approve {id}"),
        command: "cargo test".to_owned(),
        command_language: Some(shell_language().to_owned()),
        detail: "Mixed-protocol ordering regression.".to_owned(),
        decision: ApprovalDecision::Pending,
        supported_decisions: None,
    };
    let acp_pending = |request_id: &str| AcpPendingApproval {
        allow_once_option_id: Some("allow-once".to_owned()),
        allow_always_option_id: None,
        reject_option_id: Some("reject-once".to_owned()),
        request_id: json!(request_id),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[session_index];
        record.session.messages = vec![
            approval_message("acp-head"),
            approval_message("codex-newer"),
            approval_message("acp-later"),
        ];
        record.message_positions = build_message_positions(&record.session.messages);
        record
            .pending_acp_approvals
            .insert("acp-head".to_owned(), acp_pending("acp-head-request"));
        record
            .pending_acp_approvals
            .insert("acp-later".to_owned(), acp_pending("acp-later-request"));
        record
            .pending_acp_approval_order
            .extend(["acp-head".to_owned(), "acp-later".to_owned()]);
        record.pending_codex_approvals.insert(
            "codex-newer".to_owned(),
            CodexPendingApproval {
                kind: CodexApprovalKind::CommandExecution,
                request_id: json!("codex-newer-request"),
            },
        );

        assert_eq!(
            registered_pending_approval_message_id(record).as_deref(),
            Some("codex-newer"),
            "newer Codex approval should beat the older ACP FIFO head"
        );

        record.session.messages = vec![
            approval_message("codex-newer"),
            approval_message("acp-later"),
            approval_message("acp-head"),
        ];
        record.message_positions = build_message_positions(&record.session.messages);
        assert_eq!(
            registered_pending_approval_message_id(record).as_deref(),
            Some("acp-head"),
            "newer ACP FIFO head should beat other protocol candidates"
        );

        record.session.messages.remove(2);
        record.message_positions = build_message_positions(&record.session.messages);
        assert_eq!(
            registered_pending_approval_message_id(record).as_deref(),
            Some("acp-later"),
            "a trimmed ACP head should fall back to the newest retained ACP card"
        );
    }

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
                source: None,
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
                source: None,
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
                source: None,
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
    assert_eq!(
        digest.current_status,
        "Delegation child failed after review."
    );
    assert_eq!(digest.source_message_ids, vec![child_message_id]);
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-it", "review-in-termal"]
    );

    state.execute_project_action(&project_id, "fix-it").unwrap();

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

#[test]
fn project_digest_prompt_target_skips_errored_parent_sessions() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-error-target-skip-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Error Target Skip Project");
    let healthy_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let errored_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let errored_index = inner.find_session_index(&errored_session_id).unwrap();
        inner.sessions[errored_index].session.status = SessionStatus::Error;
        state.commit_locked(&mut inner).unwrap();
    }
    let sessions = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .sessions
            .iter()
            .map(project_digest_session_from_record)
            .collect::<Vec<_>>()
    };

    let target = latest_project_prompt_target_session(&sessions)
        .expect("healthy parent session should remain targetable");

    assert_eq!(target.id.as_str(), healthy_session_id.as_str());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_digest_error_actions_skip_latest_errored_parent_target() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!(
        "termal-project-error-action-target-skip-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();

    let project_id = create_test_project(&state, &root, "Error Action Target Skip Project");
    let healthy_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let errored_session_id = create_test_project_session(&state, Agent::Codex, &project_id, &root);
    let (runtime, input_rx) = test_codex_runtime_handle("project-error-target-skip");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let healthy_index = inner.find_session_index(&healthy_session_id).unwrap();
        inner.sessions[healthy_index].runtime = SessionRuntime::Codex(runtime);
        let errored_index = inner.find_session_index(&errored_session_id).unwrap();
        inner.sessions[errored_index].session.status = SessionStatus::Error;
        inner.sessions[errored_index].session.preview =
            "Latest parent failed before retry.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let digest = state.project_digest(&project_id).unwrap();

    assert_eq!(
        digest.primary_session_id.as_deref(),
        Some(healthy_session_id.as_str())
    );
    assert_eq!(
        digest.deep_link.as_deref(),
        Some(format!("/?projectId={project_id}&sessionId={healthy_session_id}").as_str())
    );
    assert_eq!(digest.current_status, "Latest parent failed before retry.");
    assert_eq!(
        digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-it", "review-in-termal"]
    );

    state.execute_project_action(&project_id, "fix-it").unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, healthy_session_id);
            assert_eq!(command.prompt, ProjectActionId::FixIt.prompt().unwrap());
        }
        _ => panic!("expected healthy parent prompt dispatch"),
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
                supported_decisions: None,
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
