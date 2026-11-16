// Incoming Gemini/Antigravity ACP permission decisions through the real router.
use super::*;

fn session(
    mode: GeminiApprovalMode,
) -> (
    AppState,
    String,
    AcpRuntimeHandle,
    mpsc::Receiver<AcpRuntimeCommand>,
) {
    let state = test_app_state();
    let created = state
        .create_session(
            serde_json::from_value(json!({
                "agent": "Gemini", "workdir": state.default_workdir,
                "geminiApprovalMode": mode
            }))
            .unwrap(),
        )
        .unwrap();
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::Gemini, "agy-permission-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&created.session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    (state, created.session_id, runtime, rx)
}

fn ordinary_permission() -> Value {
    // Captured from agy 1.5.8: one default-mode echo permission was rejected,
    // then the prompt was cancelled. Only session, request and tool ids differ.
    serde_json::from_str(include_str!("../fixtures/agy-tool-permission.json")).unwrap()
}

fn route(state: &AppState, id: &str, runtime: &AcpRuntimeHandle, message: &Value) {
    handle_acp_message(
        message,
        state,
        id,
        &RuntimeToken::Acp(runtime.runtime_id.clone()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        &mut AcpTurnState::default(),
        &mut SessionRecorder::new(state.clone(), id.to_owned()),
        AcpAgent::Gemini,
    )
    .unwrap();
}

fn assert_no_card(state: &AppState, id: &str) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(id).unwrap()];
    assert!(record.pending_acp_approvals.is_empty());
    assert!(record.pending_acp_approval_order.is_empty());
    assert!(!record.session.messages.iter().any(|message| matches!(
        message,
        Message::Approval { .. } | Message::UserInputRequest { .. }
    )));
}

#[test]
fn yolo_answers_an_ordinary_incoming_permission_once_without_a_card() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
    let mut request = ordinary_permission();
    // Display text and opaque identifiers do not choose the grant kind.
    for option in request["params"]["options"].as_array_mut().unwrap() {
        if option["kind"] == "allow_once" {
            option["optionId"] = json!("Reject-looking-opaque-ID");
            option["name"] = json!("Reject");
        }
    }
    route(&state, &id, &runtime, &request);
    let AcpRuntimeCommand::JsonRpcMessage(response) = rx
        .try_recv()
        .expect("Yolo should answer without waiting for a manual card")
    else {
        panic!("JSON-RPC response expected")
    };
    assert_eq!(response["id"], request["id"]);
    assert_eq!(
        response["result"]["outcome"],
        json!({"outcome":"selected", "optionId":"Reject-looking-opaque-ID"})
    );
    assert!(rx.try_recv().is_err(), "one request must have one answer");
    assert_no_card(&state, &id);
    assert_eq!(state.snapshot().sessions[0].status, SessionStatus::Active);
}

#[test]
fn default_plan_and_autoedit_keep_an_incoming_execute_request_manual() {
    for mode in [
        GeminiApprovalMode::Default,
        GeminiApprovalMode::Plan,
        GeminiApprovalMode::AutoEdit,
    ] {
        let (state, id, runtime, rx) = session(mode);
        route(&state, &id, &runtime, &ordinary_permission());
        assert!(rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Approval);
        assert_eq!(record.pending_acp_approvals.len(), 1);
        assert_eq!(record.pending_acp_approval_order.len(), 1);
    }
}

#[test]
fn yolo_does_not_convert_persistent_or_multiple_choices_into_a_grant() {
    for options in [
        json!([{"optionId":"persistent", "kind":"allow_always", "name":"Allow"}, {"optionId":"no", "kind":"reject_once", "name":"Reject"}]),
        json!([{"optionId":"answer-a", "kind":"allow_once", "name":"Answer A"}, {"optionId":"answer-b", "kind":"allow_once", "name":"Answer B"}]),
    ] {
        let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
        let mut request = ordinary_permission();
        request["params"]["options"] = options;
        route(&state, &id, &runtime, &request);
        assert!(rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert_eq!(record.pending_acp_approvals.len(), 1);
        assert_eq!(record.session.status, SessionStatus::Approval);
    }
}

#[test]
fn stopping_runtime_cancels_an_incoming_permission_without_a_card() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime_stop_in_progress = true;
    }
    route(&state, &id, &runtime, &ordinary_permission());
    let AcpRuntimeCommand::JsonRpcMessage(response) =
        rx.try_recv().expect("Stop must settle the permission")
    else {
        panic!("JSON-RPC response expected")
    };
    assert_eq!(
        response["result"]["outcome"],
        json!({"outcome":"cancelled"})
    );
    assert!(rx.try_recv().is_err());
    assert_no_card(&state, &id);
}

