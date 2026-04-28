// orchestrator subsystem: lifecycle, transitions, stop recovery, and templates.
//
// an orchestrator template declares N session slots (planner, builder,
// reviewer, ...) with agent + prompt defaults plus transitions between them
// (e.g. "when planner idles, dispatch planner's final turn message to
// implementer"). a template spawns an instance: a live run that holds a
// template_session_id -> real session_id mapping, a list of
// `pending_transitions`, and a state machine of Running -> StopInProgress ->
// Stopped.
//
// stop semantics are the hard part. stopping an instance must stop every
// child session without losing work that arrived mid-stop. aborted stops
// (a child failed to stop cleanly, usually via a persist failure) must
// recover gracefully on resume or full restart and must never re-dispatch
// orchestrator work that already completed during the stop window. blocked
// sessions (auto-dispatch blocked after a failed stop) require manual
// recovery that prioritizes the user's recovery prompt over pending
// orchestrator transitions while preserving FIFO for older queued user
// prompts and never re-firing stale orchestrator work.
//
// `begin_orchestrator_stop` must roll back its StopInProgress guard on
// persist failure or the instance would be stuck forever. `load_state` on
// startup reconciles mid-stop instances: completed children let the stop
// finish; unstopped children keep the instance Running while pending
// transitions pointing at already-stopped children are pruned. templates
// round-trip through `orchestrator_template_from_draft` /
// `orchestrator_template_to_draft`. stop/fail/mark-error/runtime-exit must
// never schedule orchestrator transitions — those are reactive completion
// events, not error paths.
//
// production surfaces: `AppState::create_orchestrator_instance`,
// `stop_orchestrator_instance`, `begin_orchestrator_stop`,
// `schedule_orchestrator_transitions_for_completed_session`,
// `orchestrator_template_from_draft`, and the `load_state` recovery paths
// live in `src/orchestrators.rs` and `src/state.rs`.

use super::*;

// Pins the /api/orchestrators POST fallback: an empty projectId in the
// request falls back to the template's own project, and the snapshot echoes
// that resolved project id on every session instance.
// Guards against silently dropping a template's default project and
// against the response forgetting to inline the fallback project id.
#[tokio::test]
async fn create_orchestrator_instance_route_uses_template_project_when_request_project_id_is_empty()
{
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-route-empty-project-id-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route fallback project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Fallback Project");
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let template_id = template.id.clone();
    let template_session_count = template.sessions.len();

    let app = app_router(state);
    let (status, response): (StatusCode, CreateOrchestratorInstanceResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/orchestrators")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "templateId": template_id,
                    "projectId": "",
                }))
                .expect("request body should serialize"),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(response.orchestrator.project_id, project_id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template_session_count
    );
    let _ = fs::remove_dir_all(project_root);
}

// Pins /api/orchestrators/{id}/{pause,resume,stop}: status transitions
// flow through the HTTP layer, and a successful stop idles active child
// sessions, clears their queued orchestrator follow-ups, and records a
// completed_at stamp.
// Guards against the route returning stale orchestrator state, leaking an
// Active child runtime past stop, or forgetting to drain queued prompts.
#[tokio::test]
async fn orchestrator_lifecycle_routes_update_state_and_stop_active_sessions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-lifecycle-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("route-orchestrator-stop");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[planner_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued orchestrator follow-up".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[planner_index]);
    }

    let app = app_router(state.clone());
    let (pause_status, pause_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/pause"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(pause_status, StatusCode::OK);
    let paused = pause_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("paused orchestrator should be present");
    assert_eq!(paused.status, OrchestratorInstanceStatus::Paused);

    let (resume_status, resume_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/resume"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(resume_status, StatusCode::OK);
    let resumed = resume_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("resumed orchestrator should be present");
    assert_eq!(resumed.status, OrchestratorInstanceStatus::Running);

    let (stop_status, stop_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(stop_status, StatusCode::OK);
    let stopped = stop_response
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("stopped orchestrator should be present");
    assert_eq!(stopped.status, OrchestratorInstanceStatus::Stopped);
    assert!(stopped.pending_transitions.is_empty());
    assert!(stopped.completed_at.is_some());

    let planner_session = stop_response
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Idle);
    assert!(planner_session.pending_prompts.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should still exist");
    assert_eq!(planner_record.session.status, SessionStatus::Idle);
    assert!(matches!(planner_record.runtime, SessionRuntime::None));
    assert!(planner_record.queued_prompts.is_empty());
    assert!(planner_record.session.pending_prompts.is_empty());
    assert!(
        planner_record
            .session
            .messages
            .iter()
            .any(|message| matches!(
                message,
                Message::Text { text, .. } if text == "Turn stopped by user."
            ))
    );
    drop(inner);
    assert!(!planner_session.messages_loaded);
    assert!(planner_session.messages.is_empty());

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Pins the stop-route rollback: when one child's kill fails mid-stop the
// route returns 500 and the instance stays Running (not half-Stopped),
// while children that did stop cleanly keep their Idle state both in
// memory and on disk.
// Guards against leaving the instance wedged in StopInProgress or
// silently marking it Stopped despite a live child process.
#[tokio::test]
async fn orchestrator_stop_route_preserves_running_state_when_a_child_stop_fails() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-failure-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("route project root should exist");
    let project_id = create_test_project(&state, &project_root, "Route Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let failing_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (planner_input_tx, _planner_input_rx) = mpsc::channel();
    let planner_runtime = ClaudeRuntimeHandle {
        runtime_id: "route-orchestrator-stop-fail".to_owned(),
        input_tx: planner_input_tx,
        process: failing_process.clone(),
    };
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("route-orchestrator-stop-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        for (session_id, runtime) in [
            (
                planner_session_id.clone(),
                SessionRuntime::Claude(planner_runtime),
            ),
            (
                reviewer_session_id.clone(),
                SessionRuntime::Claude(reviewer_runtime),
            ),
        ] {
            let index = inner
                .find_session_index(&session_id)
                .expect("orchestrator session should exist");
            inner.sessions[index].runtime = runtime;
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].active_turn_start_message_count =
                Some(inner.sessions[index].session.messages.len());
        }
    }

    let app = app_router(state.clone());
    let failure_guard = force_test_kill_child_process_failure(&failing_process, "Claude");
    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/orchestrators/{instance_id}/stop"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should read");
    let error: Value = serde_json::from_slice(&body).expect("error response should parse");
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("failed to stop session `"))
    );
    drop(failure_guard);

    let snapshot = state.full_snapshot();
    let instance = snapshot
        .orchestrators
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still be present");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(instance.completed_at.is_none());

    let planner_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == planner_session_id)
        .expect("planner session should still be present");
    assert_eq!(planner_session.status, SessionStatus::Active);

    let reviewer_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == reviewer_session_id)
        .expect("reviewer session should still be present");
    assert_eq!(reviewer_session.status, SessionStatus::Idle);
    assert!(reviewer_session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(reloaded_instance.completed_at.is_none());
    let reloaded_reviewer = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist");
    assert_eq!(reloaded_reviewer.session.status, SessionStatus::Idle);

    let _ = failing_process.kill();
    let _ = failing_process.wait();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the aborted-stop cleanup path: if child-stop persistence fails,
// the stop guard tracks zero stopped children and
// prune_pending_transitions_for_stopped_orchestrator_sessions keeps the
// builder's queued work and pending transition intact, rolling the
// instance back to Running both in memory and on reload.
// Guards against the cleanup path throwing away uncommitted child work
// or leaving stale stopped-id entries after the persist failure.
#[test]
fn aborted_stop_cleanup_preserves_child_work_when_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-cleanup-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Cleanup");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-cleanup-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder work that should survive aborted cleanup".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should survive aborted cleanup".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert!(instance.stopped_session_ids_during_stop.is_empty());
    }
    assert!(
        state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .get(&instance_id)
            .is_some_and(|session_ids| session_ids.is_empty())
    );

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve work for uncommitted child stops");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(!instance.stop_in_progress);
        assert!(instance.active_session_ids_during_stop.is_none());
        assert!(instance.stopped_session_ids_during_stop.is_empty());
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!reloaded_instance.stop_in_progress);
    assert!(reloaded_instance.active_session_ids_during_stop.is_none());
    assert!(reloaded_instance.stopped_session_ids_during_stop.is_empty());
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert_eq!(reloaded_builder.queued_prompts.len(), 1);
    assert_eq!(reloaded_builder.session.pending_prompts.len(), 1);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins aborted-stop resume within the same process: after cleanup,
