use super::*;

fn test_session_with_two_messages(state: &AppState) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner.create_session(
        Agent::Codex,
        Some("Marked".to_owned()),
        "/tmp".to_owned(),
        None,
        None,
    );
    let session_id = record.session.id.clone();
    let index = inner
        .find_session_index(&session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "message-1".to_owned(),
            timestamp: stamp_now(),
            author: Author::You,
            text: "First".to_owned(),
            expanded_text: None,
        },
    );
    push_message_on_record(
        record,
        Message::Text {
            attachments: Vec::new(),
            id: "message-2".to_owned(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Second".to_owned(),
            expanded_text: None,
        },
    );
    state.commit_locked(&mut inner).unwrap();
    session_id
}

fn test_marker_response(marker: ConversationMarker, revision: u64) -> ConversationMarkerResponse {
    ConversationMarkerResponse {
        marker,
        revision,
        server_instance_id: "remote-instance".to_owned(),
        session_mutation_stamp: Some(11),
    }
}

fn test_marker(
    id: &str,
    session_id: &str,
    message_id: &str,
    kind: ConversationMarkerKind,
) -> ConversationMarker {
    ConversationMarker {
        id: id.to_owned(),
        session_id: session_id.to_owned(),
        kind,
        name: "Remote marker".to_owned(),
        body: None,
        color: "#3b82f6".to_owned(),
        message_id: message_id.to_owned(),
        message_index_hint: 0,
        end_message_id: None,
        end_message_index_hint: None,
        created_at: "2026-05-01 10:00:00".to_owned(),
        updated_at: "2026-05-01 10:00:00".to_owned(),
        created_by: ConversationMarkerAuthor::User,
    }
}

fn marker_session_stamps(state: &AppState, session_id: &str) -> (u64, u64) {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    (
        inner.last_mutation_stamp,
        inner.sessions[index].mutation_stamp,
    )
}

fn remote_proxy_session_with_message(state: &AppState, remote: &RemoteConfig) -> String {
    let local_project_id = create_test_remote_project(
        state,
        remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.messages = vec![Message::Text {
        attachments: Vec::new(),
        id: "remote-message-1".to_owned(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        text: "Remote message".to_owned(),
        expanded_text: None,
    }];
    remote_session.messages_loaded = true;
    remote_session.message_count = 1;

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let local_session_id = upsert_remote_proxy_session_record(
        &mut inner,
        &remote.id,
        &remote_session,
        Some(local_project_id),
    );
    state
        .commit_locked(&mut inner)
        .expect("remote proxy session should persist");
    local_session_id
}

fn spawn_remote_marker_proxy_server(
    expected_request_prefix: &'static str,
    response_status: StatusCode,
    response_body: String,
) -> (
    u16,
    Arc<Mutex<Vec<(String, String)>>>,
    std::thread::JoinHandle<()>,
) {
    let requests = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote marker proxy listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push((request.request_line.clone(), request.body.clone()));

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request.request_line.starts_with(expected_request_prefix) {
                write_test_http_response(
                    &mut stream,
                    response_status,
                    "application/json",
                    &response_body,
                );
                continue;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    (port, requests, server)
}

fn spawn_remote_marker_create_server(
    response: ConversationMarkerResponse,
) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let response_body = serde_json::to_string(&response).expect("marker response should encode");
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote marker listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true}"#,
                );
                continue;
            }

            if request
                .request_line
                .starts_with("POST /api/sessions/remote-session-1/markers ")
            {
                write_test_http_response(
                    &mut stream,
                    StatusCode::CREATED,
                    "application/json",
                    &response_body,
                );
                continue;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    (port, requests, server)
}

