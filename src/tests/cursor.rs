// cursor-agent is an ACP-protocol CLI (same JSON-RPC bridge as Claude Code
// and Gemini CLI) that ships three distinct "cursor modes" which change how
// TermAl handles incoming tool permission requests:
//   - agent: auto-approve every tool permission (allow-once)
//   - ask:   queue each permission as a pending approval card for the user
//   - plan:  refuse all tool permissions (reject-once) for plan-only runs
//
// TermAl keeps cursor_mode in sync with the live agent from three directions:
// ACP config_update messages, standalone mode_update notifications, and
// user-driven settings changes that must be forwarded into any active ACP
// session so switching agent->ask does not leave the next tool call silently
// auto-approved. `matching_acp_config_option_value` resolves ACP option
// values by both `name` and `label` because config payloads are loosely typed.
//
// Production surfaces: `handle_acp_session_permission_request` /
// `handle_cursor_permission_request`, `sync_cursor_mode_from_acp_config`,
// `update_live_cursor_mode_on_active_sessions` (src/runtime.rs, src/state.rs).

use super::*;

// pins that `sync_session_model_options` stores the full option list and
// applies the selected model value onto the Cursor session record.
// guards against drift where ACP-advertised model choices would be silently
// dropped or a new selection would fail to update `session.model`.
#[test]
fn syncs_cursor_model_options_from_acp_config() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor ACP".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let model_options = vec![
        SessionModelOption::plain("Auto", "auto"),
        SessionModelOption::plain("GPT-5.3 Codex", "gpt-5.3-codex"),
    ];
    state
        .sync_session_model_options(
            &created.session_id,
            Some("gpt-5.3-codex".to_owned()),
            model_options.clone(),
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let session = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .map(|record| &record.session)
        .expect("Cursor session should exist");
    assert_eq!(session.model, "gpt-5.3-codex");
    assert_eq!(session.model_options, model_options);
}

// pins that Cursor in agent mode auto-approves every ACP permission request
// with `allow-once`, emitting no pending approval card and leaving the
// session Idle. guards against a regression where agent mode would either
// prompt the user or stall tool execution waiting for a decision.
#[test]
fn cursor_agent_mode_auto_approves_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Agent".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-agent-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor agent mode should auto-respond")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-agent-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("allow-once"))
            );
        }
        _ => panic!("expected automatic Cursor approval response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

// pins that Cursor in ask mode does NOT auto-respond: the request is queued
// in `pending_acp_approvals`, surfaced as a Pending approval message, and
// the session flips to Approval status awaiting the user. guards against
// ask mode silently approving or rejecting without user interaction.
#[test]
fn cursor_ask_mode_queues_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Ask".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-ask-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    assert!(matches!(
        input_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.pending_acp_approvals.len(), 1);
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Approval {
            title,
            command,
            decision,
            ..
        }) if title == "Cursor needs approval"
            && command == "Edit src/main.rs"
            && *decision == ApprovalDecision::Pending
    ));
    assert_eq!(record.session.status, SessionStatus::Approval);
}

// pins that Cursor in plan mode auto-rejects every ACP permission request
// with `reject-once`, never posting an approval card, and returns to Idle.
// guards against plan-only runs being able to execute tools or stall on an
// approval card the user never meant to see.
#[test]
fn cursor_plan_mode_rejects_acp_permission_requests() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Plan".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Plan),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());

    handle_acp_request(
        &cursor_permission_request("cursor-plan-approval"),
        &state,
        &created.session_id,
        &input_tx,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor plan mode should auto-reject")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(message.get("id"), Some(&json!("cursor-plan-approval")));
            assert_eq!(
                message.pointer("/result/outcome/optionId"),
                Some(&json!("reject-once"))
            );
        }
        _ => panic!("expected automatic Cursor rejection response"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.status, SessionStatus::Idle);
}

// pins that an ACP `config_update` carrying a `mode` option with
// `currentValue: "ask"` flips the session's cursor_mode to Ask.
// guards against config-driven mode changes (e.g. user toggling in the
// Cursor UI) failing to propagate into TermAl's session record.
#[test]
fn syncs_cursor_mode_from_acp_config_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Config Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "config_update",
            "configOptions": [
                {
                    "id": "model",
                    "currentValue": "auto",
                    "options": [{ "value": "auto", "name": "Auto" }]
                },
                {
                    "id": "mode",
                    "currentValue": "ask",
                    "options": [
                        { "value": "agent" },
                        { "value": "ask" },
                        { "value": "plan" }
                    ]
                }
            ]
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Ask));
}