// resume_pending_orchestrator_transitions leaves the pending transition
// parked because the child is flagged orchestrator_auto_dispatch_blocked.
// The block survives a fresh load_state.
// Guards against the resume path auto-re-firing a transition into a
// session that was left blocked by a failed stop.
#[test]
fn aborted_stop_resume_does_not_redispatch_child_after_child_stop_persist_fails() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-resume-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Persist Failure Resume");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-resume-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after resume"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = original_persistence_path.clone();
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stop resume should succeed without redispatching the blocked child");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(original_persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_instance = reloaded_inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("persisted orchestrator should still exist");
    assert_eq!(
        reloaded_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert_eq!(reloaded_instance.pending_transitions.len(), 1);
    assert_eq!(
        reloaded_instance.pending_transitions[0].destination_session_id,
        builder_session_id
    );
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should still exist");
    assert!(reloaded_builder.orchestrator_auto_dispatch_blocked);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins aborted-stop resume across a full process restart: after a new
// AppState loads from disk the pending transition is still parked, the
// builder is still auto_dispatch_blocked, and no transition fires during
// recovery.
// Guards against reboot re-dispatch — once a blocked child is persisted
// blocked, load_state must not re-arm its pending transition.
#[test]
fn aborted_stop_restart_does_not_redispatch_child_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Restart assertions read the JSON fixture immediately; keep all setup
    // commits on the synchronous test fallback instead of racing the
    // background persist worker.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "builder pending work should remain pending after restart"
                    .to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve pending child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert_eq!(instance.pending_transitions.len(), 1);
        assert_eq!(
            instance.pending_transitions[0].destination_session_id,
            builder_session_id
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    drop(restarted);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

// Pins the orphaned-queue variant: when the child had an orchestrator
// prompt already queued before the failed stop, a restart keeps the
// queued prompt parked behind the auto_dispatch_blocked flag instead of
// firing it on boot.
// Guards against boot-time auto-dispatch of a pre-queued orchestrator
// prompt that should be gated on manual recovery.
#[test]
fn aborted_stop_restart_does_not_dispatch_orphaned_child_queue_after_child_stop_persist_fails() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-restart-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Restart assertions read the JSON fixture immediately; keep all setup
    // commits on the synchronous test fallback instead of racing the
    // background persist worker.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Persist Failure Restart Queue");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (builder_runtime, _builder_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-restart-queued-builder");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].runtime = SessionRuntime::Claude(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].active_turn_start_message_count =
            Some(inner.sessions[builder_index].session.messages.len());
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-restart-builder".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder queued work should remain parked after restart".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    // Force synchronous persistence so the error propagates instead of
    // being swallowed by the background persist thread.
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should still exist after restart");
        assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
        assert!(instance.pending_transitions.is_empty());
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should still exist after restart");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.orchestrator_auto_dispatch_blocked);
        assert_eq!(builder.queued_prompts.len(), 1);
        assert_eq!(builder.session.pending_prompts.len(), 1);
        assert_eq!(
            builder.queued_prompts[0].pending_prompt.text,
            "builder queued work should remain parked after restart"
        );
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins manual recovery on a blocked orchestrator child: a wrong-agent
// runtime rejects with 500 without clearing the block, and once the
// correct Claude runtime is attached the user's recovery prompt
// dispatches immediately while the older orchestrator queue stays parked
// behind it.
// Guards against stale orchestrator work jumping ahead of the recovery
// prompt or the first failed attempt silently unblocking the session.
#[test]
fn blocked_session_manual_recovery_dispatch_prioritizes_user_prompt_after_restart() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-persist-failure-manual-recovery-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");
    let failing_persistence_path = state_root.join("persist-failure");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    let project_id = create_test_project(&state, &project_root, "Manual Recovery Ordering");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (reviewer_runtime, _reviewer_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-persist-failure-manual-recovery-reviewer");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(reviewer_runtime);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[reviewer_index].active_turn_start_message_count =
            Some(inner.sessions[reviewer_index].session.messages.len());
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-persist-failure-manual-recovery-reviewer".to_owned(),
                    timestamp: stamp_now(),
                    text: "reviewer queued work should stay behind the user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let error = state
        .stop_session_with_options(
            &reviewer_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: Some(instance_id.clone()),
            },
        )
        .err()
        .expect("persist failures should abort child stop persistence");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    state.persistence_path = Arc::new(persistence_path.clone());
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted cleanup should preserve queued child work");
    state.finish_orchestrator_stop(&instance_id);
    drop(state);

    let restarted = AppState::new_with_paths(
        normalized_root,
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should restart");
    let (wrong_runtime, _wrong_input_rx) = test_codex_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-wrong-runtime",
    );
    let baseline_message_count = {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Codex(wrong_runtime);
        inner.sessions[reviewer_index].session.messages.len()
    };

    let failed_recovery = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "this failed recovery should not clear the block".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .err()
        .expect("wrong runtime should reject the first manual recovery attempt");
    assert_eq!(failed_recovery.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        failed_recovery
            .message
            .contains("unexpected Codex runtime attached to Claude session")
    );

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after failed manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Idle);
        assert!(reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(reviewer.session.messages.len(), baseline_message_count);
        assert!(reviewer.session.messages.iter().all(|message| !matches!(
            message,
            Message::Text { text, author: Author::You, .. }
                if text.contains("this failed recovery should not clear the block")
        )));
    }

    let (restart_reviewer_runtime, _restart_reviewer_input_rx) = test_claude_runtime_handle(
        "orchestrator-stop-persist-failure-manual-recovery-reviewer-restarted",
    );
    {
        let mut inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist after restart");
        inner.sessions[reviewer_index].runtime = SessionRuntime::Claude(restart_reviewer_runtime);
    }

    let dispatch_result = restarted
        .dispatch_turn(
            &reviewer_session_id,
            SendMessageRequest {
                text: "please continue with a manual recovery prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery prompt should dispatch");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert!(
                command
                    .text
                    .contains("please continue with a manual recovery prompt")
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("manual recovery should dispatch on the reviewer Claude runtime")
        }
        DispatchTurnResult::Queued => panic!("manual recovery prompt should dispatch immediately"),
    }

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let reviewer = inner
            .sessions
            .iter()
            .find(|record| record.session.id == reviewer_session_id)
            .expect("reviewer session should still exist after manual recovery");
        assert_eq!(reviewer.session.status, SessionStatus::Active);
        assert!(!reviewer.orchestrator_auto_dispatch_blocked);
        assert_eq!(reviewer.queued_prompts.len(), 1);
        assert_eq!(reviewer.session.pending_prompts.len(), 1);
        assert_eq!(
            reviewer.queued_prompts[0].pending_prompt.text,
            "reviewer queued work should stay behind the user prompt"
        );
        assert!(matches!(
            reviewer.session.messages.last(),
            Some(Message::Text { text, author: Author::You, .. })
                if text.contains("please continue with a manual recovery prompt")
        ));
    }

    drop(restarted);

    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins FIFO for queued user prompts on a plain blocked session: after