#[test]
fn conversation_marker_crud_updates_session_and_publishes_deltas() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let mut delta_events = state.subscribe_delta_events();

    let created = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Decision,
                name: " Decision point ".to_owned(),
                body: Some(" Keep the overview rail. ".to_owned()),
                color: "#3B82F6".to_owned(),
                message_id: "message-1".to_owned(),
                end_message_id: None,
            },
        )
        .expect("marker should be created");
    assert_eq!(created.marker.name, "Decision point");
    assert_eq!(
        created.marker.body.as_deref(),
        Some("Keep the overview rail.")
    );
    assert_eq!(created.marker.color, "#3b82f6");
    assert_eq!(created.marker.message_index_hint, 0);

    let payload = delta_events
        .try_recv()
        .expect("create marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerCreated {
            session_id: delta_session_id,
            marker,
            session_mutation_stamp,
            ..
        } => {
            assert_eq!(delta_session_id, session_id);
            assert_eq!(marker.id, created.marker.id);
            assert!(session_mutation_stamp.is_some());
        }
        _ => panic!("expected ConversationMarkerCreated delta"),
    }

    let listed = state
        .list_conversation_markers(&session_id)
        .expect("markers should list");
    assert_eq!(listed.markers.len(), 1);

    let updated = state
        .update_conversation_marker(
            &session_id,
            &created.marker.id,
            UpdateConversationMarkerRequest {
                kind: Some(ConversationMarkerKind::Checkpoint),
                name: Some("Checkpoint".to_owned()),
                body: Some(None),
                color: Some("#22c55e".to_owned()),
                message_id: None,
                end_message_id: Some(Some("message-2".to_owned())),
            },
        )
        .expect("marker should update");
    assert_eq!(updated.marker.kind, ConversationMarkerKind::Checkpoint);
    assert_eq!(updated.marker.body, None);
    assert_eq!(updated.marker.end_message_id.as_deref(), Some("message-2"));
    assert_eq!(updated.marker.end_message_index_hint, Some(1));

    let payload = delta_events
        .try_recv()
        .expect("update marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerUpdated { marker, .. } => {
            assert_eq!(marker.name, "Checkpoint");
        }
        _ => panic!("expected ConversationMarkerUpdated delta"),
    }

    let deleted = state
        .delete_conversation_marker(&session_id, &created.marker.id)
        .expect("marker should delete");
    assert_eq!(deleted.marker_id, created.marker.id);

    let payload = delta_events
        .try_recv()
        .expect("delete marker should publish a delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
    match delta {
        DeltaEvent::ConversationMarkerDeleted { marker_id, .. } => {
            assert_eq!(marker_id, created.marker.id);
        }
        _ => panic!("expected ConversationMarkerDeleted delta"),
    }

    let listed = state
        .list_conversation_markers(&session_id)
        .expect("markers should list after delete");
    assert!(listed.markers.is_empty());
}

#[tokio::test]
async fn marker_routes_create_list_patch_clear_and_delete() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let app = app_router(state);
    let encoded_session_id = encode_uri_component(&session_id);

    let (status, created): (StatusCode, ConversationMarkerResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{encoded_session_id}/markers"))
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "kind": "decision",
                    "name": "Route marker",
                    "body": "route body",
                    "color": "#3B82F6",
                    "messageId": "message-1",
                    "endMessageId": "message-2"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created.marker.body.as_deref(), Some("route body"));
    assert_eq!(created.marker.end_message_id.as_deref(), Some("message-2"));

    let (status, listed): (StatusCode, ConversationMarkersResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{encoded_session_id}/markers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.markers.len(), 1);
    assert_eq!(listed.markers[0].id, created.marker.id);

    let encoded_marker_id = encode_uri_component(&created.marker.id);
    let (status, updated): (StatusCode, ConversationMarkerResponse) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!(
                "/api/sessions/{encoded_session_id}/markers/{encoded_marker_id}"
            ))
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "body": null,
                    "endMessageId": null
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated.marker.body, None);
    assert_eq!(updated.marker.end_message_id, None);
    assert_eq!(updated.marker.end_message_index_hint, None);

    let (status, deleted): (StatusCode, DeleteConversationMarkerResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/sessions/{encoded_session_id}/markers/{encoded_marker_id}"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted.marker_id, created.marker.id);

    let (status, listed): (StatusCode, ConversationMarkersResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/sessions/{encoded_session_id}/markers"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed.markers.is_empty());
}

#[test]
fn conversation_marker_rejects_missing_message_anchor() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);

    let err = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Bug,
                name: "Missing anchor".to_owned(),
                body: None,
                color: "#ef4444".to_owned(),
                message_id: "missing-message".to_owned(),
                end_message_id: None,
            },
        )
        .expect_err("missing message anchor should be rejected");

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("message id was not found"));
}

