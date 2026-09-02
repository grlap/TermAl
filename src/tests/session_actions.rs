// Mutable session-action HTTP route tests.
//
// Owns settings updates, queued prompts, approval decisions, structured
// user input, MCP elicitation, and Codex app-request submission routes.
// Does not own review-document persistence or protocol classifiers.
// Split from: src/tests/review.rs.

use super::*;
#[tokio::test]
async fn update_session_settings_route_updates_session_name() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Updated Session"
    }))
    .expect("settings route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/settings"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.name, "Route Updated Session");
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that send message route accepts and queues prompt for busy session.
#[tokio::test]
async fn send_message_route_accepts_and_queues_prompt_for_busy_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Queued route prompt",
        "expandedText": "Expanded queued route prompt"
    }))
    .expect("message route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/messages"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("queued session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "Queued route prompt");
    assert_eq!(
        session.pending_prompts[0].expanded_text.as_deref(),
        Some("Expanded queued route prompt")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that submit approval route updates Claude session and delivers runtime response.
#[tokio::test]
async fn submit_approval_route_updates_claude_session_and_delivers_runtime_response() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-approval-route");
    let message_id = "approval-route-1".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs approval".to_owned(),
                command: "Edit src/main.rs".to_owned(),
                command_language: None,
                detail: "Need to update the route tests.".to_owned(),
                decision: ApprovalDecision::Pending,
                supported_decisions: None,
            },
        )
        .expect("approval message should be recorded");
    state
        .register_claude_pending_approval(
            &session_id,
            message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: Some("acceptEdits".to_owned()),
                request_id: "claude-route-request".to_owned(),
                tool_input: json!({
                    "path": "src/main.rs"
                }),
            },
        )
        .expect("pending Claude approval should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "decision": "acceptedForSession"
    }))
    .expect("approval route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/approvals/{message_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    let expected_preview = approval_preview_text("Claude", ApprovalDecision::AcceptedForSession);
    assert_interaction_response_session_hydrated(
        session,
        SessionStatus::Active,
        &expected_preview,
        1,
        &message_id,
        |message| match message {
            Message::Approval {
                decision: ApprovalDecision::AcceptedForSession,
                ..
            } => {}
            _ => panic!("expected accepted-for-session approval message"),
        },
    );
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::SetPermissionMode(mode)) => {
            assert_eq!(mode, "acceptEdits");
        }
        Ok(_) => panic!("expected Claude permission-mode update"),
        Err(err) => panic!("Claude permission-mode update should arrive: {err}"),
    }
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-route-request");
            assert_eq!(updated_input, json!({ "path": "src/main.rs" }));
        }
        Ok(_) => panic!("expected Claude permission response"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_approvals.is_empty());
    drop(inner);

    assert!(
        state_rx.try_recv().is_err(),
        "approval submission should not publish a full state snapshot"
    );
    let delta: DeltaEvent = serde_json::from_str(
        &delta_rx
            .try_recv()
            .expect("approval submission should publish a message update delta"),
    )
    .expect("approval delta should decode");
    match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id: delta_session_id,
            message_id: delta_message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, response.revision);
            assert_eq!(delta_session_id, session_id);
            assert_eq!(delta_message_id, message_id);
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 1);
            assert_eq!(session_mutation_stamp, session.session_mutation_stamp);
            assert_eq!(preview, expected_preview);
            assert_eq!(status, SessionStatus::Active);
            assert!(matches!(
                message,
                Message::Approval {
                    decision: ApprovalDecision::AcceptedForSession,
                    ..
                }
            ));
        }
        _ => panic!("expected approval submission to publish MessageUpdated"),
    }
    assert!(
        delta_rx.try_recv().is_err(),
        "approval submission should publish exactly one delta"
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn attach_test_codex_runtime(
    state: &AppState,
    session_id: &str,
    runtime_id: &str,
) -> mpsc::Receiver<CodexRuntimeCommand> {
    let (runtime, input_rx) = test_codex_runtime_handle(runtime_id);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("Codex session should exist");
    inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    input_rx
}

fn assert_interaction_response_session_hydrated(
    session: &Session,
    expected_status: SessionStatus,
    expected_preview: &str,
    expected_message_count: u32,
    expected_message_id: &str,
    assert_message: impl FnOnce(&Message),
) {
    assert_eq!(session.status, expected_status);
    assert_eq!(session.preview, expected_preview);
    assert!(session.messages_loaded);
    assert_eq!(session.message_count, expected_message_count);
    assert_eq!(session.messages.len(), expected_message_count as usize);
    let message = &session.messages[0];
    assert_eq!(message.id(), expected_message_id);
    assert_message(message);
}

fn assert_no_state_and_one_message_updated_delta(
    state_rx: &mut broadcast::Receiver<String>,
    delta_rx: &mut broadcast::Receiver<String>,
    session_id: &str,
    message_id: &str,
    expected_revision: u64,
    expected_message_index: usize,
    expected_message_count: u32,
    expected_preview: &str,
    expected_status: SessionStatus,
    expected_session_mutation_stamp: Option<u64>,
) -> Message {
    assert!(
        state_rx.try_recv().is_err(),
        "interaction submission should not publish a full state snapshot"
    );
    let delta: DeltaEvent = serde_json::from_str(
        &delta_rx
            .try_recv()
            .expect("interaction submission should publish MessageUpdated"),
    )
    .expect("interaction delta should decode");
    let message = match delta {
        DeltaEvent::MessageUpdated {
            revision,
            session_id: delta_session_id,
            message_id: delta_message_id,
            message_index,
            message_count,
            message,
            preview,
            status,
            session_mutation_stamp,
        } => {
            assert_eq!(revision, expected_revision);
            assert_eq!(delta_session_id, session_id);
            assert_eq!(delta_message_id, message_id);
            assert_eq!(message_index, expected_message_index);
            assert_eq!(message_count, expected_message_count);
            assert_eq!(preview, expected_preview);
            assert_eq!(status, expected_status);
            assert_eq!(session_mutation_stamp, expected_session_mutation_stamp);
            message
        }
        _ => panic!("expected interaction submission to publish MessageUpdated"),
    };
    assert!(
        delta_rx.try_recv().is_err(),
        "interaction submission should publish exactly one delta"
    );
    message
}

#[tokio::test]
async fn submit_codex_user_input_route_updates_message_and_publishes_message_updated_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let input_rx = attach_test_codex_runtime(&state, &session_id, "codex-user-input-route");
    let message_id = "user-input-route-1".to_owned();
    let questions = vec![UserInputQuestion {
        header: "Choice".to_owned(),
        id: "choice".to_owned(),
        is_other: false,
        is_secret: false,
        multi_select: false,
        options: Some(vec![UserInputQuestionOption {
            description: "Use the recommended option".to_owned(),
            label: "Yes".to_owned(),
        }]),
        question: "Continue?".to_owned(),
    }];
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Need input".to_owned(),
                detail: "Choose one option.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: false,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_codex_pending_user_input(
            &session_id,
            message_id.clone(),
            CodexPendingUserInput {
                questions,
                request_id: json!("user-input-request"),
            },
        )
        .expect("pending user input should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "answers": {
            "choice": ["Yes"]
        }
    }))
    .expect("user input body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/user-input/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    let expected_preview =
        user_input_request_preview_text(session.agent.name(), InteractionRequestState::Submitted);
    assert_interaction_response_session_hydrated(
        session,
        SessionStatus::Active,
        &expected_preview,
        1,
        &message_id,
        |message| match message {
            Message::UserInputRequest {
                state,
                submitted_answers,
                ..
            } => {
                assert_eq!(state, &InteractionRequestState::Submitted);
                assert_eq!(
                    submitted_answers.as_ref(),
                    Some(&BTreeMap::from([(
                        "choice".to_owned(),
                        vec!["Yes".to_owned()],
                    )]))
                );
            }
            _ => panic!("expected submitted user input request message"),
        },
    );

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(CodexRuntimeCommand::JsonRpcResponse { response }) => {
            assert_eq!(response.request_id, json!("user-input-request"));
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({
                    "answers": {
                        "choice": {
                            "answers": ["Yes"]
                        }
                    }
                }))
            );
        }
        Ok(_) => panic!("expected Codex JSON-RPC user input response"),
        Err(err) => panic!("Codex user input response should arrive: {err}"),
    }
    let message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        message,
        Message::UserInputRequest {
            state: InteractionRequestState::Submitted,
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(record.pending_codex_user_inputs.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn claude_user_input_validation_fixture() -> ClaudePendingUserInput {
    ClaudePendingUserInput {
        input: json!({"questions": [], "metadata": {"source": "validation-test"}}),
        questions: vec![
            UserInputQuestion {
                header: "Scope".to_owned(),
                id: "scope".to_owned(),
                is_other: false,
                is_secret: false,
                multi_select: false,
                options: Some(vec![UserInputQuestionOption {
                    description: "Only the changed module".to_owned(),
                    label: "Focused".to_owned(),
                }]),
                question: "Which scope should I use?".to_owned(),
            },
            UserInputQuestion {
                header: "Checks".to_owned(),
                id: "checks".to_owned(),
                is_other: true,
                is_secret: false,
                multi_select: true,
                options: Some(vec![
                    UserInputQuestionOption {
                        description: "Run tests".to_owned(),
                        label: "Tests".to_owned(),
                    },
                    UserInputQuestionOption {
                        description: "Run lint".to_owned(),
                        label: "Lint".to_owned(),
                    },
                ]),
                question: "Which checks should I run?".to_owned(),
            },
            UserInputQuestion {
                header: "Token".to_owned(),
                id: "token".to_owned(),
                is_other: false,
                is_secret: true,
                multi_select: false,
                options: None,
                question: "Which token should I use?".to_owned(),
            },
        ],
        request_id: "claude-validation-request".to_owned(),
    }
}

#[test]
fn validate_claude_user_input_answers_rejects_invalid_answer_shapes() {
    let pending = claude_user_input_validation_fixture();
    let valid = || {
        BTreeMap::from([
            ("scope".to_owned(), vec!["Focused".to_owned()]),
            ("checks".to_owned(), vec!["Tests".to_owned()]),
            ("token".to_owned(), vec!["secret-value".to_owned()]),
        ])
    };

    let mut unknown = valid();
    unknown.insert("unknown".to_owned(), vec!["answer".to_owned()]);
    assert!(
        validate_claude_user_input_answers(&pending, unknown)
            .unwrap_err()
            .message
            .contains("does not match any requested question")
    );

    let mut missing = valid();
    missing.remove("scope");
    assert!(
        validate_claude_user_input_answers(&pending, missing)
            .unwrap_err()
            .message
            .contains("missing an answer")
    );

    let mut multiple = valid();
    multiple.insert(
        "scope".to_owned(),
        vec!["Focused".to_owned(), "Broad".to_owned()],
    );
    assert!(
        validate_claude_user_input_answers(&pending, multiple)
            .unwrap_err()
            .message
            .contains("exactly one answer")
    );

    let mut outside_options = valid();
    outside_options.insert("scope".to_owned(), vec!["Broad".to_owned()]);
    assert!(
        validate_claude_user_input_answers(&pending, outside_options)
            .unwrap_err()
            .message
            .contains("outside the provided options")
    );
}

#[test]
fn validate_claude_user_input_answers_encodes_permission_shape_and_masks_secrets() {
    let answers = || {
        BTreeMap::from([
            ("scope".to_owned(), vec!["Focused".to_owned()]),
            (
                "checks".to_owned(),
                vec!["Tests".to_owned(), "custom smoke check".to_owned()],
            ),
            ("token".to_owned(), vec!["secret-value".to_owned()]),
        ])
    };

    let permission_pending = claude_user_input_validation_fixture();
    let (permission_input, display_answers) =
        validate_claude_user_input_answers(&permission_pending, answers())
            .expect("valid permission Claude answers should be normalized");
    assert_eq!(
        permission_input["answers"]["Which checks should I run?"],
        json!(["Tests", "custom smoke check"])
    );
    assert_eq!(
        permission_input["answers"]["Which token should I use?"],
        "secret-value"
    );
    assert_eq!(
        display_answers["token"],
        vec!["[secret provided]".to_owned()]
    );
}

#[test]
fn validate_claude_user_input_answers_rejects_non_object_pending_input_without_panicking() {
    let mut pending = claude_user_input_validation_fixture();
    pending.input = Value::Null;
    let err = validate_claude_user_input_answers(
        &pending,
        BTreeMap::from([
            ("scope".to_owned(), vec!["Focused".to_owned()]),
            ("checks".to_owned(), vec!["Tests".to_owned()]),
            ("token".to_owned(), vec!["secret-value".to_owned()]),
        ]),
    )
    .expect_err("a non-object pending input must return a typed error");

    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(err.message.contains("not a JSON object"));
}

#[tokio::test]
async fn submit_claude_user_input_route_delivers_all_permission_answers() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-user-input-route");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let message_id = "claude-user-input-route-1".to_owned();
    let questions = vec![
        UserInputQuestion {
            header: "Scope".to_owned(),
            id: "claude-question-1".to_owned(),
            is_other: true,
            is_secret: false,
            multi_select: false,
            options: Some(vec![UserInputQuestionOption {
                description: "Only the changed module".to_owned(),
                label: "Focused".to_owned(),
            }]),
            question: "Which scope should I use?".to_owned(),
        },
        UserInputQuestion {
            header: "Checks".to_owned(),
            id: "claude-question-2".to_owned(),
            is_other: true,
            is_secret: false,
            multi_select: true,
            options: Some(vec![
                UserInputQuestionOption {
                    description: "Run tests".to_owned(),
                    label: "Tests".to_owned(),
                },
                UserInputQuestionOption {
                    description: "Run lint".to_owned(),
                    label: "Lint".to_owned(),
                },
            ]),
            question: "Which checks should I run?".to_owned(),
        },
    ];
    let original_input = json!({
        "questions": [
            {"question": "Which scope should I use?"},
            {"question": "Which checks should I run?"}
        ],
        "metadata": {"source": "test"}
    });
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Answer Claude's 2 questions to continue.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: true,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_claude_pending_user_input(
            &session_id,
            message_id.clone(),
            ClaudePendingUserInput {
                input: original_input.clone(),
                questions,
                request_id: "claude-permission-request-with-other".to_owned(),
            },
        )
        .expect("pending Claude user input should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "answers": {
            "claude-question-1": ["Focused"],
            "claude-question-2": ["Tests", "custom smoke check"]
        }
    }))
    .expect("user input body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/user-input/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Submitted,
            submitted_answers: Some(answers),
            ..
        }) if answers == &BTreeMap::from([
            ("claude-question-1".to_owned(), vec!["Focused".to_owned()]),
            (
                "claude-question-2".to_owned(),
                vec!["Tests".to_owned(), "custom smoke check".to_owned()],
            ),
        ])
    ));

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-permission-request-with-other");
            let mut expected_input = original_input;
            expected_input.as_object_mut().unwrap().insert(
                "answers".to_owned(),
                json!({
                    "Which scope should I use?": "Focused",
                    "Which checks should I run?": ["Tests", "custom smoke check"]
                }),
            );
            assert_eq!(updated_input, expected_input);
        }
        Ok(_) => panic!("expected Claude permission response"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let expected_preview =
        user_input_request_preview_text(session.agent.name(), InteractionRequestState::Submitted);
    let delta_message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        delta_message,
        Message::UserInputRequest {
            state: InteractionRequestState::Submitted,
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn submit_claude_user_input_route_answers_permission_transport_via_allow() {
    // AskUserQuestion questions that arrived as a can_use_tool permission
    // request must be answered through the permission decision: the submit
    // route returns a PermissionResponse::Allow whose updatedInput carries
    // the answers, flips the card to Submitted with one delta, and removes
    // the pending claim.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-user-input-permission");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let message_id = "claude-user-input-permission-1".to_owned();
    let questions = vec![
        UserInputQuestion {
            header: "Scope".to_owned(),
            id: "claude-question-1".to_owned(),
            is_other: true,
            is_secret: false,
            multi_select: false,
            options: Some(vec![UserInputQuestionOption {
                description: "Only the changed module".to_owned(),
                label: "Focused".to_owned(),
            }]),
            question: "Which scope should I use?".to_owned(),
        },
        UserInputQuestion {
            header: "Checks".to_owned(),
            id: "claude-question-2".to_owned(),
            is_other: true,
            is_secret: false,
            multi_select: true,
            options: Some(vec![
                UserInputQuestionOption {
                    description: "Run the test suite".to_owned(),
                    label: "Tests".to_owned(),
                },
                UserInputQuestionOption {
                    description: "Run the linter".to_owned(),
                    label: "Lint".to_owned(),
                },
            ]),
            question: "Which checks should I run?".to_owned(),
        },
    ];
    let original_input = json!({
        "questions": [
            {"question": "Which scope should I use?"},
            {"question": "Which checks should I run?"}
        ]
    });
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Answer Claude's question to continue.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: true,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_claude_pending_user_input(
            &session_id,
            message_id.clone(),
            ClaudePendingUserInput {
                input: original_input.clone(),
                questions,
                request_id: "claude-permission-request".to_owned(),
            },
        )
        .expect("pending Claude user input should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "answers": {
            "claude-question-1": ["Focused"],
            "claude-question-2": ["Tests", "Lint"]
        }
    }))
    .expect("user input body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/user-input/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Submitted,
            submitted_answers: Some(answers),
            ..
        }) if answers == &BTreeMap::from([
            ("claude-question-1".to_owned(), vec!["Focused".to_owned()]),
            (
                "claude-question-2".to_owned(),
                vec!["Tests".to_owned(), "Lint".to_owned()]
            ),
        ])
    ));

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-permission-request");
            // The live-verified permission contract: a label string for
            // single-select, a label array for multi-select, keyed by the
            // exact question text.
            let mut expected_input = original_input;
            expected_input.as_object_mut().unwrap().insert(
                "answers".to_owned(),
                json!({
                    "Which scope should I use?": "Focused",
                    "Which checks should I run?": ["Tests", "Lint"]
                }),
            );
            assert_eq!(updated_input, expected_input);
        }
        Ok(_) => panic!("permission-transport answers must return through the allow decision"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let expected_preview =
        user_input_request_preview_text(session.agent.name(), InteractionRequestState::Submitted);
    let delta_message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        delta_message,
        Message::UserInputRequest {
            state: InteractionRequestState::Submitted,
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn submit_claude_user_input_route_decline_permission_transport_sends_deny() {
    // Skipping a permission-transport question card must resolve the pending
    // permission with a deny that tells Claude to decide on its own, flip
    // the card to Declined with no recorded answers, and drop the claim.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-user-input-decline");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let message_id = "claude-user-input-decline-1".to_owned();
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "claude-question-1".to_owned(),
        is_other: true,
        is_secret: false,
        multi_select: false,
        options: None,
        question: "Which scope should I use?".to_owned(),
    }];
    state
        .push_message(
            &session_id,
            Message::UserInputRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Answer Claude's question to continue.".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                declinable: true,
                submitted_answers: None,
            },
        )
        .expect("user input request should be recorded");
    state
        .register_claude_pending_user_input(
            &session_id,
            message_id.clone(),
            ClaudePendingUserInput {
                input: json!({ "questions": [{ "question": "Which scope should I use?" }] }),
                questions,
                request_id: "claude-decline-request".to_owned(),
            },
        )
        .expect("pending Claude user input should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    // `answers` is deliberately omitted: a bare {"declined": true} is a
    // valid empty-answer decline (serde default fills the map).
    let body =
        serde_json::to_vec(&json!({ "declined": true })).expect("decline body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/user-input/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    // The transcript must durably distinguish a user Skip from an
    // agent-side cancel: the declined card's detail records the choice.
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Declined,
            submitted_answers: None,
            detail,
            ..
        }) if detail == "The user skipped these questions; Claude was asked to decide on its own."
    ));

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Deny {
            request_id,
            message,
        })) => {
            assert_eq!(request_id, "claude-decline-request");
            assert!(message.contains("declined to answer"));
            assert!(message.contains("best judgment"));
        }
        Ok(_) => panic!("a declined permission-transport question must deny the permission"),
        Err(err) => panic!("Claude permission deny should arrive: {err}"),
    }
    let expected_preview =
        user_input_request_preview_text(session.agent.name(), InteractionRequestState::Declined);
    let delta_message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        delta_message,
        Message::UserInputRequest {
            state: InteractionRequestState::Declined,
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_user_inputs.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn submit_codex_user_input_decline_is_rejected() {
    // Codex's request_user_input protocol has no decline response; the
    // route must refuse instead of inventing one.
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Approval;
    }
    let error =
        match state.submit_user_input(&session_id, "codex-input-message", BTreeMap::new(), true) {
            Ok(_) => panic!("Codex user input must reject a decline"),
            Err(error) => error,
        };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("Codex"));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn register_single_question_claude_input(
    state: &AppState,
    session_id: &str,
    runtime_id: &str,
    message_id: &str,
) -> std::sync::mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input_rx) = test_claude_runtime_handle(runtime_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let question = UserInputQuestion {
        header: "Scope".to_owned(),
        id: "scope".to_owned(),
        is_other: false,
        is_secret: false,
        multi_select: false,
        options: Some(vec![UserInputQuestionOption {
            description: "Use the focused scope".to_owned(),
            label: "Focused".to_owned(),
        }]),
        question: "Which scope?".to_owned(),
    };
    state
        .push_message(
            session_id,
            Message::UserInputRequest {
                id: message_id.to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs your input".to_owned(),
                detail: "Choose a scope.".to_owned(),
                questions: vec![question.clone()],
                state: InteractionRequestState::Pending,
                declinable: true,
                submitted_answers: None,
            },
        )
        .unwrap();
    state
        .register_claude_pending_user_input(
            session_id,
            message_id.to_owned(),
            ClaudePendingUserInput {
                input: json!({ "questions": [{ "question": "Which scope?" }] }),
                questions: vec![question],
                request_id: format!("request-{message_id}"),
            },
        )
        .unwrap();
    input_rx
}