// the user queued a prompt, a failed stop persistence flipped the
// auto_dispatch_blocked flag. Manual recovery then dispatches the
// *older* user prompt first and requeues the new recovery prompt
// behind it.
// Guards against a new user prompt stealing the dispatch slot from an
// older queued user prompt during block recovery.
#[test]
fn blocked_session_manual_recovery_preserves_user_prompt_fifo_after_plain_stop_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-user-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked FIFO".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("plain-stop-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 1);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::User
        );
    }

    let mut delta_rx = state.subscribe_delta_events();
    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new recovery prompt should stay behind old queued user work".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the oldest queued user prompt");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(command.text, "older queued user prompt");
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("plain blocked FIFO recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("plain blocked FIFO recovery should dispatch immediately")
        }
    }
    let queued_delta_stamp = match serde_json::from_str(
        &delta_rx
            .try_recv()
            .expect("queued dispatch should publish a started-turn delta"),
    )
    .expect("started-turn delta should decode")
    {
        DeltaEvent::MessageCreated {
            session_mutation_stamp,
            ..
        } => session_mutation_stamp,
        _ => panic!("expected queued dispatch to publish MessageCreated"),
    };

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.session.pending_prompts.len(), 1);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new recovery prompt should stay behind old queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert_eq!(
            queued_delta_stamp,
            Some(record.mutation_stamp),
            "queued turn delta should carry the final session mutation stamp"
        );
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins the mixed-queue recovery ordering: with a stale orchestrator
// prompt queued ahead of a user prompt, manual recovery dispatches the
// older user prompt first, leaves the stale orchestrator entry in the
// queue, and slots the fresh recovery prompt ahead of it as a User
// entry.
// Guards against stale orchestrator work winning the dispatch race
// against older queued user work during manual recovery.
#[test]
fn blocked_session_manual_recovery_prioritizes_existing_user_queue_ahead_of_stale_orchestrator_work()
 {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-persist-failure-mixed-queue-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Blocked Mixed Queue".to_owned()),
            workdir: Some(state.default_workdir.clone()),
            project_id: None,
            model: Some("claude-sonnet-4-5".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id.clone();
    let (initial_runtime, _initial_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-initial-runtime");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(initial_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_start_message_count =
            Some(inner.sessions[index].session.messages.len());
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stale-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "older stale orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-older-user-prompt-mixed".to_owned(),
                    timestamp: stamp_now(),
                    text: "older queued user prompt behind stale orchestrator work".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        state.commit_locked(&mut inner).unwrap();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;
    let stop_error = state
        .stop_session_with_options(&session_id, StopSessionOptions::default())
        .err()
        .expect("persist failures should abort plain stop persistence");
    assert_eq!(stop_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        stop_error
            .message
            .contains("failed to persist session state")
    );

    state.persistence_path = original_persistence_path.clone();
    let (recovery_runtime, _recovery_input_rx) =
        test_claude_runtime_handle("mixed-queue-persist-failure-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist after stop failure");
        inner.sessions[index].runtime = SessionRuntime::Claude(recovery_runtime);
        assert!(inner.sessions[index].orchestrator_auto_dispatch_blocked);
        assert_eq!(inner.sessions[index].queued_prompts.len(), 2);
        assert_eq!(
            inner.sessions[index].queued_prompts[0].source,
            QueuedPromptSource::Orchestrator
        );
        assert_eq!(
            inner.sessions[index].queued_prompts[1].source,
            QueuedPromptSource::User
        );
    }

    let dispatch_result = state
        .dispatch_turn(
            &session_id,
            SendMessageRequest {
                text: "new manual recovery prompt should not jump ahead of older queued user work"
                    .to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
            },
        )
        .expect("manual recovery should dispatch the older queued user prompt first");

    match dispatch_result {
        DispatchTurnResult::Dispatched(TurnDispatch::PersistentClaude { command, .. }) => {
            assert_eq!(
                command.text,
                "older queued user prompt behind stale orchestrator work"
            );
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("mixed blocked recovery should dispatch on the Claude runtime")
        }
        DispatchTurnResult::Queued => {
            panic!("mixed blocked recovery should dispatch immediately")
        }
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == session_id)
            .expect("session should still exist after mixed recovery dispatch");
        assert_eq!(record.session.status, SessionStatus::Active);
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(
            record.queued_prompts[0].pending_prompt.text,
            "new manual recovery prompt should not jump ahead of older queued user work"
        );
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert_eq!(
            record.queued_prompts[1].pending_prompt.text,
            "older stale orchestrator prompt"
        );
        assert_eq!(
            record.queued_prompts[1].source,
            QueuedPromptSource::Orchestrator
        );
    }

    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins the completion-during-stop guard: the planner completes its
// turn while the orchestrator stop is in flight, scheduling two pending
// transitions. After the builder stops cleanly mid-stop,
// prune_pending_transitions_for_stopped_orchestrator_sessions drops the
// planner-to-builder transition but keeps planner-to-reviewer, and
// finish_orchestrator_stop + resume delivers only the surviving one.
// Guards against the stop path relaunching a child that just stopped
// or discarding transitions aimed at still-healthy peers.
#[test]
fn aborted_stop_does_not_relaunch_child_work_completed_during_stop() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-stop-guard-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("stop guard project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Guard Project");
    let mut draft = sample_orchestrator_template_draft();
    draft.transitions.push(OrchestratorTemplateTransition {
        id: "planner-to-reviewer-during-stop".to_owned(),
        from_session_id: "planner".to_owned(),
        to_session_id: "reviewer".to_owned(),
        from_anchor: Some("right".to_owned()),
        to_anchor: Some("top".to_owned()),
        trigger: OrchestratorTransitionTrigger::OnCompletion,
        result_mode: OrchestratorTransitionResultMode::LastResponse,
        prompt_template: Some("Review this plan directly:\n\n{{result}}".to_owned()),
    });
    let template = state
        .create_orchestrator_template(draft)
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-guard-planner");
    let (builder_runtime, _builder_input_rx) =
        test_codex_runtime_handle("orchestrator-stop-guard-builder");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");

        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].runtime = SessionRuntime::Codex(builder_runtime);
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-builder-orchestrator-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "builder follow-up that should be cleared on aborted stop".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop guard should be acquired");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should succeed while stop is in flight");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        assert_eq!(instance.pending_transitions.len(), 2);
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == builder_session_id })
        );
        assert!(
            instance
                .pending_transitions
                .iter()
                .any(|pending| { pending.destination_session_id == reviewer_session_id })
        );
    }

    state
        .stop_session_with_options(
            &builder_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
        .expect("builder stop should succeed while the orchestrator stop is in flight");
    state.note_stopped_orchestrator_session(&instance_id, &builder_session_id);
    state
        .prune_pending_transitions_for_stopped_orchestrator_sessions(&instance_id)
        .expect("aborted stops should prune pending work for stopped children");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let planner_instance = instance
            .session_instances
            .iter()
            .find(|candidate| candidate.session_id == planner_session_id)
            .expect("planner instance should exist");
        assert_eq!(instance.pending_transitions.len(), 1);
        assert!(
            instance
                .pending_transitions
                .iter()
                .all(|pending| { pending.destination_session_id == reviewer_session_id })
        );
        assert_ne!(
            planner_instance.last_completion_revision,
            planner_instance.last_delivered_completion_revision
        );
        let builder = inner
            .sessions
            .iter()
            .find(|record| record.session.id == builder_session_id)
            .expect("builder session should exist");
        assert_eq!(builder.session.status, SessionStatus::Idle);
        assert!(matches!(builder.runtime, SessionRuntime::None));
        assert!(builder.queued_prompts.is_empty());
        assert!(builder.session.pending_prompts.is_empty());
    }

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded_builder = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist");
    assert!(reloaded_builder.queued_prompts.is_empty());
    assert!(reloaded_builder.session.pending_prompts.is_empty());

    state.finish_orchestrator_stop(&instance_id);
    state
        .resume_pending_orchestrator_transitions()
        .expect("aborted stops should resume completions for unstopped children");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should exist");
    let planner_instance = instance
        .session_instances
        .iter()
        .find(|candidate| candidate.session_id == planner_session_id)
        .expect("planner instance should exist");
    assert!(instance.pending_transitions.is_empty());
    assert_eq!(
        planner_instance.last_completion_revision,
        planner_instance.last_delivered_completion_revision
    );
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.status, SessionStatus::Idle);
    assert!(matches!(builder.runtime, SessionRuntime::None));
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    let reviewer = inner
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("reviewer session should exist");
    assert_eq!(reviewer.session.status, SessionStatus::Active);
    assert_eq!(reviewer.queued_prompts.len(), 1);
    assert_eq!(reviewer.session.pending_prompts.len(), 1);
    assert_eq!(
        reviewer.queued_prompts[0].source,
        QueuedPromptSource::Orchestrator
    );
    assert!(
        reviewer.session.pending_prompts[0]
            .text
            .contains("Implement the panel dragging changes.")
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the begin_orchestrator_stop pre-flight guards: a missing
// instance returns 404 and a stopped instance returns 409, and in
// neither case do the stopping_orchestrator_ids /
// stopping_orchestrator_session_ids maps retain a lingering entry.
// Guards against leaking stop guards on the error path, which would
// wedge future stop attempts against the same instance id.
#[test]
fn begin_orchestrator_stop_cleans_up_guards_on_missing_and_stopped_errors() {
    let state = test_app_state();
    let missing_instance_id = "missing-orchestrator-instance";
    let error = state
        .begin_orchestrator_stop(missing_instance_id)
        .expect_err("missing orchestrators should not start a stop");
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "orchestrator instance not found");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(missing_instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(missing_instance_id)
    );

    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-errors-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Errors Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance = inner
            .orchestrator_instances
            .iter_mut()
            .find(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        instance.status = OrchestratorInstanceStatus::Stopped;
        instance.stop_in_progress = false;
    }

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("stopped orchestrators should reject stop");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "orchestrator is already stopped");
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Stopped);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the persist-failure rollback in begin_orchestrator_stop: if the
// StopInProgress checkpoint cannot be persisted, the instance reverts
// to Running with stop_in_progress cleared, and the stopping_* guard
// maps are empty.
// Guards against the instance getting stuck permanently in
// StopInProgress — exactly the failure mode that motivates the whole
// rollback dance.
#[test]
fn begin_orchestrator_stop_rolls_back_stop_in_progress_after_persist_failure() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-{}",
        Uuid::new_v4()
    ));
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-begin-persist-failure-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    let project_id = create_test_project(&state, &project_root, "Begin Stop Persist Failure");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let error = state
        .begin_orchestrator_stop(&instance_id)
        .expect_err("persistence failures should abort stop initialization");
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist orchestrator stop state")
    );
    assert!(
        !state
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .contains(&instance_id)
    );
    assert!(
        !state
            .stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .contains_key(&instance_id)
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let instance = inner
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("orchestrator should still exist");
    assert_eq!(instance.status, OrchestratorInstanceStatus::Running);
    assert!(!instance.stop_in_progress);
    assert!(instance.active_session_ids_during_stop.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(original_persistence_path.as_path());
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins load_state recovery when a StopInProgress had not stopped any
// child: the recovered instance becomes Running again with cleared
// stop_in_progress / active_session_ids_during_stop, and pending
// transitions targeting untouched peers survive the reload.
// Guards against load_state either completing the stop prematurely
// (since no children were confirmed stopped) or dropping still-valid
// pending transitions while unwinding.
#[test]
fn load_state_preserves_pending_transitions_when_stop_in_progress_has_no_stopped_children() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[builder_index].session.status = SessionStatus::Active;
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert_eq!(recovered_builder.session.status, SessionStatus::Error);
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins load_state recovery when the last active child completed its
// turn during the stop window: on reload the instance flips to Stopped
// with completed_at set, all pending transitions are discarded, and
// the mid-stop snapshot we examined on disk still had stopInProgress =
// true to prove the recovery logic — not the writer — finished it.
// Guards against reboot leaving the instance stuck StopInProgress when
// the only remaining work finished in flight.
#[test]
fn load_state_recovers_completed_stop_when_active_children_finished_during_stop() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-restart-finished-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("restart recovery project root should exist");
    fs::create_dir_all(&state_root).expect("restart recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    // Force synchronous persistence so file reads in this test see the
    // written data immediately.
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Restart Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (planner_runtime, _planner_input_rx) =
        test_claude_runtime_handle("orchestrator-stop-restart-planner");

    let planner_token = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(planner_runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;

        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview =
            "Implement the panel dragging changes.".to_owned();
        inner.sessions[planner_index]
            .runtime
            .runtime_token()
            .expect("planner runtime token should exist")
    };

    state
        .begin_orchestrator_stop(&instance_id)
        .expect("stop should be marked in progress");
    state
        .finish_turn_ok_if_runtime_matches(&planner_session_id, &planner_token)
        .expect("planner completion should persist while stop is in flight");

    let persisted_mid_stop: Value = serde_json::from_slice(
        &fs::read(&persistence_path).expect("mid-stop state file should exist"),
    )
    .expect("mid-stop state should deserialize");
    let persisted_mid_stop_instance = persisted_mid_stop["orchestratorInstances"]
        .as_array()
        .expect("persisted orchestrator instances should be present")
        .iter()
        .find(|candidate| candidate["id"] == instance_id)
        .expect("persisted orchestrator should exist");
    assert_eq!(
        persisted_mid_stop_instance["status"],
        Value::String("running".to_owned())
    );
    assert_eq!(
        persisted_mid_stop_instance["stopInProgress"],
        Value::Bool(true)
    );
    assert_eq!(
        persisted_mid_stop_instance["pendingTransitions"]
            .as_array()
            .expect("pending transitions should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"]
            .as_array()
            .expect("active stop session ids should be present")
            .len(),
        1
    );
    assert_eq!(
        persisted_mid_stop_instance["activeSessionIdsDuringStop"][0],
        Value::String(planner_session_id.clone())
    );

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins the targeted prune in load_state recovery: with one active
// child still unstopped and one already stopped, transitions pointing
// at the stopped child are dropped while the one targeting the
// surviving child is kept, and only the stopped child's queued prompts
// are cleared.
// Guards against an over-aggressive prune that would also wipe the
// active peer's queued work or surviving transitions.
#[test]
fn load_state_prunes_only_stopped_child_work_when_recovering_stop_in_progress() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-queued-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("recovery project root should exist");
    fs::create_dir_all(&state_root).expect("recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Recovery Queue Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop = Some(vec![
            builder_session_id.clone(),
            reviewer_session_id.clone(),
        ]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-builder".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: builder_session_id.clone(),
                completion_revision,
                rendered_prompt: "stale stop recovery prompt".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "planner-to-reviewer".to_owned(),
                source_session_id: planner_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "reviewer work should survive recovery".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-recovery-orchestrator-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "stale queued orchestrator prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[builder_index]);
        state
            .persist_internal_locked(&inner)
            .expect("stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Running
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert_eq!(recovered_instance.pending_transitions.len(), 1);
    assert_eq!(
        recovered_instance.pending_transitions[0].destination_session_id,
        reviewer_session_id
    );
    let recovered_builder = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("persisted builder session should exist after restart");
    assert!(recovered_builder.queued_prompts.is_empty());
    assert!(recovered_builder.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins load_state recovery when every recorded active child stopped
// before shutdown: the instance flips to Stopped, completed_at is set,
// pending transitions targeting stopped children are dropped, and the
// idle peer's queued orchestrator prompt is cleared.
// Guards against reboot resurrecting transitions into children that
// were already stopped or leaving phantom queued work on idle peers.
#[test]
fn load_state_recovers_completed_stop_when_all_active_children_were_stopped() {
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-{}",
        Uuid::new_v4()
    ));
    let state_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-recovery-complete-state-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("completed recovery project root should exist");
    fs::create_dir_all(&state_root).expect("completed recovery state root should exist");
    let normalized_root = normalize_user_facing_path(&fs::canonicalize(&project_root).unwrap())
        .to_string_lossy()
        .into_owned();
    let persistence_path = state_root.join("sessions.json");
    let orchestrator_templates_path = state_root.join("orchestrators.json");

    let mut state = AppState::new_with_paths(
        normalized_root.clone(),
        persistence_path.clone(),
        orchestrator_templates_path.clone(),
    )
    .expect("state should initialize");
    state.persist_tx = mpsc::channel().0;
    let project_id = create_test_project(&state, &project_root, "Completed Recovery Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;
    let instance_id = orchestrator.id.clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let reviewer_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "reviewer")
        .expect("reviewer session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let instance_index = inner
            .orchestrator_instances
            .iter()
            .position(|candidate| candidate.id == instance_id)
            .expect("orchestrator should exist");
        let completion_revision = inner.revision.saturating_add(1);
        inner.orchestrator_instances[instance_index].stop_in_progress = true;
        inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
            Some(vec![builder_session_id.clone()]);
        inner.orchestrator_instances[instance_index]
            .stopped_session_ids_during_stop
            .push(builder_session_id.clone());
        inner.orchestrator_instances[instance_index]
            .pending_transitions
            .push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: "builder-to-reviewer".to_owned(),
                source_session_id: builder_session_id.clone(),
                destination_session_id: reviewer_session_id.clone(),
                completion_revision,
                rendered_prompt: "idle reviewer work should be discarded".to_owned(),
                created_at: stamp_orchestrator_template_now(),
            });
        let reviewer_index = inner
            .find_session_index(&reviewer_session_id)
            .expect("reviewer session should exist");
        inner.sessions[reviewer_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::Orchestrator,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-completed-stop-reviewer-prompt".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued reviewer work should be discarded".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[reviewer_index]);
        state
            .persist_internal_locked(&inner)
            .expect("completed stop recovery state should persist");
    }

    drop(state);

    let recovered = load_state(&persistence_path)
        .expect("recovered state should load")
        .expect("recovered state should exist");
    let recovered_instance = recovered
        .orchestrator_instances
        .iter()
        .find(|candidate| candidate.id == instance_id)
        .expect("recovered orchestrator should exist");
    assert_eq!(
        recovered_instance.status,
        OrchestratorInstanceStatus::Stopped
    );
    assert!(!recovered_instance.stop_in_progress);
    assert!(recovered_instance.active_session_ids_during_stop.is_none());
    assert!(
        recovered_instance
            .stopped_session_ids_during_stop
            .is_empty()
    );
    assert!(recovered_instance.pending_transitions.is_empty());
    assert!(recovered_instance.completed_at.is_some());
    let recovered_reviewer = recovered
        .sessions
        .iter()
        .find(|record| record.session.id == reviewer_session_id)
        .expect("persisted reviewer session should exist after restart");
    assert!(recovered_reviewer.queued_prompts.is_empty());
    assert!(recovered_reviewer.session.pending_prompts.is_empty());
    let _ = fs::remove_file(persistence_path);
    let _ = fs::remove_file(orchestrator_templates_path);
    let _ = fs::remove_dir_all(state_root);
    let _ = fs::remove_dir_all(project_root);
}

// Pins the draft round-trip: a sample draft fed through
// orchestrator_template_from_draft and back through
// orchestrator_template_to_draft compares equal to the original.
// Guards against either helper dropping, reordering, or defaulting
// away any field in OrchestratorTemplateDraft.
#[test]
fn orchestrator_template_draft_round_trips_through_template_helpers() {
    let draft = sample_orchestrator_template_draft();
    let template = orchestrator_template_from_draft("template-round-trip", draft.clone())
        .expect("sample draft should normalize into a template");
    let round_tripped = orchestrator_template_to_draft(&template);

    assert_eq!(round_tripped, draft);
}

pub fn sample_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Feature Delivery Flow".to_owned(),
        description: "Coordinate implementation and review.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "planner".to_owned(),
                name: "Planner".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Plan the work and decide the next action.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 620.0, y: 120.0 },
            },
            OrchestratorSessionTemplate {
                id: "builder".to_owned(),
                name: "Builder".to_owned(),
                agent: Agent::Codex,
                model: Some("gpt-5".to_owned()),
                instructions: "Implement the requested changes.".to_owned(),
                auto_approve: true,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 180.0, y: 420.0 },
            },
            OrchestratorSessionTemplate {
                id: "reviewer".to_owned(),
                name: "Reviewer".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Review the produced changes and summarize issues.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 980.0, y: 420.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "planner-to-builder".to_owned(),
                from_session_id: "planner".to_owned(),
                to_session_id: "builder".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::LastResponse,
                prompt_template: Some(
                    "Use this plan and implement it:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "builder-to-reviewer".to_owned(),
                from_session_id: "builder".to_owned(),
                to_session_id: "reviewer".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::SummaryAndLastResponse,
                prompt_template: Some(
                    "Review this implementation:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

pub fn sample_deadlocked_orchestrator_template_draft() -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: "Consolidate Deadlock Flow".to_owned(),
        description: "Exercise remote deadlock skipping.".to_owned(),
        project_id: None,
        sessions: vec![
            OrchestratorSessionTemplate {
                id: "source-a".to_owned(),
                name: "Source A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the first source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "source-b".to_owned(),
                name: "Source B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Provide the second source input.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Queue,
                position: OrchestratorNodePosition { x: 120.0, y: 460.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-a".to_owned(),
                name: "Consolidate A".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source A and consolidate B.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 160.0 },
            },
            OrchestratorSessionTemplate {
                id: "consolidate-b".to_owned(),
                name: "Consolidate B".to_owned(),
                agent: Agent::Claude,
                model: Some("claude-sonnet-4-5".to_owned()),
                instructions: "Wait on source B and consolidate A.".to_owned(),
                auto_approve: false,
                input_mode: OrchestratorSessionInputMode::Consolidate,
                position: OrchestratorNodePosition { x: 760.0, y: 460.0 },
            },
        ],
        transitions: vec![
            OrchestratorTemplateTransition {
                id: "source-a-to-consolidate-a".to_owned(),
                from_session_id: "source-a".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source A summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-b-to-consolidate-a".to_owned(),
                from_session_id: "consolidate-b".to_owned(),
                to_session_id: "consolidate-a".to_owned(),
                from_anchor: Some("top".to_owned()),
                to_anchor: Some("bottom".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "source-b-to-consolidate-b".to_owned(),
                from_session_id: "source-b".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("right".to_owned()),
                to_anchor: Some("left".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Source B summary:

{{result}}"
                        .to_owned(),
                ),
            },
            OrchestratorTemplateTransition {
                id: "consolidate-a-to-consolidate-b".to_owned(),
                from_session_id: "consolidate-a".to_owned(),
                to_session_id: "consolidate-b".to_owned(),
                from_anchor: Some("bottom".to_owned()),
                to_anchor: Some("top".to_owned()),
                trigger: OrchestratorTransitionTrigger::OnCompletion,
                result_mode: OrchestratorTransitionResultMode::Summary,
                prompt_template: Some(
                    "Consolidate A summary:

{{result}}"
                        .to_owned(),
                ),
            },
        ],
    }
}

// Pins start_turn_on_record's remote-proxy check: a session with a
// remote_id + remote_session_id bounces with 500 and a clear error,
// and the record is untouched (no active_turn_start, no messages, no
// pending prompts).
// Guards against the local turn path ever writing state for a session
// that should only be driven through the remote backend.
#[test]
fn start_turn_on_record_rejects_remote_proxy_sessions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    inner.sessions[index].remote_id = Some("ssh-lab".to_owned());
    inner.sessions[index].remote_session_id = Some("remote-session-1".to_owned());

    let error = match state.start_turn_on_record(
        &mut inner.sessions[index],
        "message-remote-proxy".to_owned(),
        "Dispatch through the remote backend.".to_owned(),
        Vec::new(),
        None,
    ) {
        Ok(_) => panic!("remote proxy sessions should reject local turn dispatch"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        error.message,
        "remote proxy sessions must dispatch through the remote backend"
    );
    assert!(
        inner.sessions[index]
            .active_turn_start_message_count
            .is_none()
    );
    assert!(inner.sessions[index].session.messages.is_empty());
    assert!(inner.sessions[index].session.pending_prompts.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins failed transition delivery: when the destination's runtime
// can't accept the queued prompt, resume_pending_orchestrator_transitions
// still marks last_delivered_completion_revision on the source, clears
// the pending transition, and writes a visible "failed to queue
// prompt" error message on the destination's transcript.
// Guards against a stuck pending_transitions list when runtime
// delivery throws, and against silent failures that never surface in
// the session UI.
#[test]
fn failed_orchestrator_transition_dispatch_becomes_a_visible_destination_error() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-transition-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("transition failure project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Transition Failure Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, input_rx) = test_codex_runtime_handle("orchestrator-transition-failure");
    drop(input_rx);

    let completion_revision = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].runtime = SessionRuntime::Codex(runtime);
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement the panel dragging changes.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
        completion_revision
    };

    state
        .resume_pending_orchestrator_transitions()
        .expect("transition handoff should stay durable even if runtime delivery fails");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner_instance = inner.orchestrator_instances[0]
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner instance should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(
        planner_instance.last_delivered_completion_revision,
        Some(completion_revision)
    );
    assert_eq!(builder.session.status, SessionStatus::Error);
    assert!(builder.queued_prompts.is_empty());
    assert!(builder.session.pending_prompts.is_empty());
    assert!(matches!(
        builder.session.messages.first(),
        Some(Message::Text {
            author: Author::You,
            text,
            ..
        }) if text.contains("Implement the panel dragging changes.")
    ));
    assert!(matches!(
        builder.session.messages.last(),
        Some(Message::Text {
            author: Author::Assistant,
            text,
            ..
        }) if text.contains("failed to queue prompt for Codex session")
    ));
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Pins the cross-instance isolation: with two orchestrator instances
// waiting on transitions and only instance A's delivery failing, the
// resume pass still delivers instance B's prompt to its builder and
// every instance's pending_transitions ends up empty.
// Guards against one instance's runtime failure short-circuiting the
// dispatch loop and starving other pending transitions.
#[test]
fn failed_orchestrator_transition_dispatch_does_not_block_other_instances() {
    let state = test_app_state();
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;

    let project_root_a =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-a-{}", Uuid::new_v4()));
    let project_root_b =
        std::env::temp_dir().join(format!("termal-orchestrator-multi-b-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root_a).expect("first project root should exist");
    fs::create_dir_all(&project_root_b).expect("second project root should exist");

    let project_id_a = create_test_project(&state, &project_root_a, "Multi A");
    let project_id_b = create_test_project(&state, &project_root_b, "Multi B");

    let orchestrator_a = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(project_id_a),
            template: None,
        })
        .expect("first orchestrator instance should be created")
        .orchestrator;
    let orchestrator_b = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id_b),
            template: None,
        })
        .expect("second orchestrator instance should be created")
        .orchestrator;

    let planner_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("first planner session should be mapped")
        .session_id
        .clone();
    let builder_a_session_id = orchestrator_a
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("first builder session should be mapped")
        .session_id
        .clone();
    let planner_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("second planner session should be mapped")
        .session_id
        .clone();
    let builder_b_session_id = orchestrator_b
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("second builder session should be mapped")
        .session_id
        .clone();
    let (failing_runtime, failing_input_rx) =
        test_codex_runtime_handle("orchestrator-transition-failure-a");
    drop(failing_input_rx);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_a_index = inner
            .find_session_index(&planner_a_session_id)
            .expect("first planner session should exist");
        let builder_a_index = inner
            .find_session_index(&builder_a_session_id)
            .expect("first builder session should exist");
        let planner_b_index = inner
            .find_session_index(&planner_b_session_id)
            .expect("second planner session should exist");
        let builder_b_index = inner
            .find_session_index(&builder_b_session_id)
            .expect("second builder session should exist");

        inner.sessions[builder_a_index].runtime = SessionRuntime::Codex(failing_runtime);
        inner.sessions[builder_b_index].session.status = SessionStatus::Active;

        let planner_a_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_a_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_a_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Implement canvas drop zones.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_a_index].session.status = SessionStatus::Idle;

        let planner_b_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_b_index],
            Message::Text {
                attachments: Vec::new(),
                id: planner_b_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Audit the orchestration editor UI.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_b_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_a_session_id,
            completion_revision,
        );
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_b_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("delivery failure in one instance should not block others");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder_a = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_a_session_id)
        .expect("first builder session should exist");
    let builder_b = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_b_session_id)
        .expect("second builder session should exist");

    assert_eq!(builder_a.session.status, SessionStatus::Error);
    assert_eq!(builder_b.session.pending_prompts.len(), 1);
    assert!(
        builder_b.session.pending_prompts[0]
            .text
            .contains("Audit the orchestration editor UI.")
    );
    assert!(
        inner
            .orchestrator_instances
            .iter()
            .all(|instance| instance.pending_transitions.is_empty())
    );
}