#[test]
fn conversation_marker_validation_errors_do_not_bump_mutation_stamp() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);

    let (before_last_stamp, before_session_stamp) = marker_session_stamps(&state, &session_id);

    let err = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Bug,
                name: "Missing anchor".to_owned(),
                body: None,
                color: "#ef4444".to_owned(),
                message_id: "missing-message".to_owned(),
                end_message_id: None,
            },
        )
        .expect_err("missing message anchor should be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    let (after_last_stamp, after_session_stamp) = marker_session_stamps(&state, &session_id);
    assert_eq!(after_last_stamp, before_last_stamp);
    assert_eq!(after_session_stamp, before_session_stamp);
}

#[test]
fn conversation_marker_update_and_delete_errors_do_not_bump_mutation_stamp() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let created = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Decision,
                name: "Decision point".to_owned(),
                body: None,
                color: "#3b82f6".to_owned(),
                message_id: "message-1".to_owned(),
                end_message_id: None,
            },
        )
        .expect("marker should create");
    let baseline_stamps = marker_session_stamps(&state, &session_id);

    let err = state
        .update_conversation_marker(
            &session_id,
            &created.marker.id,
            UpdateConversationMarkerRequest {
                kind: None,
                name: None,
                body: None,
                color: None,
                message_id: Some("missing-message".to_owned()),
                end_message_id: None,
            },
        )
        .expect_err("missing update anchor should be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(marker_session_stamps(&state, &session_id), baseline_stamps);

    let err = state
        .delete_conversation_marker(&session_id, "marker-missing")
        .expect_err("missing marker delete should be rejected");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert_eq!(marker_session_stamps(&state, &session_id), baseline_stamps);
}

#[test]
fn marker_patch_deserializes_explicit_null_as_clear_operation() {
    let request: UpdateConversationMarkerRequest = serde_json::from_value(json!({
        "body": null,
        "endMessageId": null
    }))
    .expect("patch request should deserialize");

    assert_eq!(request.body, Some(None));
    assert_eq!(request.end_message_id, Some(None));
    assert!(update_conversation_marker_request_has_changes(&request));

    let request: UpdateConversationMarkerRequest =
        serde_json::from_value(json!({})).expect("empty patch should deserialize");
    assert_eq!(request.body, None);
    assert_eq!(request.end_message_id, None);
    assert!(!update_conversation_marker_request_has_changes(&request));
}

#[test]
fn marker_request_deserialization_rejects_unknown_kind_typos() {
    let err = serde_json::from_value::<CreateConversationMarkerRequest>(json!({
        "kind": "decison",
        "name": "Typo",
        "color": "#3b82f6",
        "messageId": "message-1"
    }))
    .expect_err("unknown marker kind should be rejected");

    assert!(
        err.to_string()
            .contains("unsupported conversation marker kind `decison`"),
        "unexpected error: {err}"
    );
}

#[test]
fn remote_session_snapshot_localizes_marker_session_ids() {
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.markers = vec![test_marker(
        "marker-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Decision,
    )];

    let session = localize_remote_session(
        "local-session-9",
        Some("project-9".to_owned()),
        &remote_session,
    );

    assert_eq!(session.id, "local-session-9");
    assert_eq!(session.markers[0].session_id, "local-session-9");
}

