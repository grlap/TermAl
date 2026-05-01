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

fn assert_read_only_delegation_error(value: &Value, title: &str) {
    let error = value["error"]
        .as_str()
        .expect("error response should include a message");
    assert!(error.contains("disabled for read-only delegated sessions"));
    assert!(error.contains(&format!("while read-only delegation `{title}` is running")));
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
        .cancel_delegation(&created.delegation.id)
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
                    | DeltaEvent::DelegationCanceled { .. }
            ),
            "terminal cancel should not publish delegation deltas"
        );
    }

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

    finish_delegation_child_with_assistant_text(
        &state,
        &completed.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDone.",
    );
    state
        .refresh_delegation_for_child_session(&completed.delegation.child_session_id)
        .expect("refresh should complete delegation");
    state
        .cancel_delegation(&canceled.delegation.id)
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
                    "sessionId": created.delegation.child_session_id.as_str(),
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
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let ssh_remote = RemoteConfig {
        id: "ssh-read-only-guard".to_owned(),
        name: "SSH Read-only Guard".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &ssh_remote,
        "/remote/read-only-guard",
        "Remote Read-only Guard",
        "remote-read-only-guard",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let proxy_remote = RemoteConfig {
        id: "remote-read-only-bypass".to_owned(),
        name: "Remote Read-only Bypass".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("127.0.0.1".to_owned()),
        port: Some(22),
        user: None,
    };
    let proxy_remote_project_id = create_test_remote_project(
        &state,
        &proxy_remote,
        "/remote/read-only-bypass",
        "Remote Read-only Bypass",
        "remote-project-read-only-bypass",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": proxy_remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked before proxy",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let child_session_id = created.delegation.child_session_id.clone();
    let child_workdir = created.child_session.workdir.clone();
    for (route, body) in [
        (
            "/api/git/file",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "action": "stage",
                "path": "remote-bypass.txt"
            }),
        ),
        (
            "/api/git/commit",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "message": "blocked commit"
            }),
        ),
        (
            "/api/git/push",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
        (
            "/api/git/sync",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
    ] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let review = default_review_document("read-only-review-guard");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/reviews/read-only-review-guard?sessionId={}&projectId={}",
                child_session_id, proxy_remote_project_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&review).expect("review document should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": child_session_id.as_str(),
                        "workdir": child_workdir.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_expired_child_link_uses_fallback_error_wording() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Leave a stale child link.".to_owned(),
                title: Some("Expired Link".to_owned()),
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
        inner
            .delegations
            .retain(|delegation| delegation.id != created.delegation.id);
        state.commit_locked(&mut inner).unwrap();
    }

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "path": "blocked-expired.txt",
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
            .contains("while an expired read-only delegation is still attached")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_delegation_missing_child_session_still_blocks_scope_writes() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-missing-child-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Missing Child Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Keep blocking this scope if the child disappears.".to_owned(),
                title: Some("Missing Child Delegation".to_owned()),
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
        inner.remove_session_at(child_index);
        state.commit_locked(&mut inner).unwrap();
    }

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&project_id),
            None,
            "project writes",
        )
        .expect_err("missing child session should not make the delegation fail open");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Missing Child Delegation` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
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
    assert_read_only_delegation_error(&body, "Read-only Project Scope");
    assert!(!blocked_file.exists());

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("workdir-only writes should be blocked inside read-only delegation scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "projectId": project_id.as_str(),
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn read_only_delegation_blocks_bidirectional_workdir_containment() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-workdir-scope-{}", Uuid::new_v4()));
    let delegated_dir = project_root.join("delegated");
    let nested_write_dir = delegated_dir.join("nested");
    fs::create_dir_all(&nested_write_dir).expect("nested workdir should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Workdir Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate nested project scope.".to_owned(),
                title: Some("Read-only Nested Scope".to_owned()),
                cwd: Some(delegated_dir_label.clone()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let nested_write_dir_label = nested_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(nested_write_dir_label.as_str()),
            "nested workdir write",
        )
        .expect_err("writes below the delegated workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "ancestor workdir write",
        )
        .expect_err("writes from an ancestor workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let sibling_write_dir = project_root.join("sibling");
    fs::create_dir_all(&sibling_write_dir).expect("sibling workdir should exist");
    let sibling_write_dir_label = sibling_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(sibling_write_dir_label.as_str()),
            "sibling workdir write",
        )
        .expect_err("workdir-only writes should inherit the enclosing project root");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn delegation_write_scope_does_not_match_empty_workdir() {
    for workdir in ["", "   \t"] {
        let scope = DelegationWriteScope {
            title: "Empty Workdir".to_owned(),
            project_id: None,
            workdir: workdir.to_owned(),
        };

        assert!(!delegation_write_scope_matches(
            &scope,
            None,
            Some("/tmp/termal-empty-workdir")
        ));
    }
}

#[tokio::test]
async fn read_only_delegation_blocks_git_repo_root_writes_from_sibling_workdir() {
    let state = test_app_state();
    let repo_root =
        std::env::temp_dir().join(format!("termal-read-only-git-root-{}", Uuid::new_v4()));
    let delegated_dir = repo_root.join("delegated");
    let sibling_dir = repo_root.join("sibling");
    fs::create_dir_all(&delegated_dir).expect("delegated workdir should exist");
    fs::create_dir_all(&sibling_dir).expect("sibling workdir should exist");
    init_git_document_test_repo(&repo_root);
    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Git Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate this repository.".to_owned(),
                title: Some("Read-only Git Root".to_owned()),
                cwd: Some(delegated_dir_label),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/git/commit")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "workdir": sibling_dir.to_string_lossy(),
                    "message": "blocked"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Git Root");

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_project_and_workdir_writes_with_parent_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-parent-scope-{}", Uuid::new_v4()));
    let unrelated_root = std::env::temp_dir().join(format!(
        "termal-read-only-unrelated-scope-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Parent Scope");
    let unrelated_project_id =
        create_test_project(&state, &unrelated_root, "Unrelated Project Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let sibling_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate through the parent.".to_owned(),
                title: Some("Read-only Parent Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    for (session_id, name) in [
        (parent_session_id.as_str(), "parent can still rename"),
        (sibling_session_id.as_str(), "sibling can still rename"),
    ] {
        state
            .update_session_settings(
                session_id,
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
            .expect("parent/sibling settings should not inherit project-wide write blocking");
    }
    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("child rename stays blocked".to_owned()),
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
        Ok(_) => panic!("read-only child settings should stay blocked"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            Some(&project_id),
            None,
            "file writes",
        )
        .expect_err("parent session id should not bypass read-only project scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("parent session id should not bypass read-only workdir scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let unrelated_root_label = unrelated_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&sibling_session_id),
            Some(&unrelated_project_id),
            Some(unrelated_root_label.as_str()),
            "mixed-scope file writes",
        )
        .expect_err("explicit project/workdir should not hide the session-derived scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    for (session_id, file_name) in [
        (parent_session_id.as_str(), "parent-session-only.txt"),
        (sibling_session_id.as_str(), "sibling-session-only.txt"),
    ] {
        let blocked_file = project_root.join(file_name);
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/api/file")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": session_id,
                        "path": file_name,
                        "content": "blocked",
                        "overwrite": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Parent Scope");
        assert!(!blocked_file.exists());
    }

    let unrelated_file = unrelated_root.join("mixed-project.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": sibling_session_id,
                    "projectId": unrelated_project_id,
                    "path": "mixed-project.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Parent Scope");
    assert!(!unrelated_file.exists());

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(unrelated_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_overlapping_project_id_only_writes() {
    let state = test_app_state();
    let outer_root =
        std::env::temp_dir().join(format!("termal-read-only-outer-{}", Uuid::new_v4()));
    let inner_root = outer_root.join("inner");
    let outer_sibling_root = outer_root.join("sibling");
    let unrelated_root =
        std::env::temp_dir().join(format!("termal-read-only-disjoint-{}", Uuid::new_v4()));
    fs::create_dir_all(&inner_root).expect("inner project should exist");
    fs::create_dir_all(&outer_sibling_root).expect("outer sibling project should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project should exist");
    let outer_project_id = create_test_project(&state, &outer_root, "Read-only Outer");
    let inner_project_id = create_test_project(&state, &inner_root, "Read-only Inner");
    let unrelated_project_id = create_test_project(&state, &unrelated_root, "Unrelated Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &inner_project_id, &inner_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate overlapping project scope.".to_owned(),
                title: Some("Read-only Overlap".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            None,
            "overlapping project writes",
        )
        .expect_err("outer project id should block writes into nested delegated project");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let outer_sibling_root_label = outer_sibling_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            Some(outer_sibling_root_label.as_str()),
            "mixed project/workdir writes",
        )
        .expect_err("outer project root should stay checked when workdir is also supplied");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&unrelated_project_id),
            None,
            "unrelated project writes",
        )
        .expect("unrelated project id should not be blocked");

    let app = app_router(state.clone());
    let blocked_file = inner_root.join("blocked-overlap.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": outer_project_id,
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
    assert_read_only_delegation_error(&body, "Read-only Overlap");
    assert!(!blocked_file.exists());

    let allowed_file = unrelated_root.join("allowed.txt");
    let (status, _body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": unrelated_project_id,
                    "path": allowed_file.to_string_lossy(),
                    "content": "allowed",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fs::read_to_string(&allowed_file).expect("allowed write should persist"),
        "allowed"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(outer_root);
    let _ = fs::remove_dir_all(unrelated_root);
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

    for decision in [
        ApprovalDecision::Accepted,
        ApprovalDecision::AcceptedForSession,
    ] {
        let err = match state.update_approval(
            &created.delegation.child_session_id,
            "approval-1",
            decision,
        ) {
            Ok(_) => panic!("read-only child approval acceptance should be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

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

    let isolated: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "isolated_worktree",
        "ownedPaths": ["src"],
        "worktreePath": "C:/tmp/delegation-worktree"
    }))
    .expect("isolated_worktree alias");
    assert_eq!(
        isolated,
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec!["src".to_owned()],
            worktree_path: "C:/tmp/delegation-worktree".to_owned(),
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

#[test]
fn delegation_whitespace_prompt_is_rejected() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "   \t\n".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("whitespace-only prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("cannot be empty"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_rejects_remote_proxy_parent_without_local_project() {
    for (remote_id, remote_session_id) in [
        (Some("ssh-review"), Some("remote-parent-1")),
        (Some("ssh-review"), None),
        (None, Some("remote-parent-1")),
    ] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let parent_index = inner
                .find_visible_session_index(&parent_session_id)
                .expect("parent session should exist");
            let parent = inner
                .session_mut_by_index(parent_index)
                .expect("parent session index should be valid");
            parent.session.project_id = None;
            parent.remote_id = remote_id.map(str::to_owned);
            parent.remote_session_id = remote_session_id.map(str::to_owned);
        }

        let app = app_router(state.clone());
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{parent_session_id}/delegations"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"prompt":"review remote parent","writePolicy":{"kind":"readOnly"}}"#,
                ))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("delegations for remote-backed sessions are not implemented")
        );
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.delegations.is_empty());
        assert_eq!(inner.sessions.len(), 1);
        drop(inner);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
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