// Pins stop_session's non-scheduling invariant: stopping the planner
// writes the "Turn stopped by user." message but does not enqueue an
// orchestrator transition into the builder.
// Guards against treating a user-initiated stop as a turn completion,
// which would hand stale planner output to the next session slot.
#[test]
fn stop_session_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-stop-transition-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("stop project root should exist");
    let project_id = create_test_project(&state, &project_root, "Stop Transition Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "orchestrator-stop-transition".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(test_sleep_child()).unwrap()),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .stop_session(&planner_session_id)
        .expect("stopping the session should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Pins fail_turn_if_runtime_matches: a failed planner turn writes
// "Turn failed: ..." onto the planner and leaves the builder's
// pending_prompts and the instance's pending_transitions empty.
// Guards against propagating partial or failed planner output to
// downstream sessions as if the turn had completed.
#[test]
fn fail_turn_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-fail-turn-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("fail-turn project root should exist");
    let project_id = create_test_project(&state, &project_root, "Fail Turn Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-fail-turn");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .fail_turn_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner turn failed before completion",
        )
        .expect("turn failure should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner turn failed before completion"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Pins mark_turn_error_if_runtime_matches: flipping the planner into
// Error with a preview message does not enqueue a builder prompt or a
// pending transition.
// Guards against the error path accidentally being treated as a
// completion that should fan out to dependent sessions.
#[test]
fn mark_turn_error_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-orchestrator-mark-error-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("mark-error project root should exist");
    let project_id = create_test_project(&state, &project_root, "Mark Error Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-mark-error");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[planner_index].active_turn_start_message_count =
            Some(inner.sessions[planner_index].session.messages.len());
    }

    state
        .mark_turn_error_if_runtime_matches(
            &planner_session_id,
            &runtime_token,
            "planner runtime entered an error state",
        )
        .expect("turn error should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");

    assert_eq!(planner.session.status, SessionStatus::Error);
    assert_eq!(
        planner.session.preview,
        "planner runtime entered an error state"
    );
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Pins the turn-scope slice used by the transition renderer: only
// messages emitted after active_turn_start_message_count are eligible
// as {{result}} input, so stale prior-turn planner output never shows
// up in the builder's rendered prompt.
// Guards against leaking older turn history into the downstream
// session's prompt, which would confuse the receiver with obsolete
// context.
#[test]
fn orchestrator_transition_uses_only_messages_from_the_current_turn() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-current-turn-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("current turn project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Current Turn Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");

        inner.sessions[builder_index].session.status = SessionStatus::Active;
        let old_message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: old_message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Old plan from yesterday.".to_owned(),
                expanded_text: None,
            },
        );
        let turn_start = inner.sessions[planner_index].session.messages.len();
        inner.sessions[planner_index].active_turn_start_message_count = Some(turn_start);
        let current_prompt_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: current_prompt_id,
                timestamp: stamp_now(),
                author: Author::You,
                text: "Current task prompt.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.preview = "Current task prompt.".to_owned();
        inner.sessions[planner_index].session.status = SessionStatus::Idle;

        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .resume_pending_orchestrator_transitions()
        .expect("pending transitions should be delivered");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    assert_eq!(builder.session.pending_prompts.len(), 1);
    assert!(
        !builder.session.pending_prompts[0]
            .text
            .contains("Old plan from yesterday.")
    );
    assert!(
        builder.session.pending_prompts[0]
            .text
            .contains("Use this plan and implement it:")
    );
}