#[test]
fn concurrent_claude_user_input_submissions_deliver_exactly_one_runtime_response() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = "claude-concurrent-input-message".to_owned();
    let input_rx = register_single_question_claude_input(
        &state,
        &session_id,
        "claude-concurrent-input",
        &message_id,
    );
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let answers = BTreeMap::from([("scope".to_owned(), vec!["Focused".to_owned()])]);

    let results = std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..2 {
            let state = state.clone();
            let barrier = Arc::clone(&barrier);
            let session_id = session_id.clone();
            let message_id = message_id.clone();
            let answers = answers.clone();
            workers.push(scope.spawn(move || {
                barrier.wait();
                state.submit_user_input(&session_id, &message_id, answers, false)
            }));
        }
        barrier.wait();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("submission worker should not panic"))
            .collect::<Vec<_>>()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(50)),
        Ok(ClaudeRuntimeCommand::PermissionResponse(
            ClaudePermissionDecision::Allow { .. }
        ))
    ));
    assert!(
        input_rx.try_recv().is_err(),
        "the runtime must receive exactly one response for one pending request"
    );
}

#[test]
fn failed_claude_user_input_delivery_restores_the_pending_claim() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = "claude-failed-input-message".to_owned();
    let input_rx = register_single_question_claude_input(
        &state,
        &session_id,
        "claude-failed-input",
        &message_id,
    );
    drop(input_rx);

    let error = match state.submit_user_input(
        &session_id,
        &message_id,
        BTreeMap::from([("scope".to_owned(), vec!["Focused".to_owned()])]),
        false,
    ) {
        Ok(_) => panic!("closed runtime channel should reject the response"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .unwrap();
    assert!(record.pending_claude_user_inputs.contains_key(&message_id));
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::UserInputRequest {
            state: InteractionRequestState::Pending,
            ..
        })
    ));
}