#[test]
fn replaced_runtime_cancels_its_old_permission_without_touching_the_successor() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
    let (successor, successor_rx) = test_acp_runtime_handle(AcpAgent::Gemini, "agy-successor");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(successor);
    }
    route(&state, &id, &runtime, &ordinary_permission());
    let AcpRuntimeCommand::JsonRpcMessage(response) =
        rx.try_recv().expect("old request must be cancelled")
    else {
        panic!("JSON-RPC response expected")
    };
    assert_eq!(
        response["result"]["outcome"],
        json!({"outcome":"cancelled"})
    );
    assert!(rx.try_recv().is_err());
    assert!(successor_rx.try_recv().is_err());
    assert_no_card(&state, &id);
    assert_eq!(state.snapshot().sessions[0].status, SessionStatus::Active);
}

fn make_read_only(state: &AppState, id: &str) {
    {
        let mut inner = state.inner.lock().unwrap();
        let parent = inner.create_session(
            Agent::Claude,
            None,
            state.default_workdir.clone(),
            None,
            None,
        );
        let parent_id = parent.session.id.clone();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("delegation-gate".to_owned());
        inner.delegations.push(DelegationRecord {
            original_project_id: None,
            project_basis_recorded: false,
            id: "delegation-gate".to_owned(),
            parent_session_id: parent_id,
            child_session_id: id.to_owned(),
            mode: DelegationMode::Reviewer,
            status: DelegationStatus::Running,
            title: "gate".to_owned(),
            prompt: "/review-code".to_owned(),
            cwd: state.default_workdir.clone(),
            agent: Agent::Gemini,
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
        let delegation_index = inner.delegations.len() - 1;
        inner.mark_delegation_mutated(delegation_index);
        inner.sync_running_read_only_delegation_index(delegation_index);
    }
}

fn system_notices(state: &AppState, id: &str) -> Vec<String> {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(id).unwrap()]
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

fn response(rx: &mpsc::Receiver<AcpRuntimeCommand>) -> Value {
    let AcpRuntimeCommand::JsonRpcMessage(reply) = rx
        .try_recv()
        .expect("a bounded permission answer is required")
    else {
        panic!("JSON-RPC answer expected")
    };
    assert!(rx.try_recv().is_err(), "one request gets one answer");
    reply
}

#[test]
fn selecting_yolo_for_next_turn_cannot_grant_the_current_process_permission() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Default);
    state
        .update_session_settings(
            &id,
            serde_json::from_value(json!({"geminiApprovalMode":"yolo"})).unwrap(),
        )
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.sessions[inner.find_session_index(&id).unwrap()].runtime_reset_required);
    }
    let request = ordinary_permission();
    route(&state, &id, &runtime, &request);
    let reply = response(&rx);
    assert_eq!(reply["id"], request["id"]);
    assert_eq!(
        reply["result"]["outcome"],
        json!({"outcome":"selected","optionId":"deny"})
    );
    assert_no_card(&state, &id);
    let notices = system_notices(&state, &id);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("Yolo") && notices[0].contains("restart"));
    assert!(notices[0].chars().count() < 1600);
}