// Pins handle_runtime_exit_if_matches: a crashed planner runtime
// writes "Turn failed: ..." and leaves the builder's pending_prompts
// empty with no pending transition queued on the instance.
// Guards against runtime-exit being treated as a normal turn end that
// would propagate incomplete planner state downstream.
#[test]
fn runtime_exit_does_not_schedule_orchestrator_transitions() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-runtime-exit-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("runtime exit project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Runtime Exit Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();
    let builder_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "builder")
        .expect("builder session should be mapped")
        .session_id
        .clone();
    let (runtime, _input_rx) = test_claude_runtime_handle("orchestrator-runtime-exit");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let builder_index = inner
            .find_session_index(&builder_session_id)
            .expect("builder session should exist");
        inner.sessions[planner_index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[planner_index].session.status = SessionStatus::Active;
        inner.sessions[builder_index].session.status = SessionStatus::Active;
    }

    state
        .handle_runtime_exit_if_matches(
            &planner_session_id,
            &runtime_token,
            Some("planner runtime crashed"),
        )
        .expect("runtime exit should be handled");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let builder = inner
        .sessions
        .iter()
        .find(|record| record.session.id == builder_session_id)
        .expect("builder session should exist");
    let planner = inner
        .sessions
        .iter()
        .find(|record| record.session.id == planner_session_id)
        .expect("planner session should exist");

    assert!(planner.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn failed: planner runtime crashed"
    )));
    assert!(builder.session.pending_prompts.is_empty());
    assert!(
        inner.orchestrator_instances[0]
            .pending_transitions
            .is_empty()
    );
}

