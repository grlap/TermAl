use super::*;

fn temp_delegation_state_paths() -> (PathBuf, PathBuf, PathBuf) {
    let unique = Uuid::new_v4();
    let project_root = std::env::temp_dir().join(format!("termal-delegation-root-{unique}"));
    let state_root = std::env::temp_dir().join(format!("termal-delegation-state-{unique}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    (
        project_root,
        state_root.join("sessions.json"),
        state_root.join("orchestrators.json"),
    )
}

fn finish_delegation_child_with_assistant_text(
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
        },
    );
    child.session.status = SessionStatus::Idle;
    child.session.preview = text.lines().last().unwrap_or_default().to_owned();
    state.commit_locked(&mut inner).unwrap();
}

fn next_delegation_delta_event(delta_events: &mut broadcast::Receiver<String>) -> DeltaEvent {
    let payload = delta_events
        .try_recv()
        .expect("expected a delegation delta event");
    serde_json::from_str(&payload).expect("delta event should deserialize")
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
            .uri(format!("/api/delegations/{}", created.delegation.id))
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
            .uri(format!("/api/delegations/{}/result", created.delegation.id))
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
            },
        );
        child.session.status = SessionStatus::Idle;
        child.session.preview = "No issues found.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&created.delegation.id)
        .expect("completed child should yield result");
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
        delegation.result.as_ref().map(|result| result.summary.as_str()),
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

    let parent_message_id = (0..8)
        .find_map(|_| match next_delegation_delta_event(&mut delta_events) {
            DeltaEvent::MessageCreated {
                session_id,
                message_id,
                message,
                session_mutation_stamp,
                ..
            } if session_id == parent_session_id => {
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
                Some(message_id)
            }
            _ => None,
        })
        .expect("parent MessageCreated delta should be published");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReady.",
    );
    let _ = state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let saw_parent_update = (0..8).any(|_| {
        matches!(
            next_delegation_delta_event(&mut delta_events),
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
        )
    });
    assert!(
        saw_parent_update,
        "parent ParallelAgentsUpdate delta should be published"
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
    for _ in 0..3 {
        if let DeltaEvent::MessageCreated {
            session_id,
            message,
            ..
        } = next_delegation_delta_event(&mut delta_events)
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
fn delegation_status_get_does_not_refresh_child_state() {
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
        .get_delegation(&created.delegation.id)
        .expect("status should be readable");

    assert_eq!(response.revision, revision_before_get);
    assert_eq!(response.delegation.status, DelegationStatus::Running);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_idle_child_without_result_packet_fails() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Packet Required".to_owned()),
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
        "Plain assistant response without the required packet.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "child finished without a result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_packet_accepts_preamble_and_case_drift() {
    let parsed = parse_delegation_result_packet(
        "Done, here is the packet:\n\n## result\n\nstatus: completed\n\nsummary:\nReady.",
    )
    .expect("packet with preamble and lowercase labels should parse");
    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Ready.");

    let parsed = parse_delegation_result_packet("## RESULT\n\nSTATUS: failed\n\nSummary:\nNope.")
        .expect("uppercase status label should parse");
    assert_eq!(parsed.status, DelegationStatus::Failed);
    assert_eq!(parsed.summary, "Nope.");
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
        .get_delegation_result(&created.delegation.id)
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
    let response = state
        .cancel_delegation(&created.delegation.id)
        .expect("cancel should preserve completed state");

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
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    let response = state
        .cancel_delegation(&created.delegation.id)
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
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_removed.delegation.child_session_id)
        .expect("child session should remain visible");
    assert_eq!(child.session.parent_delegation_id, None);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_blocks_write_capable_surfaces() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate.".to_owned(),
                title: Some("Read-only Enforcement".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("try rename".to_owned()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("read-only child settings update should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id,
                    "path": "blocked.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("disabled for read-only delegated sessions")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_blocks_project_writes_without_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate project scope.".to_owned(),
                title: Some("Read-only Project Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let blocked_file = project_root.join("blocked.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": project_id,
                    "path": blocked_file.to_string_lossy(),
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("disabled for read-only delegated sessions")
    );
    assert!(!blocked_file.exists());

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn read_only_delegation_rejects_approval_acceptance() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not approve writes.".to_owned(),
                title: Some("Approval Guard".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = match state.update_approval(
        &created.delegation.child_session_id,
        "approval-1",
        ApprovalDecision::Accepted,
    ) {
        Ok(_) => panic!("read-only child approval acceptance should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let err = match state.update_approval(
        &created.delegation.child_session_id,
        "approval-1",
        ApprovalDecision::Rejected,
    ) {
        Ok(_) => panic!("missing pending approval should still fail normally"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_cursor_delegation_uses_plan_mode() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Cursor);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Inspect without editing.".to_owned(),
                title: Some("Cursor Plan Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Cursor),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("cursor delegation should be created");

    assert_eq!(
        created.delegation.write_policy,
        DelegationWritePolicy::ReadOnly
    );
    assert_eq!(created.child_session.cursor_mode, Some(CursorMode::Plan));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_empty_model_uses_agent_default() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the default model.".to_owned(),
                title: Some("Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, Agent::Codex.default_model());
    assert_eq!(
        created.delegation.model.as_deref(),
        Some(Agent::Codex.default_model())
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_write_policy_accepts_legacy_snake_case_discriminators() {
    let read_only: DelegationWritePolicy =
        serde_json::from_value(json!({ "kind": "read_only" })).expect("read_only alias");
    assert_eq!(read_only, DelegationWritePolicy::ReadOnly);

    let shared: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "shared_worktree",
        "ownedPaths": ["src"]
    }))
    .expect("shared_worktree alias");
    assert_eq!(
        shared,
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec!["src".to_owned()],
        }
    );
}

#[test]
fn delegation_prompt_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_prompt = "x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: oversized_prompt,
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_rejects_worker_and_writable_policy() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (worker_status, worker_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try worker","mode":"worker","writePolicy":{"kind":"readOnly"}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(worker_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        worker_body["error"]
            .as_str()
            .unwrap()
            .contains("worker delegations are not implemented")
    );

    let (policy_status, policy_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try write","writePolicy":{"kind":"sharedWorktree","ownedPaths":["src"]}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(policy_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        policy_body["error"]
            .as_str()
            .unwrap()
            .contains("only readOnly delegation write policy")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
