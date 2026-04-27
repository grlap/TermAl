// End-to-end HTTP route integration tests. Every case spins up the real
// `axum::Router` returned by `app_router(state)`, fires an actual HTTP
// request through `tower::ServiceExt`, and parses the real JSON or SSE
// response bytes — no handler is called directly.
//
// Contrast with the domain-specific submodules (sessions, orchestrators,
// workspaces, ...), which test production logic via direct `AppState`
// method calls. This module instead confirms the router wires request
// shapes, extractors, and response types (`StatusCode`,
// `CreateSessionResponse`, `SessionResponse`, `StateResponse`) correctly.
// SSE cases use `collect_sse_events` to drain the event stream and verify
// initial-state + delta ordering. The Codex thread action routes proxy to
// real `SharedCodex` runtime calls; tests stub those via fake JSON-RPC
// responses on a test TCP server driven by `test_shared_codex_runtime`.
// Production surfaces: `app_router` plus the `create_session`,
// `get_session`, `state_events`, `archive_codex_thread`, `unarchive_codex_thread`,
// `rollback_codex_thread`, `fork_codex_thread` handlers in src/api.rs.

use super::*;

struct HttpRouteTestFiles {
    persistence_path: std::path::PathBuf,
    orchestrator_templates_path: std::path::PathBuf,
}

impl HttpRouteTestFiles {
    fn capture(state: &AppState) -> Self {
        Self {
            persistence_path: state.persistence_path.as_ref().clone(),
            orchestrator_templates_path: state.orchestrator_templates_path.as_ref().clone(),
        }
    }
}

impl Drop for HttpRouteTestFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.persistence_path);
        let _ = fs::remove_file(&self.orchestrator_templates_path);
    }
}

