use super::*;

// Schema-derived from ACP v1, not captured from either installed provider:
// https://github.com/agentclientprotocol/agent-client-protocol/blob/
// 6faa3d9f74e94da307ed89f521416a9fb3b99bf9/schema/v1/schema.json
// RequestPermissionRequest -> ToolCallUpdate / PermissionOption; content uses
// ToolCallContent's Content/Diff alternatives and ToolCallLocation. Only the
// request envelope enters the router; fixture provenance is not wire metadata.
fn permission(agent: AcpAgent) -> Value {
    let fixture = match agent {
        AcpAgent::Cursor => include_str!("fixtures/cursor-tool-permission.json"),
        AcpAgent::OpenCode => include_str!("fixtures/opencode-tool-permission.json"),
        _ => panic!("this fixture covers Cursor and OpenCode"),
    };
    serde_json::from_str::<Value>(fixture).unwrap()["request"].clone()
}

fn session(
    agent: AcpAgent,
    mut settings: Value,
) -> (
    AppState,
    String,
    AcpRuntimeHandle,
    mpsc::Receiver<AcpRuntimeCommand>,
) {
    let state = test_app_state();
    settings["agent"] = json!(agent.agent());
    settings["workdir"] = json!(state.default_workdir);
    let created = state
        .create_session(serde_json::from_value(settings).unwrap())
        .unwrap();
    let (runtime, rx) = test_acp_runtime_handle(agent, "schema-permission-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&created.session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    (state, created.session_id, runtime, rx)
}

fn make_read_only(state: &AppState, id: &str) {
    let mut inner = state.inner.lock().unwrap();
    let parent = inner.create_session(
        Agent::Claude,
        None,
        state.default_workdir.clone(),
        None,
        None,
    );
    let parent_id = parent.session.id.clone();
    let index = inner.find_session_index(id).unwrap();
    inner.sessions[index].session.parent_delegation_id = Some("cursor-plan-gate".to_owned());
    inner.delegations.push(DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
        id: "cursor-plan-gate".to_owned(),
        parent_session_id: parent_id,
        child_session_id: id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Inspect".to_owned(),
        prompt: "Inspect without editing".to_owned(),
        cwd: state.default_workdir.clone(),
        agent: Agent::Cursor,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: Some(stamp_now()),
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        base_revision: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 1,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    });
    let index = inner.delegations.len() - 1;
    inner.mark_delegation_mutated(index);
    inner.sync_running_read_only_delegation_index(index);
}

fn route(
    state: &AppState,
    id: &str,
    runtime: &AcpRuntimeHandle,
    agent: AcpAgent,
    request: &Value,
) -> Result<()> {
    route_with_turn_state(
        state,
        id,
        runtime,
        agent,
        request,
        &mut AcpTurnState::default(),
    )
}

fn route_with_turn_state(
    state: &AppState,
    id: &str,
    runtime: &AcpRuntimeHandle,
    agent: AcpAgent,
    request: &Value,
    turn_state: &mut AcpTurnState,
) -> Result<()> {
    handle_acp_message(
        request,
        state,
        id,
        &RuntimeToken::Acp(runtime.runtime_id.clone()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        turn_state,
        &mut SessionRecorder::new(state.clone(), id.to_owned()),
        agent,
    )
}

#[test]
fn cursor_permission_uses_the_mode_at_admission() {
    let (state, id, runtime, rx) = session(AcpAgent::Cursor, json!({"cursorMode":"agent"}));
    let mut request = permission(AcpAgent::Cursor);
    request["params"]["options"]
        .as_array_mut()
        .unwrap()
        .retain(|option| option["kind"] != "reject_once");
    let mut turn_state = AcpTurnState {
        before_cursor_permission_fallback: Some(|state, id| {
            state
                .update_session_settings(
                    id,
                    serde_json::from_value(json!({"cursorMode":"plan"})).unwrap(),
                )
                .unwrap();
        }),
        ..Default::default()
    };

    route_with_turn_state(
        &state,
        &id,
        &runtime,
        AcpAgent::Cursor,
        &request,
        &mut turn_state,
    )
    .unwrap();
    assert_eq!(state.cursor_mode(&id).unwrap(), CursorMode::Plan);
    let reply = response(&rx);
    assert_eq!(reply["id"], request["id"]);
    assert_eq!(
        reply["result"]["outcome"],
        json!({"outcome":"selected","optionId":"cursor-once-71"})
    );
    assert!(system_notices(&state, &id).is_empty());

    request["id"] = json!("next-permission");
    route(&state, &id, &runtime, AcpAgent::Cursor, &request).unwrap();
    let reply = response(&rx);
    assert_eq!(reply["id"], request["id"]);
    assert_eq!(reply["result"]["outcome"], json!({"outcome":"cancelled"}));
    assert!(rx.try_recv().is_err());
    let notices = system_notices(&state, &id);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("Cursor") && notices[0].contains("Plan"));
}

