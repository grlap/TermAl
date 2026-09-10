// Owns TermAl's OpenCode approval policy, defaults, persistence, and ACP response contracts.
// Does not own OpenCode model/mode negotiation or other agents' permission policies.
use super::*;

fn create(state: &AppState, extra: Value) -> String {
    let mut request = json!({"agent": "OpenCode", "workdir": "/tmp"});
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    state
        .create_session(serde_json::from_value(request).unwrap())
        .unwrap()
        .session_id
}

fn set_status(state: &AppState, id: &str, status: SessionStatus) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(id).unwrap();
    inner.sessions[index].session.status = status;
}

fn assert_mixed_approval_failure_preserves_policy(provider_rejects: bool) {
    let state = test_app_state();
    let id = create(
        &state,
        json!({"model":"auto", "opencodeApprovalMode":"ask"}),
    );
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "mixed-config-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Acp(runtime);
        record.session.status = SessionStatus::Idle;
        record.session.model_options = vec![SessionModelOption::plain("New model", "provider/new")];
    }
    state
        .set_external_session_id(&id, "mixed-config-session".to_owned())
        .unwrap();
    let writer = if provider_rejects {
        Some(std::thread::spawn(move || {
            // The fixed API rejects before admission. Dropping the fixture's
            // runtime below closes this channel without a timing-based probe.
            match rx.recv() {
                Ok(AcpRuntimeCommand::ApplyOpenCodeConfig {
                    started_tx,
                    proceed_rx,
                    response_tx,
                    ..
                }) => {
                    started_tx.send(()).unwrap();
                    recv_within_guard(&proceed_rx, "config execution should be authorized")
                        .unwrap();
                    response_tx
                        .send(Err("provider rejected model".to_owned()))
                        .unwrap();
                    true
                }
                Err(_) => false,
                Ok(_) => panic!("unexpected runtime command"),
            }
        }))
    } else {
        drop(rx); // Scheduling cannot admit a command on this closed channel.
        None
    };
    let result = state.update_session_settings(
        &id,
        serde_json::from_value(json!({
            "opencodeApprovalMode":"auto-approve", "model":"provider/new"
        }))
        .unwrap(),
    );
    let policy = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::None;
        record.session.opencode_approval_mode
    };
    let admitted = writer.map(|worker| worker.join().unwrap()).unwrap_or(false);
    let persisted = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let persisted_policy = persisted.sessions[persisted.find_session_index(&id).unwrap()]
        .session
        .opencode_approval_mode;
    let error = result
        .err()
        .expect("failed mixed patch must return an error");
    assert_eq!(
        (policy, persisted_policy),
        (
            Some(OpenCodeApprovalMode::Ask),
            Some(OpenCodeApprovalMode::Ask)
        ),
        "failed patch must preserve both in-memory and persisted policy (HTTP {})",
        error.status
    );
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        !admitted,
        "mixed patch must be rejected before provider admission"
    );
}

#[test]
fn opencode_mixed_approval_patch_preserves_policy_with_closed_writer() {
    assert_mixed_approval_failure_preserves_policy(false);
}

#[test]
fn opencode_mixed_approval_patch_preserves_policy_with_rejecting_provider() {
    assert_mixed_approval_failure_preserves_policy(true);
}