// Pins `POST /api/sessions` — asserts 201 Created with a
// `CreateSessionResponse` whose `session` field carries the normalized
// workdir and default `Agent::Codex`. Guards against handler regressions
// that drop the session payload or return the wrong status code.
#[tokio::test]
async fn create_session_route_returns_created_response() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let initial_session_count = state.snapshot().sessions.len();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Created Session",
        "workdir": "/tmp"
    }))
    .expect("create session route body should serialize");
    let (status, response_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/sessions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response_body["session"]["messageCount"], 0);
    let response: CreateSessionResponse =
        serde_json::from_value(response_body).expect("create session response should decode");
    let created_session = &response.session;
    assert_eq!(response.session_id, created_session.id);
    assert!(response.revision > 0);
    assert_eq!(state.snapshot().sessions.len(), initial_session_count + 1);
    assert_eq!(created_session.name, "Route Created Session");
    let expected_workdir = resolve_session_workdir("/tmp").expect("route workdir should normalize");
    assert_eq!(created_session.workdir, expected_workdir);
    assert_eq!(created_session.agent, Agent::Codex);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/sessions/{id}` — asserts 200 OK with a `SessionResponse`
// carrying the full `Session` and the current `revision`, without the
// caller needing a full `StateResponse` snapshot. Guards against the
// single-session handler drifting from the state snapshot revision.
#[tokio::test]
async fn get_session_route_returns_full_session() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Route Session Detail".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;

    let (status, response): (StatusCode, SessionResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.session.id, session_id);
    assert_eq!(response.session.name, "Route Session Detail");
    assert_eq!(response.revision, state.snapshot().revision);
    // `serverInstanceId` is carried on `SessionResponse` so
    // `adoptFetchedSession` can detect a server restart mid-hydration
    // and accept a revision downgrade. The per-process id must be
    // non-empty (the frontend treats `""` as "unknown / older server"
    // and cannot trigger the restart branch on it).
    assert_eq!(response.server_instance_id, state.server_instance_id);
    assert!(!response.server_instance_id.is_empty());
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `messageCount` on full snapshot-bearing routes. The count is computed
// from the transcript at wire-projection time so reconnect/state adoption can
// keep summary metadata without waiting for the next delta.
#[tokio::test]
async fn snapshot_bearing_routes_include_message_count() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            name: Some("Counted Session".to_owned()),
            agent: None,
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("session should be created");
    let session_id = created.session_id;
    for text in ["First counted message", "Second counted message"] {
        let message_id = state.allocate_message_id();
        state
            .push_message(
                &session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: text.to_owned(),
                    expanded_text: None,
                },
            )
            .expect("message should be recorded");
    }
    let app = app_router(state.clone());

    let (state_status, state_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/state")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(state_status, StatusCode::OK);
    let state_session = state_body["sessions"]
        .as_array()
        .expect("state sessions should be an array")
        .iter()
        .find(|session| session["id"] == session_id)
        .expect("state should include counted session");
    assert_eq!(state_session["messageCount"], 2);
    assert_eq!(state_session["messagesLoaded"], false);
    assert!(
        state_session["messages"]
            .as_array()
            .expect("state session messages should stay adapter-compatible")
            .is_empty()
    );

    let (session_status, session_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{session_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(session_status, StatusCode::OK);
    assert_eq!(session_body["session"]["id"], session_id);
    assert_eq!(session_body["session"]["messageCount"], 2);
    assert_eq!(session_body["session"]["messagesLoaded"], true);
    assert_eq!(
        session_body["session"]["messages"]
            .as_array()
            .expect("session response should include full transcript")
            .len(),
        2
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `POST /api/sessions/{id}/messages`: prompt start must not advance the
// UI with a metadata-only session. The HTTP response carries the target
// transcript, and SSE carries a narrow `messageCreated` delta for other
// subscribers.
#[tokio::test]
async fn send_message_route_returns_full_session_and_publishes_prompt_delta() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("send-message-route-full-session");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Visible route prompt"
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
        .expect("prompt session should be present");
    assert!(session.messages_loaded);
    assert_eq!(session.message_count, 1);
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        &session.messages[0],
        Message::Text {
            author: Author::You,
            text,
            ..
        } if text == "Visible route prompt"
    ));

    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let payload = delta_events
        .try_recv()
        .expect("prompt start should publish a message delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::MessageCreated {
            revision,
            session_id: delta_session_id,
            message_index,
            message_count,
            message,
            status,
            ..
        } => {
            assert_eq!(revision, response.revision);
            assert_eq!(delta_session_id, session_id);
            assert_eq!(message_index, 0);
            assert_eq!(message_count, 1);
            assert_eq!(status, SessionStatus::Active);
            assert!(matches!(
                message,
                Message::Text {
                    author: Author::You,
                    text,
                    ..
                } if text == "Visible route prompt"
            ));
        }
        _ => panic!("expected prompt messageCreated delta"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn next_delta_event(delta_events: &mut broadcast::Receiver<String>) -> DeltaEvent {
    let payload = delta_events
        .try_recv()
        .expect("delta event should be published");
    serde_json::from_str(&payload).expect("delta event should decode")
}

// Pins `message_count` on representative local streaming delta emitters. The
// frontend uses this metadata to reconcile metadata-first snapshots with
// transcript deltas, so Rust-side wire regressions must fail here instead of
// only in frontend fixture tests.
#[test]
fn local_streaming_delta_events_include_message_count() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Claude);
    let mut delta_events = state.subscribe_delta_events();

    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "text-1".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: String::new(),
                expanded_text: None,
            },
        )
        .expect("streaming text placeholder should be recorded");
    let _ = next_delta_event(&mut delta_events);

    state
        .append_text_delta(&session_id, "text-1", "hello")
        .expect("text delta should append");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::TextDelta { message_count, .. } => assert_eq!(message_count, 1),
        _ => panic!("expected TextDelta"),
    }

    state
        .replace_text_message(&session_id, "text-1", "hello final")
        .expect("text replacement should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::TextReplace { message_count, .. } => assert_eq!(message_count, 1),
        _ => panic!("expected TextReplace"),
    }

    state
        .upsert_command_message(
            &session_id,
            "command-1",
            "pwd",
            "",
            CommandStatus::Running,
        )
        .expect("command message should be created");
    let _ = next_delta_event(&mut delta_events);
    state
        .upsert_command_message(
            &session_id,
            "command-1",
            "pwd",
            "/tmp",
            CommandStatus::Success,
        )
        .expect("command update should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::CommandUpdate { message_count, .. } => assert_eq!(message_count, 2),
        _ => panic!("expected CommandUpdate"),
    }

    let running_agent = ParallelAgentProgress {
        detail: Some("Checking files".to_owned()),
        id: "agent-1".to_owned(),
        status: ParallelAgentStatus::Running,
        title: "Reviewer".to_owned(),
    };
    state
        .upsert_parallel_agents_message(&session_id, "agents-1", vec![running_agent])
        .expect("parallel agents message should be created");
    let _ = next_delta_event(&mut delta_events);
    state
        .upsert_parallel_agents_message(
            &session_id,
            "agents-1",
            vec![ParallelAgentProgress {
                detail: Some("Done".to_owned()),
                id: "agent-1".to_owned(),
                status: ParallelAgentStatus::Completed,
                title: "Reviewer".to_owned(),
            }],
        )
        .expect("parallel agents update should publish");
    match next_delta_event(&mut delta_events) {
        DeltaEvent::ParallelAgentsUpdate { message_count, .. } => {
            assert_eq!(message_count, 3)
        }
        _ => panic!("expected ParallelAgentsUpdate"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Tests that the empty SSE fallback payload carries an explicit fallback marker.
#[test]
fn empty_state_events_payload_carries_explicit_fallback_marker() {
    let payload: Value = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("SSE fallback payload should parse");
    assert_eq!(payload["_sseFallback"], true);
    assert_eq!(payload["revision"], 0);
    assert!(payload.get("preferences").is_some());
    assert!(payload.get("sessions").is_some());

    let decoded: StateEventPayload = serde_json::from_str(EMPTY_STATE_EVENTS_PAYLOAD.as_str())
        .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 0);
}

// Tests that fallback SSE payloads can carry the recovered revision.
#[test]
fn fallback_state_events_payload_uses_supplied_revision() {
    let decoded: StateEventPayload = serde_json::from_str(
        &fallback_state_events_payload(42).expect("fallback payload should encode"),
    )
    .expect("fallback payload should decode as a state event payload");
    assert!(decoded.sse_fallback);
    assert_eq!(decoded.state.revision, 42);
}

// Pins `GET /api/events` (SSE) — asserts the `text/event-stream`
// content-type, that the first frame is a `state` event carrying a
// `StateResponse`, and that a subsequent `push_message` produces a
// live `delta` event with `type: "messageCreated"`. Guards against SSE
// frame ordering or naming regressions.
#[tokio::test]
async fn state_events_route_streams_initial_state_and_live_deltas() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .expect("SSE route should set a content type");
    assert!(content_type.starts_with("text/event-stream"));
    let mut body = Box::pin(response.into_body().into_data_stream());
    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(
        initial_state
            .sessions
            .iter()
            .any(|session| session.id == session_id)
    );
    let initial_session = initial_state
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("initial state should include test session");
    assert!(!initial_session.messages_loaded);
    assert!(initial_session.messages.is_empty());
    let message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Live delta".to_owned(),
                expanded_text: None,
            },
        )
        .expect("delta message should be recorded");
    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "messageCreated");
    assert_eq!(delta["sessionId"], session_id);
    assert_eq!(delta["messageId"], message_id);
    assert_eq!(delta["messageCount"], 1);
    assert_eq!(delta["message"]["type"], "text");
    assert_eq!(delta["message"]["text"], "Live delta");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/events` + `PUT/DELETE /api/workspaces/{id}` — asserts