fn response(rx: &mpsc::Receiver<AcpRuntimeCommand>) -> Value {
    match rx
        .try_recv()
        .expect("the permission must be answered immediately")
    {
        AcpRuntimeCommand::JsonRpcMessage(message) => message,
        _ => panic!("expected a permission response"),
    }
}

fn system_notices(state: &AppState, id: &str) -> Vec<String> {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.pending_acp_approval_order.is_empty());
    assert!(!record.session.messages.iter().any(|message| matches!(
        message,
        Message::Approval { .. } | Message::UserInputRequest { .. }
    )));
    record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::System,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn cursor_read_only_plan_refusal_has_a_bounded_visible_reason() {
    for offer_rejection in [true, false] {
        let (state, id, runtime, rx) = session(AcpAgent::Cursor, json!({"cursorMode":"plan"}));
        make_read_only(&state, &id);
        let mut events = state.subscribe_delta_events();
        let before = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&id).unwrap()].mutation_stamp
        };
        let mut request = permission(AcpAgent::Cursor);
        if !offer_rejection {
            request["params"]["options"]
                .as_array_mut()
                .unwrap()
                .retain(|option| option["kind"] != "reject_once");
        }
        route(&state, &id, &runtime, AcpAgent::Cursor, &request).unwrap();
        let reply = response(&rx);
        assert_eq!(reply["id"], request["id"]);
        assert_eq!(
            reply["result"]["outcome"],
            if offer_rejection {
                json!({"outcome":"selected","optionId":"cursor-no-83"})
            } else {
                json!({"outcome":"cancelled"})
            }
        );
        assert!(rx.try_recv().is_err(), "one permission gets one answer");
        let notices = system_notices(&state, &id);
        assert_eq!(
            notices.len(),
            1,
            "a refused operation needs a visible reason"
        );
        assert!(notices[0].contains("Cursor") && notices[0].contains("Plan"));
        assert!(notices[0].contains("writable session"));
        assert!(notices[0].chars().count() < 1600);
        let mut created = 0;
        while let Ok(payload) = events.try_recv() {
            if let DeltaEvent::MessageCreated {
                session_id,
                message:
                    Message::Text {
                        author: Author::System,
                        text,
                        ..
                    },
                session_mutation_stamp: Some(stamp),
                ..
            } = serde_json::from_str(&payload).unwrap()
            {
                if session_id == id {
                    assert_eq!(text, notices[0]);
                    assert!(stamp > before);
                    created += 1;
                }
            }
        }
        assert_eq!(
            created, 1,
            "the reason is published once to session clients"
        );
    }
}