#[test]
fn remote_backed_marker_create_proxies_to_owner_and_localizes_response() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let remote_marker = test_marker(
        "marker-remote-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );
    let (port, requests, server) =
        spawn_remote_marker_create_server(test_marker_response(remote_marker, 7));
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .create_conversation_marker(
            &local_session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Checkpoint,
                name: "Checkpoint".to_owned(),
                body: None,
                color: "#3b82f6".to_owned(),
                message_id: "remote-message-1".to_owned(),
                end_message_id: None,
            },
        )
        .expect("remote marker create should proxy");

    assert_eq!(response.marker.session_id, local_session_id);
    assert_eq!(response.marker.id, "marker-remote-1");
    let local_markers = state
        .list_conversation_markers(&local_session_id)
        .expect("local marker list should read");
    assert_eq!(local_markers.markers.len(), 1);
    assert_eq!(local_markers.markers[0].session_id, local_session_id);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("POST /api/sessions/remote-session-1/markers ")),
        "expected remote marker create request, saw {request_lines:?}"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_backed_marker_update_proxies_patch_and_localizes_response() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let mut remote_marker = test_marker(
        "marker-remote-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Bug,
    );
    remote_marker.name = "Updated marker".to_owned();
    remote_marker.body = None;
    let response_body =
        serde_json::to_string(&test_marker_response(remote_marker, 8)).expect("response encodes");
    let (port, requests, server) = spawn_remote_marker_proxy_server(
        "PATCH /api/sessions/remote-session-1/markers/marker-remote-1 ",
        StatusCode::OK,
        response_body,
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .update_conversation_marker(
            &local_session_id,
            "marker-remote-1",
            UpdateConversationMarkerRequest {
                kind: Some(ConversationMarkerKind::Bug),
                name: Some(" Updated marker ".to_owned()),
                body: Some(None),
                color: Some("#ef4444".to_owned()),
                message_id: None,
                end_message_id: Some(None),
            },
        )
        .expect("remote marker update should proxy");

    assert_eq!(response.marker.session_id, local_session_id);
    assert_eq!(response.marker.id, "marker-remote-1");
    assert_eq!(response.marker.name, "Updated marker");
    let local_markers = state
        .list_conversation_markers(&local_session_id)
        .expect("local marker list should read");
    assert_eq!(local_markers.markers.len(), 1);
    assert_eq!(local_markers.markers[0].session_id, local_session_id);

    let requests = requests.lock().expect("requests mutex poisoned").clone();
    let patch = requests
        .iter()
        .find(|(line, _)| {
            line.starts_with("PATCH /api/sessions/remote-session-1/markers/marker-remote-1 ")
        })
        .unwrap_or_else(|| panic!("expected remote marker PATCH request, saw {requests:?}"));
    let body: Value = serde_json::from_str(&patch.1).expect("PATCH body should decode");
    assert_eq!(body["kind"], "bug");
    assert_eq!(body["name"], " Updated marker ");
    assert_eq!(body["body"], Value::Null);
    assert_eq!(body["endMessageId"], Value::Null);
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_backed_marker_delete_proxies_delete_and_localizes_response() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ConversationMarkerCreated {
                revision: 7,
                session_id: "remote-session-1".to_owned(),
                marker: test_marker(
                    "marker-remote-1",
                    "remote-session-1",
                    "remote-message-1",
                    ConversationMarkerKind::Checkpoint,
                ),
                session_mutation_stamp: Some(11),
            },
        )
        .expect("remote marker create should seed local marker");
    let response_body = serde_json::to_string(&DeleteConversationMarkerResponse {
        marker_id: "marker-remote-1".to_owned(),
        revision: 8,
        server_instance_id: "remote-instance".to_owned(),
        session_mutation_stamp: Some(12),
    })
    .expect("response encodes");
    let (port, requests, server) = spawn_remote_marker_proxy_server(
        "DELETE /api/sessions/remote-session-1/markers/marker-remote-1 ",
        StatusCode::OK,
        response_body,
    );
    insert_test_remote_connection(&state, &remote, port);

    let response = state
        .delete_conversation_marker(&local_session_id, "marker-remote-1")
        .expect("remote marker delete should proxy");

    assert_eq!(response.marker_id, "marker-remote-1");
    assert!(response.session_mutation_stamp.is_some());
    let local_markers = state
        .list_conversation_markers(&local_session_id)
        .expect("local marker list should read");
    assert!(local_markers.markers.is_empty());

    let requests = requests.lock().expect("requests mutex poisoned").clone();
    let delete = requests
        .iter()
        .find(|(line, _)| {
            line.starts_with("DELETE /api/sessions/remote-session-1/markers/marker-remote-1 ")
        })
        .unwrap_or_else(|| panic!("expected remote marker DELETE request, saw {requests:?}"));
    assert!(
        delete.1.is_empty(),
        "DELETE request should not carry a body"
    );
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn marker_patch_request_deserialization_rejects_unknown_kind_typos() {
    let err = serde_json::from_value::<UpdateConversationMarkerRequest>(json!({
        "kind": "decison"
    }))
    .expect_err("unknown marker kind should be rejected");

    assert!(
        err.to_string()
            .contains("unsupported conversation marker kind `decison`"),
        "unexpected error: {err}"
    );
}

