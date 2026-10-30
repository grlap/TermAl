// Owns isolated project containment regressions through the real settings,
// persistence and dispatch paths. Does not exercise live authority or stores;
// only producer responses are scripted, using the adapter's existing fixture.
use super::*;

#[path = "engram_session_obligations.rs"]
mod obligations;

#[path = "engram_project_containment_recovery.rs"]
mod recovery;

#[path = "engram_project_containment_boundaries.rs"]
mod boundaries;

#[path = "engram_project_containment_admission.rs"]
mod admission;

#[path = "engram_project_containment_queue.rs"]
mod queue;

#[path = "engram_project_containment_checkpoint.rs"]
mod checkpoint;

fn fixture() -> (
    AppState,
    String,
    String,
    PathBuf,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, receiver) = test_app_state_with_delegation_codex_runtime("containment-runtime");
    select_scripted_engram_budget_clock_before_enable(&state);
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("containment-project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join(".engram-project"), "fixture-ready\n").unwrap();
    let store = materialize_fixture_engram_store(&root, &root);
    let project = create_test_project(&state, &root, "Project containment");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let mut settings = real_fixture_engram_settings(&root);
        settings.authority_store_key = Some(store);
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram = Some(settings);
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Error;
        record.runtime = SessionRuntime::None;
        record.engram.routing_token = Some("retained-token".to_owned());
        record.engram.active_grant_id = Some("retained-grant".to_owned());
        record.engram.begins_recorded = true;
        state.commit_locked(&mut inner).unwrap();
    }
    (state, project, session, root, receiver)
}

fn settings(state: &AppState, project: &str, gated: bool) -> EngramProjectSettings {
    let inner = state.inner.lock().unwrap();
    let mut settings = inner.find_project(project).unwrap().engram.clone().unwrap();
    settings.turn_gated_control = gated;
    settings
}

fn prompt() -> SendMessageRequest {
    SendMessageRequest {
        text: "Do not dispatch while the old authority remains unsettled.".to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

fn assert_retained(state: &AppState, project: &str, session: &str, code: &str) {
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    let index = inner.find_session_index(session).unwrap();
    let record = &inner.sessions[index];
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("retained-token")
    );
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert!(!inner.engram_project_resets.contains(project));
    assert!(record.session.messages.iter().any(|message| matches!(
        message,
        Message::EngramControl { card, .. }
            if card.stage == EngramControlStage::Checkpoint
                && card.decision == EngramControlCardDecision::Degraded
                && card.refusal_code.as_deref() == Some(code)
    )));
}

fn refused_containment(code: &str) {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_refusal_reply(code)]);
    install_control_only_transport(&state, transport.clone());
    let result = state.update_project_engram_settings(&project, settings(&state, &project, false));
    assert!(
        result.is_ok(),
        "a session refusal must not veto same-authority containment: {}",
        result.err().map(|error| error.message).unwrap_or_default()
    );
    assert_retained(&state, &project, &session, code);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "session_bind"),
        "a refusal is not evidence permitting replacement of the old binding"
    );
    let persisted = persisted_session_json(&state, &session);
    assert!(persisted.contains("retained-grant"));
    assert!(persisted.contains("retained-token"));
    {
        let inner = state.inner.lock().unwrap();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err());
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_ok(),
            "inactive original report debt permits only fresh unmediated OFF use");
        assert!(AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap().is_none());
    }
    assert!(
        receiver.try_recv().is_err(),
        "an excluded session sends no provider command"
    );
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        !loaded
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("retained-token")
    );
}

#[test]
fn project_containment_scope_mismatch_preserves_unresolved_authority() {
    refused_containment("GrantScopeMismatch");
}

#[test]
fn project_containment_not_begun_does_not_prove_begin_was_unsent() {
    refused_containment("GrantNotBegun");
}

#[test]
fn project_containment_unknown_schema_preserves_unresolved_authority() {
    refused_containment("UnknownControlSchema");
}

#[test]
fn project_containment_lost_checkpoint_reply_preserves_authority() {
    let (state, project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(
        Err(EngramTransportError::deadline("checkpoint reply lost")),
    )]);
    install_control_only_transport(&state, transport.clone());
    let result = state.update_project_engram_settings(&project, settings(&state, &project, false));
    assert!(
        result.is_ok(),
        "a lost checkpoint reply must not veto containment: {}",
        result.err().map(|error| error.message).unwrap_or_default()
    );
    assert_retained(&state, &project, &session, "deadline_exceeded");
    assert!(
        !transport
            .requests()
            .iter()
            .any(|r| r.request["operation"] == "session_bind")
    );
    {
        let inner = state.inner.lock().unwrap();
        assert!(require_no_engram_session_obligation(&inner, &session).is_err());
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_ok());
    }
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_strict_reset_error_keeps_refusal_code() {
    for code in [
        "GrantScopeMismatch",
        "GrantNotBegun",
        "UnknownControlSchema",
    ] {
        let (state, project, session, _root, _receiver) = fixture();
        {
            let mut inner = state.inner.lock().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project)
                .unwrap()
                .engram
                .as_mut()
                .unwrap()
                .turn_gated_control = false;
            state.commit_locked(&mut inner).unwrap();
        }
        let transport = ScriptedEngramControlTransport::new([checkpoint_refusal_reply(code)]);
        install_control_only_transport(&state, transport);
        // The reverse transition is deliberately strict. Availability of the
        // containment operation grants no exception to enabling control.
        let error = state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .err()
            .expect("an unsettled strict reset must be refused");
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error.message.contains(code),
            "refusal code was lost: {}",
            error.message
        );
        assert!(error.message.contains(&session));
        assert!(!error.message.contains("retained-token"));
    }
}