#[tokio::test]
async fn submit_codex_mcp_elicitation_route_updates_message_and_publishes_message_updated_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let input_rx = attach_test_codex_runtime(&state, &session_id, "codex-mcp-route");
    let message_id = "mcp-route-1".to_owned();
    let request = McpElicitationRequestPayload {
        thread_id: "thread-1".to_owned(),
        turn_id: Some("turn-1".to_owned()),
        server_name: "docs-server".to_owned(),
        mode: McpElicitationRequestMode::Url {
            meta: None,
            elicitation_id: "elicitation-1".to_owned(),
            message: "Open documentation?".to_owned(),
            url: "https://example.test/docs".to_owned(),
        },
    };
    state
        .push_message(
            &session_id,
            Message::McpElicitationRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "MCP request".to_owned(),
                detail: "Open documentation?".to_owned(),
                request: request.clone(),
                state: InteractionRequestState::Pending,
                submitted_action: None,
                submitted_content: None,
            },
        )
        .expect("MCP elicitation request should be recorded");
    state
        .register_codex_pending_mcp_elicitation(
            &session_id,
            message_id.clone(),
            CodexPendingMcpElicitation {
                request: request.clone(),
                request_id: json!("mcp-request"),
            },
        )
        .expect("pending MCP elicitation should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "action": "decline"
    }))
    .expect("MCP elicitation body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/mcp-elicitation/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    let expected_preview = mcp_elicitation_request_preview_text(
        session.agent.name(),
        InteractionRequestState::Submitted,
        Some(McpElicitationAction::Decline),
    );
    assert_interaction_response_session_hydrated(
        session,
        SessionStatus::Active,
        &expected_preview,
        1,
        &message_id,
        |message| match message {
            Message::McpElicitationRequest {
                state,
                submitted_action,
                submitted_content,
                ..
            } => {
                assert_eq!(state, &InteractionRequestState::Submitted);
                assert_eq!(
                    submitted_action.as_ref(),
                    Some(&McpElicitationAction::Decline)
                );
                assert!(submitted_content.is_none());
            }
            _ => panic!("expected submitted MCP elicitation request message"),
        },
    );

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(CodexRuntimeCommand::JsonRpcResponse { response }) => {
            assert_eq!(response.request_id, json!("mcp-request"));
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({
                    "action": "decline",
                    "content": null
                }))
            );
        }
        Ok(_) => panic!("expected Codex JSON-RPC MCP elicitation response"),
        Err(err) => panic!("Codex MCP elicitation response should arrive: {err}"),
    }
    let message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        message,
        Message::McpElicitationRequest {
            state: InteractionRequestState::Submitted,
            submitted_action: Some(McpElicitationAction::Decline),
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(record.pending_codex_mcp_elicitations.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn submit_codex_app_request_route_updates_message_and_publishes_message_updated_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let input_rx = attach_test_codex_runtime(&state, &session_id, "codex-app-request-route");
    let message_id = "app-request-route-1".to_owned();
    let result = json!({ "ok": true, "value": 42 });
    state
        .push_message(
            &session_id,
            Message::CodexAppRequest {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "App request".to_owned(),
                detail: "Generic app request".to_owned(),
                method: "termal/test".to_owned(),
                params: json!({ "question": true }),
                state: InteractionRequestState::Pending,
                submitted_result: None,
            },
        )
        .expect("Codex app request should be recorded");
    state
        .register_codex_pending_app_request(
            &session_id,
            message_id.clone(),
            CodexPendingAppRequest {
                request_id: json!("app-request"),
            },
        )
        .expect("pending app request should be registered");
    let mut state_rx = state.subscribe_events();
    let mut delta_rx = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "result": result
    }))
    .expect("app request body should serialize");

    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/codex/requests/{message_id}"
            ))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    let expected_preview =
        codex_app_request_preview_text(session.agent.name(), InteractionRequestState::Submitted);
    assert_interaction_response_session_hydrated(
        session,
        SessionStatus::Active,
        &expected_preview,
        1,
        &message_id,
        |message| match message {
            Message::CodexAppRequest {
                state,
                submitted_result,
                ..
            } => {
                assert_eq!(state, &InteractionRequestState::Submitted);
                assert_eq!(submitted_result.as_ref(), Some(&result));
            }
            _ => panic!("expected submitted Codex app request message"),
        },
    );

    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(CodexRuntimeCommand::JsonRpcResponse { response }) => {
            assert_eq!(response.request_id, json!("app-request"));
            assert_eq!(
                response.payload,
                CodexJsonRpcResponsePayload::Result(json!({ "ok": true, "value": 42 }))
            );
        }
        Ok(_) => panic!("expected Codex JSON-RPC app request response"),
        Err(err) => panic!("Codex app request response should arrive: {err}"),
    }
    let message = assert_no_state_and_one_message_updated_delta(
        &mut state_rx,
        &mut delta_rx,
        &session_id,
        &message_id,
        response.revision,
        0,
        1,
        &expected_preview,
        SessionStatus::Active,
        session.session_mutation_stamp,
    );
    assert!(matches!(
        message,
        Message::CodexAppRequest {
            state: InteractionRequestState::Submitted,
            submitted_result: Some(_),
            ..
        }
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(record.pending_codex_app_requests.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