#[test]
fn remote_marker_proxy_response_carries_localized_stamp_and_dedupes_replayed_event() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let target = state
        .remote_session_target(&local_session_id)
        .expect("remote target lookup should succeed")
        .expect("session should be remote-backed");
    let remote_marker = test_marker(
        "marker-remote-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );
    let mut delta_events = state.subscribe_delta_events();

    let response = state
        .apply_remote_marker_response(
            target,
            ConversationMarkerResponse {
                marker: remote_marker.clone(),
                revision: 7,
                server_instance_id: "remote-instance".to_owned(),
                session_mutation_stamp: Some(9_999),
            },
            true,
            None,
        )
        .expect("remote marker response should apply");
    assert_eq!(response.marker.session_id, local_session_id);
    assert_ne!(response.session_mutation_stamp, Some(9_999));
    let published_delta: DeltaEvent = serde_json::from_str(
        &delta_events
            .try_recv()
            .expect("proxy response should publish localized marker delta"),
    )
    .expect("localized marker delta should decode");
    let published_stamp = match published_delta {
        DeltaEvent::ConversationMarkerCreated {
            session_mutation_stamp: Some(stamp),
            ..
        } => stamp,
        _ => panic!("expected localized marker create delta"),
    };
    assert_eq!(response.session_mutation_stamp, Some(published_stamp));

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ConversationMarkerCreated {
                revision: 7,
                session_id: "remote-session-1".to_owned(),
                marker: remote_marker,
                session_mutation_stamp: Some(9_999),
            },
        )
        .expect("matching remote SSE replay should be skipped");
    assert!(
        delta_events.try_recv().is_err(),
        "matching remote SSE replay should not publish a duplicate local delta"
    );
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert_eq!(markers.markers.len(), 1);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_marker_proxy_response_returns_current_marker_after_stale_skip() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let target = state
        .remote_session_target(&local_session_id)
        .expect("remote target lookup should succeed")
        .expect("session should be remote-backed");
    let mut newer_marker = test_marker(
        "marker-remote-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );
    newer_marker.name = "Newer remote marker".to_owned();
    newer_marker.updated_at = "2026-05-01 10:02:00".to_owned();
    let mut delta_events = state.subscribe_delta_events();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ConversationMarkerCreated {
                revision: 8,
                session_id: "remote-session-1".to_owned(),
                marker: newer_marker,
                session_mutation_stamp: Some(12),
            },
        )
        .expect("newer remote marker delta should apply");
    let _ = delta_events
        .try_recv()
        .expect("newer marker delta should publish locally");

    let mut stale_marker = test_marker(
        "marker-remote-1",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );
    stale_marker.name = "Stale remote marker".to_owned();
    stale_marker.updated_at = "2026-05-01 10:01:00".to_owned();
    let response = state
        .apply_remote_marker_response(
            target,
            ConversationMarkerResponse {
                marker: stale_marker,
                revision: 7,
                server_instance_id: "remote-instance".to_owned(),
                session_mutation_stamp: Some(11),
            },
            true,
            None,
        )
        .expect("stale skipped marker response should return current local marker");

    assert_eq!(response.marker.session_id, local_session_id);
    assert_eq!(response.marker.name, "Newer remote marker");
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert_eq!(markers.markers.len(), 1);
    assert_eq!(markers.markers[0].name, "Newer remote marker");
    assert!(
        delta_events.try_recv().is_err(),
        "stale skipped response should not publish another local delta"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_marker_update_response_rejects_mismatched_marker_id() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let target = state
        .remote_session_target(&local_session_id)
        .expect("remote target lookup should succeed")
        .expect("session should be remote-backed");
    let remote_marker = test_marker(
        "marker-other",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );

    let err = state
        .apply_remote_marker_response(
            target,
            ConversationMarkerResponse {
                marker: remote_marker,
                revision: 7,
                server_instance_id: "remote-instance".to_owned(),
                session_mutation_stamp: Some(11),
            },
            false,
            Some("marker-expected"),
        )
        .expect_err("mismatched remote marker id should be rejected");
    assert!(
        err.message.contains("did not match requested marker"),
        "unexpected error: {}",
        err.message
    );
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert!(markers.markers.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_marker_update_proxy_rejects_mismatched_marker_id() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let remote_marker = test_marker(
        "marker-other",
        "remote-session-1",
        "remote-message-1",
        ConversationMarkerKind::Checkpoint,
    );
    let response_body =
        serde_json::to_string(&test_marker_response(remote_marker, 9)).expect("response encodes");
    let (port, requests, server) = spawn_remote_marker_proxy_server(
        "PATCH /api/sessions/remote-session-1/markers/marker-expected ",
        StatusCode::OK,
        response_body,
    );
    insert_test_remote_connection(&state, &remote, port);

    let err = state
        .update_conversation_marker(
            &local_session_id,
            "marker-expected",
            UpdateConversationMarkerRequest {
                kind: Some(ConversationMarkerKind::Checkpoint),
                name: None,
                body: None,
                color: None,
                message_id: None,
                end_message_id: None,
            },
        )
        .expect_err("mismatched remote marker id should be rejected");
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message.contains("did not match requested marker"),
        "unexpected error: {}",
        err.message
    );

    let requests = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        requests.iter().any(|(line, _)| {
            line.starts_with("PATCH /api/sessions/remote-session-1/markers/marker-expected ")
        }),
        "expected remote marker PATCH request, saw {requests:?}"
    );
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert!(markers.markers.is_empty());
    join_test_server(server);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_marker_deltas_localize_publish_and_skip_exact_replays() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);
    let create_marker = || {
        test_marker(
            "marker-remote-1",
            "remote-session-1",
            "remote-message-1",
            ConversationMarkerKind::Decision,
        )
    };
    let update_marker = || {
        let mut marker = create_marker();
        marker.kind = ConversationMarkerKind::Checkpoint;
        marker.name = "Remote marker updated".to_owned();
        marker.updated_at = "2026-05-01 10:01:00".to_owned();
        marker
    };
    let create_delta = || DeltaEvent::ConversationMarkerCreated {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        marker: create_marker(),
        session_mutation_stamp: Some(11),
    };
    let update_delta = || DeltaEvent::ConversationMarkerUpdated {
        revision: 4,
        session_id: "remote-session-1".to_owned(),
        marker: update_marker(),
        session_mutation_stamp: Some(12),
    };
    let delete_delta = || DeltaEvent::ConversationMarkerDeleted {
        revision: 5,
        session_id: "remote-session-1".to_owned(),
        marker_id: "marker-remote-1".to_owned(),
        session_mutation_stamp: Some(13),
    };
    let mut delta_events = state.subscribe_delta_events();

    state
        .apply_remote_delta_event(&remote.id, create_delta())
        .expect("remote marker create delta should apply");
    let published: DeltaEvent = serde_json::from_str(
        &delta_events
            .try_recv()
            .expect("create should publish localized marker delta"),
    )
    .expect("published marker create delta should decode");
    match published {
        DeltaEvent::ConversationMarkerCreated {
            session_id, marker, ..
        } => {
            assert_eq!(session_id, local_session_id);
            assert_eq!(marker.session_id, local_session_id);
            assert_eq!(marker.id, "marker-remote-1");
        }
        _ => panic!("expected ConversationMarkerCreated delta"),
    }
    state
        .apply_remote_delta_event(&remote.id, create_delta())
        .expect("exact create replay should be skipped");
    assert!(delta_events.try_recv().is_err());

    state
        .apply_remote_delta_event(&remote.id, update_delta())
        .expect("remote marker update delta should apply");
    let published: DeltaEvent = serde_json::from_str(
        &delta_events
            .try_recv()
            .expect("update should publish localized marker delta"),
    )
    .expect("published marker update delta should decode");
    match published {
        DeltaEvent::ConversationMarkerUpdated {
            session_id, marker, ..
        } => {
            assert_eq!(session_id, local_session_id);
            assert_eq!(marker.session_id, local_session_id);
            assert_eq!(marker.kind, ConversationMarkerKind::Checkpoint);
            assert_eq!(marker.name, "Remote marker updated");
        }
        _ => panic!("expected ConversationMarkerUpdated delta"),
    }
    state
        .apply_remote_delta_event(&remote.id, update_delta())
        .expect("exact update replay should be skipped");
    assert!(delta_events.try_recv().is_err());

    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert_eq!(markers.markers.len(), 1);
    assert_eq!(markers.markers[0].session_id, local_session_id);
    assert_eq!(markers.markers[0].kind, ConversationMarkerKind::Checkpoint);

    state
        .apply_remote_delta_event(&remote.id, delete_delta())
        .expect("remote marker delete delta should apply");
    let published: DeltaEvent = serde_json::from_str(
        &delta_events
            .try_recv()
            .expect("delete should publish localized marker delta"),
    )
    .expect("published marker delete delta should decode");
    match published {
        DeltaEvent::ConversationMarkerDeleted {
            session_id,
            marker_id,
            ..
        } => {
            assert_eq!(session_id, local_session_id);
            assert_eq!(marker_id, "marker-remote-1");
        }
        _ => panic!("expected ConversationMarkerDeleted delta"),
    }
    state
        .apply_remote_delta_event(&remote.id, delete_delta())
        .expect("exact delete replay should be skipped");
    assert!(delta_events.try_recv().is_err());

    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ConversationMarkerDeleted {
                revision: 6,
                session_id: "remote-session-1".to_owned(),
                marker_id: "marker-remote-1".to_owned(),
                session_mutation_stamp: Some(14),
            },
        )
        .expect("newer delete for an already-missing marker should be idempotent");
    assert!(delta_events.try_recv().is_err());
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list after delete");
    assert!(markers.markers.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_marker_delta_rejects_mismatched_marker_session_id() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_session_id = remote_proxy_session_with_message(&state, &remote);

    let err = state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::ConversationMarkerCreated {
                revision: 3,
                session_id: "remote-session-1".to_owned(),
                marker: test_marker(
                    "marker-remote-1",
                    "remote-session-2",
                    "remote-message-1",
                    ConversationMarkerKind::Decision,
                ),
                session_mutation_stamp: Some(11),
            },
        )
        .expect_err("mismatched marker/session ids should be rejected");
    assert!(
        err.to_string().contains("did not match event id"),
        "unexpected error: {err:#}"
    );
    let markers = state
        .list_conversation_markers(&local_session_id)
        .expect("localized markers should list");
    assert!(markers.markers.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn marker_create_rejects_unsupported_color_values() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let encoded_session_id = encode_uri_component(&session_id);
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{encoded_session_id}/markers"))
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "kind": "decision",
                    "name": "Unsafe color",
                    "color": "url(https://example.test/marker)",
                    "messageId": "message-1"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("conversation marker color")),
        "unexpected response: {response}"
    );
}