// Pins kill_session's orchestrator cleanup: after killing the planner
// session, no orchestrator instance still references it in
// session_instances and no pending transition names it as a source or
// destination.
// Guards against dangling orchestrator pointers to a deleted session
// that would later panic or re-dispatch into a missing slot.
#[test]
fn killing_a_session_prunes_its_orchestrator_links() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-kill-cleanup-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("kill cleanup project root should exist");
    let project_id = state
        .create_project(CreateProjectRequest {
            name: Some("Kill Cleanup Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("project should be created")
        .project_id;
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let orchestrator = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created")
        .orchestrator;

    let planner_session_id = orchestrator
        .session_instances
        .iter()
        .find(|instance| instance.template_session_id == "planner")
        .expect("planner session should be mapped")
        .session_id
        .clone();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let planner_index = inner
            .find_session_index(&planner_session_id)
            .expect("planner session should exist");
        let message_id = inner.next_message_id();
        push_message_on_record(
            &mut inner.sessions[planner_index],
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Plan before kill.".to_owned(),
                expanded_text: None,
            },
        );
        inner.sessions[planner_index].session.status = SessionStatus::Idle;
        let completion_revision = inner.revision + 1;
        schedule_orchestrator_transitions_for_completed_session(
            &mut inner,
            &HashMap::new(),
            &planner_session_id,
            completion_revision,
        );
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .kill_session(&planner_session_id)
        .expect("session should be killed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance
            .session_instances
            .iter()
            .all(|session| session.session_id != planner_session_id)
    }));
    assert!(inner.orchestrator_instances.iter().all(|instance| {
        instance.pending_transitions.iter().all(|pending| {
            pending.source_session_id != planner_session_id
                && pending.destination_session_id != planner_session_id
        })
    }));
}