#[test]
fn opencode_mixed_approval_patch_rejects_each_config_field_without_mutation() {
    // The contract also covers offline sessions, both policy directions, and
    // a repeated policy value. It must not depend on live provider availability.
    for (initial, requested) in [
        ("ask", "auto-approve"),
        ("auto-approve", "ask"),
        ("ask", "ask"),
    ] {
        for field in ["model", "opencodeEffort", "opencodeMode"] {
            let state = test_app_state();
            let id = create(
                &state,
                json!({"model":"auto", "opencodeApprovalMode":initial}),
            );
            let before = serde_json::to_value(&state.snapshot().sessions[0]).unwrap();
            let persisted_before = load_state(state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            let persisted_before =
                serde_json::to_value(&persisted_before.sessions[0].session).unwrap();
            let mut patch = json!({"name":"must not land", "opencodeApprovalMode":requested});
            patch[field] = json!("auto");
            let error = state
                .update_session_settings(&id, serde_json::from_value(patch).unwrap())
                .err()
                .expect("mixed fields must be rejected");
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{field}");
            assert_eq!(
                serde_json::to_value(&state.snapshot().sessions[0]).unwrap(),
                before,
                "{field}"
            );
            let persisted = load_state(state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(&persisted.sessions[0].session).unwrap(),
                persisted_before,
                "{field}"
            );
        }
    }
}

#[test]
fn opencode_separate_policy_and_provider_patches_remain_supported() {
    let state = test_app_state();
    let id = create(
        &state,
        json!({"model":"auto", "opencodeApprovalMode":"ask"}),
    );
    state
        .update_session_settings(
            &id,
            serde_json::from_value(json!({
                "model":"provider/new", "opencodeEffort":"auto", "opencodeMode":"auto"
            }))
            .unwrap(),
        )
        .unwrap();
    state
        .update_session_settings(
            &id,
            serde_json::from_value(json!({
                "opencodeApprovalMode":"auto-approve", "name":"New policy"
            }))
            .unwrap(),
        )
        .unwrap();
    let persisted = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let session = &persisted.sessions[persisted.find_session_index(&id).unwrap()].session;
    assert_eq!(
        session.opencode_approval_mode,
        Some(OpenCodeApprovalMode::AutoApprove)
    );
    assert_eq!(session.opencode_model.as_deref(), Some("provider/new"));
    assert_eq!(session.opencode_effort.as_deref(), Some("auto"));
    assert_eq!(session.opencode_mode.as_deref(), Some("auto"));
    assert_eq!(session.name, "New policy");
}

#[test]
fn opencode_approval_defaults_overrides_and_sqlite_roundtrip() {
    let state = test_app_state();
    let old = create(&state, json!({}));
    state
        .update_app_settings(
            serde_json::from_value(json!({
                "defaultOpenCodeApprovalMode": "auto-approve"
            }))
            .unwrap(),
        )
        .unwrap();
    let automatic = create(&state, json!({}));
    let explicit = create(&state, json!({"opencodeApprovalMode": "ask"}));
    let inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        inner.preferences.default_opencode_approval_mode,
        OpenCodeApprovalMode::AutoApprove
    );
    for (id, expected) in [
        (&old, OpenCodeApprovalMode::Ask),
        (&automatic, OpenCodeApprovalMode::AutoApprove),
        (&explicit, OpenCodeApprovalMode::Ask),
    ] {
        let record = &inner.sessions[inner.find_session_index(id).unwrap()];
        assert_eq!(record.session.opencode_approval_mode, Some(expected));
        assert_eq!(
            AppState::wire_session_summary_from_record(record).opencode_approval_mode,
            Some(expected)
        );
    }
}

#[test]
fn opencode_approval_legacy_absence_and_invalid_wire_values() {
    let state = test_app_state();
    let id = create(&state, json!({}));
    let inner = state.inner.lock().unwrap();
    let session = &inner.sessions[inner.find_session_index(&id).unwrap()].session;
    let mut value = serde_json::to_value(session).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("opencodeApprovalMode");
    let legacy: Session = serde_json::from_value(value).unwrap();
    assert_eq!(
        legacy.opencode_approval_mode.unwrap_or_default(),
        OpenCodeApprovalMode::Ask
    );
    assert!(
        serde_json::from_value::<CreateSessionRequest>(json!({"opencodeApprovalMode":"yolo"}))
            .is_err()
    );
}

#[test]
fn opencode_approval_updates_reject_other_agents_and_busy_turns() {
    let state = test_app_state();
    let id = create(&state, json!({}));
    let update = || serde_json::from_value(json!({"opencodeApprovalMode":"auto-approve"})).unwrap();
    for status in [
        SessionStatus::Active,
        SessionStatus::Approval,
        SessionStatus::Stopping,
    ] {
        set_status(&state, &id, status);
        assert_eq!(
            state
                .update_session_settings(&id, update())
                .err()
                .unwrap()
                .status,
            StatusCode::CONFLICT
        );
    }
    set_status(&state, &id, SessionStatus::Idle);
    state.update_session_settings(&id, update()).unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.sessions[loaded.find_session_index(&id).unwrap()]
            .session
            .opencode_approval_mode,
        Some(OpenCodeApprovalMode::AutoApprove)
    );
    let codex = state
        .create_session(serde_json::from_value(json!({"agent":"Codex","workdir":"/tmp"})).unwrap())
        .unwrap();
    assert_eq!(
        state
            .update_session_settings(&codex.session_id, update())
            .err()
            .unwrap()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert!(
        state
            .create_session(
                serde_json::from_value(
                    json!({"agent":"Claude","workdir":"/tmp","opencodeApprovalMode":"auto-approve"})
                )
                .unwrap()
            )
            .is_err()
    );
}