#[tokio::test]
async fn marker_patch_rejects_unsupported_color_values() {
    let state = test_app_state();
    let session_id = test_session_with_two_messages(&state);
    let created = state
        .create_conversation_marker(
            &session_id,
            CreateConversationMarkerRequest {
                kind: ConversationMarkerKind::Decision,
                name: "Safe color".to_owned(),
                body: None,
                color: "#3b82f6".to_owned(),
                message_id: "message-1".to_owned(),
                end_message_id: None,
            },
        )
        .expect("marker should be created");
    let encoded_session_id = encode_uri_component(&session_id);
    let encoded_marker_id = encode_uri_component(&created.marker.id);
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
        Request::builder()
            .method("PATCH")
            .uri(format!(
                "/api/sessions/{encoded_session_id}/markers/{encoded_marker_id}"
            ))
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "color": "var(--signal-blue)"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("conversation marker color")),
        "unexpected response: {response}"
    );
}

#[tokio::test]
async fn marker_json_rejection_uses_api_error_envelope() {
    let state = test_app_state();
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
        Request::builder()
            .method("POST")
            .uri("/api/sessions/session-1/markers")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("invalid conversation marker request JSON")),
        "unexpected response: {response}"
    );
}

#[tokio::test]
async fn marker_patch_json_rejection_uses_api_error_envelope() {
    let state = test_app_state();
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
        Request::builder()
            .method("PATCH")
            .uri("/api/sessions/session-1/markers/marker-1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("invalid conversation marker request JSON")),
        "unexpected response: {response}"
    );
}

#[tokio::test]
async fn marker_patch_unknown_kind_rejection_uses_api_error_envelope() {
    let state = test_app_state();
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(state),
        Request::builder()
            .method("PATCH")
            .uri("/api/sessions/session-1/markers/marker-1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"kind":"decison"}"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("unsupported conversation marker kind `decison`")),
        "unexpected response: {response}"
    );
}

#[tokio::test]
async fn marker_patch_is_allowed_by_cors_preflight() {
    let response = request_response(
        &app_router(test_app_state()),
        Request::builder()
            .method("OPTIONS")
            .uri("/api/sessions/session-1/markers/marker-1")
            .header(axum::http::header::ORIGIN, "http://127.0.0.1:8787")
            .header(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD, "PATCH")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let allow_methods = response
        .headers()
        .get(axum::http::header::ACCESS_CONTROL_ALLOW_METHODS)
        .expect("CORS preflight should include allowed methods")
        .to_str()
        .expect("allowed methods should be ASCII");
    assert!(
        allow_methods.contains("PATCH"),
        "expected PATCH in allow methods, saw {allow_methods}"
    );
}