#[test]
fn project_containment_tier_restore_keeps_exact_refused_checkpoint_and_exclusion() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        inner
            .projects
            .iter_mut()
            .find(|p| p.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = false;
        state.commit_locked(&mut inner).unwrap();
    }
    queue_test_engram_prompt(&state, &session, "held behind original checkpoint",
        QueuedPromptSource::User, None);
    let transport = ScriptedEngramControlTransport::new([
        checkpoint_refusal_reply("GrantScopeMismatch"),
    ]);
    install_control_only_transport(&state, transport.clone());
    let error = state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .err()
        .expect("the initial unresolved checkpoint must refuse the reset");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.kind, Some(ApiErrorKind::EngramProjectResetRefused));
    assert!(error.message.contains("GrantScopeMismatch"));
    assert!(error.message.contains("turn_checkpoint"));
    assert!(error.message.contains(&session));
    assert!(!error.message.contains("retained-token"));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request["operation"], "turn_checkpoint");
    let (original, duty, head, paused) = {
        let inner = state.inner.lock().unwrap();
        assert!(!inner.find_project(&project).unwrap().engram.as_ref().unwrap().turn_gated_control);
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let original = record.engram.checkpoint_operation.clone().unwrap();
        assert_eq!(requests[0].request, serde_json::to_value(&original.original.request).unwrap());
        assert_eq!(requests[0].connection, original.original.connection);
        assert!(matches!(original.attempts.last().unwrap().result,
            Some(EngramCheckpointAttemptResult::Refused { .. })));
        let duty = engram_session_obligation_locked(&inner, &session).unwrap().clone();
        assert_eq!(duty.kind, EngramSessionObligationKind::ExistingCheckpoint);
        assert_ne!(duty.phase, EngramSessionObligationPhase::Confirmed);
        (
            original,
            duty,
            record.queued_prompts.front().cloned().unwrap(),
            (record.orchestrator_auto_dispatch_blocked, record.engram.operator_paused),
        )
    };
    // Restoring the same authority's settings does not settle its original duty.
    state.update_project_engram_settings(&project, settings(&state, &project, true)).unwrap();
    let owner = {
        let inner = state.inner.lock().unwrap();
        assert!(inner.find_project(&project).unwrap().engram.as_ref().unwrap().turn_gated_control);
        assert!(!inner.engram_project_resets.contains(&project));
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert!(!record.engram.project_reset_in_progress);
        assert_eq!((record.orchestrator_auto_dispatch_blocked, record.engram.operator_paused), paused);
        assert_eq!(serde_json::to_value(record.queued_prompts.front()).unwrap(), serde_json::to_value(&head).unwrap());
        assert_eq!(serde_json::to_value(&record.engram.checkpoint_operation).unwrap(), serde_json::to_value(&original).unwrap());
        assert_eq!(serde_json::to_value(engram_session_obligation_locked(&inner, &session)).unwrap(), serde_json::to_value(&duty).unwrap());
        assert!(require_no_engram_local_dispatch_obligation(&inner, &session).is_err());
        EngramQueuedAdmissionOwner::capture(record).unwrap()
    };
    assert!(state.dispatch_next_queued_turn_for_activation(&session, owner).unwrap().is_none());
    assert!(state.dispatch_turn(&session, prompt()).is_err());
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), 1, "restore and refused dispatch add no wire operation");
    state.shutdown_persist_blocking();
    let inner = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(inner.find_project(&project).unwrap().engram.as_ref().unwrap().turn_gated_control);
    assert_eq!(serde_json::to_value(engram_session_obligation_locked(&inner, &session)).unwrap(), serde_json::to_value(&duty).unwrap());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert_eq!(
        record.engram.routing_token.as_deref(),
        Some("retained-token")
    );
}

#[test]
fn project_containment_checkpoint_diagnostics_exclude_private_transport_output() {
    for error in [
        EngramTransportError::remote(EngramControlErrorBody {
            code: "GrantScopeMismatch".to_owned(),
            message: "private routing-token and request-payload".to_owned(),
        }),
        EngramTransportError::transport("private routing-token and request-payload"),
    ] {
        let diagnostic =
            engram_project_checkpoint_failure("fixture-project", "fixture-session", &error);
        assert!(diagnostic.contains("fixture-session"));
        assert!(diagnostic.contains("turn_checkpoint"));
        assert!(!diagnostic.contains("routing-token"));
        assert!(!diagnostic.contains("request-payload"));
        if let Some(code) = error.code {
            assert!(diagnostic.contains(&code));
        }
    }
}

#[test]
fn project_containment_healthy_checkpoint_applies_and_clears_exact_grant() {
    let (state, project, session, _root, _receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
    install_control_only_transport(&state, transport.clone());
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(
        !inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap()
            .turn_gated_control
    );
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.engram.active_grant_id.is_none());
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
}

#[test]
fn project_containment_unaffected_session_keeps_provider_progress() {
    let (state, project, session, root, receiver) = fixture();
    let unaffected = create_test_project_session(&state, Agent::Codex, &project, &root);
    let transport =
        ScriptedEngramControlTransport::new([checkpoint_refusal_reply("GrantScopeMismatch")]);
    install_control_only_transport(&state, transport);
    state
        .update_project_engram_settings(&project, settings(&state, &project, false))
        .expect("one unresolved session must not block containment for its project");
    assert_retained(&state, &project, &session, "GrantScopeMismatch");
    let dispatch = match state.dispatch_turn(&unaffected, prompt()).unwrap() {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the unaffected idle session must progress"),
    };
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
}