// pins the standalone `mode_update` notification path: a bare `mode: "plan"`
// update rewrites cursor_mode without a full config_update envelope.
// guards against the two ACP mode-change shapes diverging and leaving one
// path unable to transition the session between modes.
#[test]
fn syncs_cursor_mode_from_mode_updates() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Mode Sync".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let mut recorder = SessionRecorder::new(state.clone(), created.session_id.clone());
    let mut turn_state = AcpTurnState::default();

    handle_acp_session_update(
        &json!({
            "sessionUpdate": "mode_update",
            "mode": "plan"
        }),
        &state,
        &created.session_id,
        &mut turn_state,
        &mut recorder,
        AcpAgent::Cursor,
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.session_id)
        .expect("Cursor session should exist");
    assert_eq!(record.session.cursor_mode, Some(CursorMode::Plan));
}

// pins that `BorrowedSessionRecorder` routes text, streaming deltas, command
// lifecycle events, and Codex user-input requests through the same message
// and pending-queue paths as the owned `SessionRecorder`.
// guards against the borrowed variant silently skipping recorder side effects.
#[test]
fn borrowed_session_recorder_uses_shared_message_and_request_logic() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let questions = vec![UserInputQuestion {
        header: "Scope".to_owned(),
        id: "scope".to_owned(),
        is_other: false,
        is_secret: false,
        options: None,
        question: "What should Codex review?".to_owned(),
    }];
    let mut recorder_state = SessionRecorderState::default();
    let mut recorder = BorrowedSessionRecorder::new(&state, &session_id, &mut recorder_state);

    recorder.push_text("Initial text").unwrap();
    recorder.text_delta("streamed text").unwrap();
    recorder.finish_streaming_text().unwrap();
    recorder.command_started("cmd-1", "pwd").unwrap();
    recorder
        .command_completed("cmd-1", "pwd", "/tmp", CommandStatus::Success)
        .unwrap();
    recorder
        .push_codex_user_input_request(
            "Need input",
            "Choose the review scope.",
            questions.clone(),
            CodexPendingUserInput {
                questions: questions.clone(),
                request_id: json!("request-1"),
            },
        )
        .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");

    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "Initial text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "streamed text")
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::Command {
                command,
                output,
                status,
                ..
            } if command == "pwd" && output == "/tmp" && *status == CommandStatus::Success
        )
    }));
    assert!(record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::UserInputRequest {
                title,
                detail,
                questions: message_questions,
                state,
                ..
            } if title == "Need input"
                && detail == "Choose the review scope."
                && message_questions == &questions
                && *state == InteractionRequestState::Pending
        )
    }));
    assert_eq!(record.pending_codex_user_inputs.len(), 1);
}

// pins that a user-driven cursor_mode change on a session with a live ACP
// runtime forwards a `session/set_config_option` JSON-RPC request to the
// agent so the next tool call obeys the new mode. guards against the stale
// in-flight agent continuing to auto-approve after the user flipped to ask.
#[test]
fn updates_live_cursor_mode_on_active_acp_sessions() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Live Mode".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Agent),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let (runtime, input_rx) = test_acp_runtime_handle(AcpAgent::Cursor, "cursor-live-mode");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        inner.sessions[index].external_session_id = Some("cursor-session-1".to_owned());
    }

    let updated = state
        .update_session_settings(
            &created.session_id,
            UpdateSessionSettingsRequest {
                name: None,
                model: None,
                sandbox_mode: None,
                approval_policy: None,
                reasoning_effort: None,
                cursor_mode: Some(CursorMode::Ask),
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
        )
        .unwrap();

    let session = updated
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Cursor session should be present");
    assert_eq!(session.cursor_mode, Some(CursorMode::Ask));

    match input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor mode change should be forwarded to the live ACP session")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => {
            assert_eq!(
                message.get("method").and_then(Value::as_str),
                Some("session/set_config_option")
            );
            assert_eq!(
                message.pointer("/params/sessionId"),
                Some(&json!("cursor-session-1"))
            );
            assert_eq!(message.pointer("/params/optionId"), Some(&json!("mode")));
            assert_eq!(message.pointer("/params/value"), Some(&json!("ask")));
        }
        _ => panic!("expected live Cursor mode update request"),
    }
}

// pins that `matching_acp_config_option_value` resolves an option's `value`
// from either the `name` field or the `label` field, since ACP config
// payloads are loosely typed across agents, and returns None on a miss.
// guards against mode/model lookups silently failing when the agent uses a
// different display-string key.
#[test]
fn matches_acp_model_options_by_name_or_label() {
    let config = json!({
        "configOptions": [
            {
                "id": "model",
                "options": [
                    {
                        "value": "auto",
                        "name": "Auto"
                    },
                    {
                        "value": "gpt-5.3-codex-high-fast",
                        "label": "GPT-5.3 Codex High Fast"
                    }
                ]
            }
        ]
    });

    assert_eq!(
        matching_acp_config_option_value(&config, "model", "Auto"),
        Some("auto".to_owned())
    );
    assert_eq!(
        matching_acp_config_option_value(&config, "model", "GPT-5.3 Codex High Fast"),
        Some("gpt-5.3-codex-high-fast".to_owned())
    );
    assert_eq!(
        matching_acp_config_option_value(&config, "model", "Missing Model"),
        None
    );
}