#[test]
fn read_only_permission_is_refused_with_one_stamped_visible_reason_without_a_card() {
    for offer_rejection in [true, false] {
        let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
        make_read_only(&state, &id);
        let mut deltas = state.subscribe_delta_events();
        let stamp_before = {
            let inner = state.inner.lock().unwrap();
            inner.sessions[inner.find_session_index(&id).unwrap()].mutation_stamp
        };
        let mut request = ordinary_permission();
        if !offer_rejection {
            request["params"]["options"]
                .as_array_mut()
                .unwrap()
                .retain(|option| option["kind"] != "reject_once");
        }
        route(&state, &id, &runtime, &request);
        let reply = response(&rx);
        assert_eq!(reply["id"], request["id"]);
        assert_eq!(
            reply["result"]["outcome"],
            if offer_rejection {
                json!({"outcome":"selected","optionId":"deny"})
            } else {
                json!({"outcome":"cancelled"})
            }
        );
        assert_no_card(&state, &id);
        let notices = system_notices(&state, &id);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("read-only") && notices[0].contains("writable session"));
        assert!(notices[0].chars().count() < 1600);
        let mut created = 0;
        while let Ok(payload) = deltas.try_recv() {
            if let DeltaEvent::MessageCreated {
                session_id,
                message:
                    Message::Text {
                        author: Author::System,
                        text,
                        ..
                    },
                session_mutation_stamp: Some(stamp),
                status,
                ..
            } = serde_json::from_str(&payload).unwrap()
            {
                if session_id == id {
                    assert_eq!(text, notices[0]);
                    assert!(stamp > stamp_before);
                    assert_eq!(status, SessionStatus::Active);
                    created += 1;
                }
            }
        }
        assert_eq!(created, 1, "a refusal must reach clients exactly once");
    }
}

#[test]
fn failed_permission_handoff_does_not_fabricate_a_refusal_notice() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
    make_read_only(&state, &id);
    let before = serde_json::to_value(state.snapshot().sessions).unwrap();
    let mut deltas = state.subscribe_delta_events();
    drop(rx);
    let result = handle_acp_message(
        &ordinary_permission(),
        &state,
        &id,
        &RuntimeToken::Acp(runtime.runtime_id.clone()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        &mut AcpTurnState::default(),
        &mut SessionRecorder::new(state.clone(), id.clone()),
        AcpAgent::Gemini,
    );
    assert!(result.is_err());
    assert_eq!(
        serde_json::to_value(state.snapshot().sessions).unwrap(),
        before
    );
    assert!(deltas.try_recv().is_err());
    assert_no_card(&state, &id);
}

#[test]
fn a_pending_manual_choice_suspends_yolo_automatic_answers() {
    let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
    let mut first = ordinary_permission();
    first["id"] = json!("first-manual");
    first["params"]["options"] = json!([{"kind":"allow_once","optionId":"answer-a","name":"A"},{"kind":"allow_once","optionId":"answer-b","name":"B"}]);
    route(&state, &id, &runtime, &first);
    let mut second = ordinary_permission();
    second["id"] = json!("second-routine");
    route(&state, &id, &runtime, &second);
    assert!(rx.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Approval);
    assert_eq!(record.pending_acp_approval_order.len(), 2);
    let ids: Vec<_> = record
        .pending_acp_approval_order
        .iter()
        .map(|message_id| record.pending_acp_approvals[message_id].request_id.clone())
        .collect();
    assert_eq!(ids, vec![json!("first-manual"), json!("second-routine")]);
}

#[test]
fn invalid_once_metadata_does_not_gain_yolo_authority_from_its_label() {
    for option in [
        json!({"kind":"allow_once","optionId":"","name":"Allow once"}),
        json!({"kind":"allow_once","optionId":7,"name":"Allow once"}),
        json!({"kind":"Allow_Once","optionId":"opaque","name":"Allow once"}),
    ] {
        let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
        let mut request = ordinary_permission();
        request["params"]["options"] = json!([option]);
        route(&state, &id, &runtime, &request);
        assert!(rx.try_recv().is_err());
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert_eq!(record.pending_acp_approvals.len(), 1);
        assert_eq!(record.session.status, SessionStatus::Approval);
    }
}

#[test]
fn stop_or_stale_runtime_does_not_publish_a_current_refusal_reason() {
    for stopped in [false, true] {
        let (state, id, runtime, rx) = session(GeminiApprovalMode::Yolo);
        make_read_only(&state, &id);
        let (new_runtime, _new_rx) = test_acp_runtime_handle(AcpAgent::Gemini, "agy-new-owner");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            if stopped {
                inner.sessions[index].runtime_stop_in_progress = true;
            } else {
                inner.sessions[index].runtime = SessionRuntime::Acp(new_runtime);
            }
        }
        route(&state, &id, &runtime, &ordinary_permission());
        assert_eq!(
            response(&rx)["result"]["outcome"],
            json!({"outcome":"cancelled"})
        );
        assert!(system_notices(&state, &id).is_empty());
        assert_no_card(&state, &id);
    }
}