fn permission(options: Value) -> Value {
    json!({"jsonrpc":"2.0","id":"permission-opaque","method":"session/request_permission",
        "params":{"toolCall":{"toolCallId":"edit-1","title":"Edit","kind":"edit"},"options":options}})
}

#[test]
fn opencode_permission_is_cancelled_after_stop_claims_an_active_runtime() {
    let state = test_app_state();
    let id = create(&state, json!({"opencodeApprovalMode":"auto-approve"}));
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "stop-permission-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    let stop_gate = install_test_stop_fence_gate(&state, &id);
    let stop_state = state.clone();
    let stop_id = id.clone();
    let stop_thread = std::thread::spawn(move || stop_state.stop_session(&stop_id));
    stop_gate.wait_until_claimed();
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
        assert!(record.runtime_stop_in_progress);
        assert_eq!(record.session.status, SessionStatus::Active);
    }
    let result = handle_acp_message(
        &permission(json!([{"optionId":"once","kind":"allow_once"}])),
        &state,
        &id,
        &RuntimeToken::Acp("stop-permission-runtime".to_owned()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        &mut AcpTurnState::default(),
        &mut SessionRecorder::new(state.clone(), id.clone()),
        AcpAgent::OpenCode,
    );
    let response = rx.try_recv();
    stop_gate.release();
    stop_thread.join().unwrap().unwrap();
    result.unwrap();
    let AcpRuntimeCommand::JsonRpcMessage(response) = response.unwrap() else {
        panic!("permission response expected before Stop proceeds")
    };
    assert_eq!(
        response["result"]["outcome"],
        json!({"outcome":"cancelled"})
    );
}

#[test]
fn opencode_auto_approval_selects_exact_once_and_never_persistent_or_unknown_options() {
    for (mode, kind, should_approve) in [
        ("auto-approve", "allow_once", true),
        ("ask", "allow_once", false),
        ("auto-approve", "allow_always", false),
        ("auto-approve", "Allow_Once", false),
        ("auto-approve", "reject_once", false),
    ] {
        let state = test_app_state();
        let id = create(&state, json!({"opencodeApprovalMode":mode}));
        set_status(&state, &id, SessionStatus::Active);
        let (runtime, rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "permission-runtime");
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&id).unwrap();
            inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
        }
        let mut recorder = SessionRecorder::new(state.clone(), id.clone());
        handle_acp_message(
            &permission(json!([
                {"optionId":"opaque-42","kind":kind,"name":"Allow once"},
                {"optionId":"allow_once","kind":"reject_once","name":"Auto approve"}
            ])),
            &state,
            &id,
            &RuntimeToken::Acp("permission-runtime".to_owned()),
            &Arc::new(Mutex::new(HashMap::new())),
            &Arc::new(Mutex::new(AcpRuntimeState::default())),
            &runtime.input_tx,
            &mut AcpTurnState::default(),
            &mut recorder,
            AcpAgent::OpenCode,
        )
        .unwrap();
        if should_approve {
            let AcpRuntimeCommand::JsonRpcMessage(response) = rx.try_recv().unwrap() else {
                panic!("response expected")
            };
            assert_eq!(response["id"], "permission-opaque");
            assert_eq!(response["result"]["outcome"]["optionId"], "opaque-42");
            assert!(rx.try_recv().is_err());
        } else {
            assert!(rx.try_recv().is_err());
            let inner = state.inner.lock().unwrap();
            assert_eq!(
                inner.sessions[inner.find_session_index(&id).unwrap()]
                    .session
                    .status,
                SessionStatus::Approval
            );
        }
    }
}