// every workspace-layout mutation (create, update, delete) republishes
// a fresh `state` SSE frame whose `workspaces` summaries reflect the
// new revision and control-panel side. Guards against layout mutations
// that persist but fail to refresh the SSE stream.
#[tokio::test]
async fn state_events_route_streams_workspace_layout_summary_updates() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.workspaces.is_empty());

    let create_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "workspace": { "panes": [] }
    }))
    .expect("workspace layout body should serialize");
    let (save_status, _save_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(create_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(save_status, StatusCode::OK);

    let saved_event = next_sse_event(&mut body).await;
    let (saved_name, saved_data) = parse_sse_event(&saved_event);
    assert_eq!(saved_name, "state");
    let saved_state: StateResponse =
        serde_json::from_str(&saved_data).expect("saved SSE payload should parse");
    assert_eq!(saved_state.workspaces.len(), 1);
    assert_eq!(saved_state.workspaces[0].id, "workspace-live");
    assert_eq!(saved_state.workspaces[0].revision, 1);
    assert_eq!(
        saved_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Left
    );

    let update_layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "right",
        "workspace": {
            "panes": [
                {
                    "id": "pane-1",
                    "tabs": []
                }
            ]
        }
    }))
    .expect("updated workspace layout body should serialize");
    let (update_status, _update_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-live")
            .header("content-type", "application/json")
            .body(Body::from(update_layout_body))
            .unwrap(),
    )
    .await;
    assert_eq!(update_status, StatusCode::OK);

    let updated_event = next_sse_event(&mut body).await;
    let (updated_name, updated_data) = parse_sse_event(&updated_event);
    assert_eq!(updated_name, "state");
    let updated_state: StateResponse =
        serde_json::from_str(&updated_data).expect("updated SSE payload should parse");
    assert_eq!(updated_state.workspaces.len(), 1);
    assert_eq!(updated_state.workspaces[0].id, "workspace-live");
    assert_eq!(updated_state.workspaces[0].revision, 2);
    assert_eq!(
        updated_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Right
    );

    let (delete_status, _delete_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/workspace-live")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_status, StatusCode::OK);

    let deleted_event = next_sse_event(&mut body).await;
    let (deleted_name, deleted_data) = parse_sse_event(&deleted_event);
    assert_eq!(deleted_name, "state");
    let deleted_state: StateResponse =
        serde_json::from_str(&deleted_data).expect("deleted SSE payload should parse");
    assert!(deleted_state.workspaces.is_empty());
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `GET /api/events` — asserts that creating an orchestrator
// instance republishes the full `state` frame (including the new
// instance and its session fan-out), and that pausing it emits a
// `delta` frame with `type: "orchestratorsUpdated"` listing the
// referenced sessions. Guards against orchestrator SSE routing
// that drops status transitions or session references.
#[tokio::test]
async fn state_events_route_streams_orchestrator_creation_state_and_live_orchestrator_deltas() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let project_root = std::env::temp_dir().join(format!(
        "termal-orchestrator-events-route-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("events project root should exist");
    let project_id = create_test_project(&state, &project_root, "Events Orchestrator Project");
    let template = state
        .create_orchestrator_template(sample_orchestrator_template_draft())
        .expect("template should be created")
        .template;
    let app = app_router(state.clone());
    let response = request_response(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/events")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());

    let initial_event = next_sse_event(&mut body).await;
    let (initial_name, initial_data) = parse_sse_event(&initial_event);
    assert_eq!(initial_name, "state");
    let initial_state: StateResponse =
        serde_json::from_str(&initial_data).expect("initial SSE payload should parse");
    assert!(initial_state.orchestrators.is_empty());

    let created = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id,
            project_id: Some(project_id),
            template: None,
        })
        .expect("orchestrator instance should be created");
    let instance_id = created.orchestrator.id.clone();
    let created_session_ids = created
        .orchestrator
        .session_instances
        .iter()
        .map(|instance| instance.session_id.clone())
        .collect::<Vec<_>>();

    let created_event = next_sse_event(&mut body).await;
    let (created_name, created_data) = parse_sse_event(&created_event);
    assert_eq!(created_name, "state");
    let created_state: StateResponse =
        serde_json::from_str(&created_data).expect("create SSE payload should parse");
    let created_orchestrator = created_state
        .orchestrators
        .iter()
        .find(|instance| instance.id == instance_id)
        .expect("create SSE state should include the orchestrator instance");
    assert_eq!(
        created_orchestrator.status,
        OrchestratorInstanceStatus::Running
    );
    for session_id in &created_session_ids {
        assert!(
            created_state
                .sessions
                .iter()
                .any(|session| session.id == *session_id),
            "create SSE state should include orchestrator session {session_id}"
        );
    }

    state
        .pause_orchestrator_instance(&instance_id)
        .expect("pause route should update orchestrator state");

    let delta_event = next_sse_event(&mut body).await;
    let (delta_name, delta_data) = parse_sse_event(&delta_event);
    assert_eq!(delta_name, "delta");
    let delta: Value = serde_json::from_str(&delta_data).expect("delta SSE payload should parse");
    assert_eq!(delta["type"], "orchestratorsUpdated");
    assert!(
        delta["orchestrators"]
            .as_array()
            .is_some_and(|instances| instances.iter().any(|instance| {
                instance["id"] == Value::String(instance_id.clone())
                    && instance["status"] == Value::String("paused".to_owned())
            }))
    );
    let delta_session_ids = delta["sessions"]
        .as_array()
        .expect("orchestrator delta should include referenced sessions")
        .iter()
        .map(|session| {
            session["id"]
                .as_str()
                .expect("delta session should include an ID")
                .to_owned()
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        delta_session_ids,
        created_session_ids.into_iter().collect::<HashSet<_>>()
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins `POST /api/sessions/{id}/codex/thread/{archive,unarchive,rollback}`
// — asserts each action returns 200 OK with a `StateResponse` whose
// session reflects the new `codex_thread_state`, and that rollback
// replaces stale local messages with the freshly-returned thread
// history. Guards against handler drift in the JSON-RPC thread action
// routes and their session-state synchronisation.
#[tokio::test]
async fn codex_thread_action_routes_update_session_state() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "stale local message".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-actions");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive", "thread/rollback"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    if method == "thread/rollback" {
                        assert_eq!(params["numTurns"], 2);
                        let _ = response_tx.send(Ok(json!({
                            "thread": {
                                "preview": "Rolled back preview",
                                "turns": [
                                    {
                                        "id": "turn-rollback",
                                        "status": "completed",
                                        "items": [
                                            {
                                                "id": "rollback-user",
                                                "type": "userMessage",
                                                "content": [
                                                    {
                                                        "type": "text",
                                                        "text": "Current diff state"
                                                    }
                                                ]
                                            },
                                            {
                                                "id": "rollback-agent",
                                                "type": "agentMessage",
                                                "text": "Rollback synced."
                                            }
                                        ]
                                    }
                                ]
                            }
                        })));
                        continue;
                    }
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let app = app_router(state.clone());
    let (archive_status, archive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/archive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(archive_status, StatusCode::OK);
    let archived_session = archive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    let (unarchive_status, unarchive_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/unarchive"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(unarchive_status, StatusCode::OK);
    let restored_session = unarchive_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (rollback_status, rollback_response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":2}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(rollback_status, StatusCode::OK);
    let rollback_session = rollback_response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(!rollback_session.messages_loaded);
    assert!(rollback_session.messages.is_empty());
    let rollback_session = state
        .get_session(&session_id)
        .expect("rolled back session should hydrate")
        .session;
    assert!(matches!(
        rollback_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Current diff state"
    ));
    assert!(matches!(
        rollback_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Rollback synced."
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. }
            if title == "Archived Codex thread"
                || title == "Restored Codex thread"
                || title == "Rolled back Codex thread")
    ));
    assert!(!rollback_session.messages.iter().any(
        |message| matches!(message, Message::Text { text, .. } if text == "stale local message")
    ));
}

// Pins `POST /api/sessions/{id}/codex/thread/rollback` — asserts that
// when Codex returns no `turns`, the handler still replies 200 OK with
// a `StateResponse`, preserves the existing local history, and appends
// a `Markdown` notice explaining the missing thread payload. Guards
// against the fallback branch regressing into a 500 or silent data loss.
#[tokio::test]
async fn codex_thread_rollback_route_falls_back_when_history_is_unavailable() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "local history".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, input_rx, _process) =
        test_shared_codex_runtime("shared-codex-route-rollback-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex rollback command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/rollback");
                assert_eq!(params["threadId"], "thread-live");
                assert_eq!(params["numTurns"], 1);
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "preview": "Fallback preview"
                    }
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state.clone());
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/codex/thread/rollback"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"numTurns":1}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(!session.messages_loaded);
    assert!(session.messages.is_empty());
    let session = state
        .get_session(&session_id)
        .expect("rolled back fallback session should hydrate")
        .session;
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "local history"
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Rolled back Codex thread"
                && markdown.contains("Codex did not return the updated thread history")
    ));
}

// Pins `POST /api/sessions/{id}/codex/thread/fork` — asserts 201 Created
// with a `CreateSessionResponse` whose `session` carries the forked
// `external_session_id`, `CodexThreadState::Active`, and the hydrated
// user/agent messages rebuilt from the fake `thread/fork` JSON-RPC
// response. Guards against fork regressions that drop thread metadata
// or return the wrong status code.
#[tokio::test]
async fn codex_thread_fork_route_returns_created_response() {
    let state = test_app_state();
    let _files = HttpRouteTestFiles::capture(&state);
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Route Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-route-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-forked",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "fork-user",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Fork context"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "fork-agent",
                                        "type": "agentMessage",
                                        "text": "Ready to continue."
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let app = app_router(state);
    let (status, response): (StatusCode, CreateSessionResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{}/codex/thread/fork",
                created.session_id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let forked_session = &response.session;
    assert_eq!(response.session_id, forked_session.id);
    assert!(response.revision > 0);
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. }) if text == "Fork context"
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "Ready to continue."
    ));
}