#[test]
fn cursor_plan_failed_handoff_does_not_fabricate_a_notice() {
    let (state, id, runtime, rx) = session(AcpAgent::Cursor, json!({"cursorMode":"plan"}));
    let before = serde_json::to_value(state.snapshot().sessions).unwrap();
    let mut events = state.subscribe_delta_events();
    drop(rx);
    assert!(
        route(
            &state,
            &id,
            &runtime,
            AcpAgent::Cursor,
            &permission(AcpAgent::Cursor)
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(state.snapshot().sessions).unwrap(),
        before
    );
    assert!(events.try_recv().is_err());
    assert!(system_notices(&state, &id).is_empty());
}

fn assert_unavailable_cursor_plan_cancels(stopping: bool) {
    let (state, id, runtime, rx) = session(AcpAgent::Cursor, json!({"cursorMode":"plan"}));
    let (_successor, _successor_rx) = if stopping {
        (runtime.clone(), None)
    } else {
        let (successor, rx) = test_acp_runtime_handle(AcpAgent::Cursor, "successor-runtime");
        (successor, Some(rx))
    };
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        if stopping {
            inner.sessions[index].runtime_stop_in_progress = true;
        } else {
            inner.sessions[index].runtime = SessionRuntime::Acp(_successor);
        }
    }
    let before = serde_json::to_value(state.snapshot().sessions).unwrap();
    let mut events = state.subscribe_delta_events();
    let request = permission(AcpAgent::Cursor);
    route(&state, &id, &runtime, AcpAgent::Cursor, &request).unwrap();
    let reply = response(&rx);
    assert_eq!(reply["id"], request["id"]);
    assert_eq!(reply["result"]["outcome"], json!({"outcome":"cancelled"}));
    assert_eq!(
        serde_json::to_value(state.snapshot().sessions).unwrap(),
        before
    );
    assert!(events.try_recv().is_err());
    assert!(system_notices(&state, &id).is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn cursor_plan_stale_permission_cancels_without_a_notice() {
    assert_unavailable_cursor_plan_cancels(false);
}

#[test]
fn cursor_plan_stop_cancels_without_a_notice() {
    assert_unavailable_cursor_plan_cancels(true);
}

#[test]
fn cursor_and_opencode_schema_permissions_auto_answer_the_opaque_once_id() {
    for (agent, settings, once) in [
        (
            AcpAgent::Cursor,
            json!({"cursorMode":"agent"}),
            "cursor-once-71",
        ),
        (
            AcpAgent::OpenCode,
            json!({"opencodeApprovalMode":"auto-approve"}),
            "opencode-once-16",
        ),
    ] {
        let (state, id, runtime, rx) = session(agent, settings);
        let request = permission(agent);
        route(&state, &id, &runtime, agent, &request).unwrap();
        let reply = response(&rx);
        assert_eq!(reply["id"], request["id"]);
        assert_eq!(
            reply["result"]["outcome"],
            json!({"outcome":"selected","optionId":once})
        );
        assert!(rx.try_recv().is_err());
        assert!(system_notices(&state, &id).is_empty());
    }
}

#[test]
fn cursor_and_opencode_schema_manual_cards_are_readable_and_keep_all_decisions() {
    for (agent, settings, options) in [
        (
            AcpAgent::Cursor,
            json!({"cursorMode":"ask"}),
            ["cursor-once-71", "cursor-always-52", "cursor-no-83"],
        ),
        (
            AcpAgent::OpenCode,
            json!({"opencodeApprovalMode":"ask"}),
            ["opencode-once-16", "opencode-always-25", "opencode-no-38"],
        ),
    ] {
        for (decision, option_id) in [
            (ApprovalDecision::Accepted, options[0]),
            (ApprovalDecision::AcceptedForSession, options[1]),
            (ApprovalDecision::Rejected, options[2]),
        ] {
            let (state, id, runtime, rx) = session(agent, settings.clone());
            let request = permission(agent);
            route(&state, &id, &runtime, agent, &request).unwrap();
            assert!(rx.try_recv().is_err(), "Ask must retain the user's choice");
            let (message_id, detail) = {
                let inner = state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
                assert_eq!(record.session.status, SessionStatus::Approval);
                record
                    .session
                    .messages
                    .iter()
                    .find_map(|message| match message {
                        Message::Approval { id, detail, .. } => Some((id.clone(), detail.clone())),
                        _ => None,
                    })
                    .expect("an actual approval card")
            };
            assert!(detail.contains("approval-demo.txt"));
            assert!(detail.contains("label = old") && detail.contains("label = new"));
            assert!(!detail.contains("\"content\"") && !detail.contains("\"toolCall\""));
            state.update_approval(&id, &message_id, decision).unwrap();
            let reply = response(&rx);
            assert_eq!(reply["id"], request["id"]);
            assert_eq!(
                reply["result"]["outcome"],
                json!({"outcome":"selected","optionId":option_id})
            );
            assert!(rx.try_recv().is_err());
        }
    }
}