#[test]
fn opencode_stale_runtime_permission_is_cancelled_even_with_auto_approval() {
    let state = test_app_state();
    let id = create(&state, json!({"opencodeApprovalMode":"auto-approve"}));
    set_status(&state, &id, SessionStatus::Active);
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "old-runtime");
    let (successor, _successor_rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "new-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(successor);
    }
    let mut recorder = SessionRecorder::new(state.clone(), id.clone());
    handle_acp_message(
        &permission(json!([{"optionId":"once","kind":"allow_once"}])),
        &state,
        &id,
        &RuntimeToken::Acp("old-runtime".to_owned()),
        &Arc::new(Mutex::new(HashMap::new())),
        &Arc::new(Mutex::new(AcpRuntimeState::default())),
        &runtime.input_tx,
        &mut AcpTurnState::default(),
        &mut recorder,
        AcpAgent::OpenCode,
    )
    .unwrap();
    let AcpRuntimeCommand::JsonRpcMessage(response) = rx.try_recv().unwrap() else {
        panic!("response expected")
    };
    assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
}

#[test]
fn opencode_approval_is_session_scoped_and_stopping_never_grants() {
    let state = test_app_state();
    let auto = create(&state, json!({"opencodeApprovalMode":"auto-approve"}));
    let ask = create(&state, json!({}));
    let approval = AcpPendingApproval {
        request_id: json!(1),
        allow_once_option_id: Some("once".to_owned()),
        allow_always_option_id: None,
        reject_option_id: Some("no".to_owned()),
    };
    set_status(&state, &auto, SessionStatus::Active);
    set_status(&state, &ask, SessionStatus::Active);
    assert_eq!(
        acp_permission_response_option_id(AcpAgent::OpenCode, &state, &auto, &approval).unwrap(),
        Some("once".to_owned())
    );
    assert_eq!(
        acp_permission_response_option_id(AcpAgent::OpenCode, &state, &ask, &approval).unwrap(),
        None
    );
    set_status(&state, &auto, SessionStatus::Stopping);
    assert_eq!(
        acp_permission_response_option_id(AcpAgent::OpenCode, &state, &auto, &approval).unwrap(),
        None
    );
}

#[test]
fn opencode_pending_manual_card_suspends_auto_approval() {
    let state = test_app_state();
    let id = create(&state, json!({"opencodeApprovalMode":"auto-approve"}));
    set_status(&state, &id, SessionStatus::Active);
    let (runtime, rx) = test_acp_runtime_handle(AcpAgent::OpenCode, "pending-card-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime.clone());
    }
    let mut recorder = SessionRecorder::new(state.clone(), id.clone());
    for (request_id, kind) in [("first", "allow_always"), ("second", "allow_once")] {
        let mut request = permission(json!([{"optionId":"opaque","kind":kind}]));
        request["id"] = json!(request_id);
        handle_acp_message(
            &request,
            &state,
            &id,
            &RuntimeToken::Acp("pending-card-runtime".to_owned()),
            &Arc::new(Mutex::new(HashMap::new())),
            &Arc::new(Mutex::new(AcpRuntimeState::default())),
            &runtime.input_tx,
            &mut AcpTurnState::default(),
            &mut recorder,
            AcpAgent::OpenCode,
        )
        .unwrap();
    }
    assert!(rx.try_recv().is_err(), "neither request may auto-grant");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&id).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Approval);
    assert_eq!(record.pending_acp_approvals.len(), 2);
}

#[test]
fn opencode_orchestrator_policy_overrides_app_default() {
    let state = test_app_state();
    let id = create(&state, json!({"opencodeApprovalMode":"auto-approve"}));
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    for automatic in [false, true] {
        let template: OrchestratorSessionTemplate = serde_json::from_value(json!({
            "id":"node","name":"OpenCode","agent":"OpenCode","model":"auto",
            "instructions":"","autoApprove":automatic,"inputMode":"queue","position":{"x":0,"y":0}
        }))
        .unwrap();
        apply_orchestrator_template_session_settings(&mut inner.sessions[index], &template);
        assert_eq!(
            inner.sessions[index].session.opencode_approval_mode,
            Some(if automatic {
                OpenCodeApprovalMode::AutoApprove
            } else {
                OpenCodeApprovalMode::Ask
            })
        );
    }
}
