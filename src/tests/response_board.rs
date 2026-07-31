// End-to-end response-board regressions. These use the real HTTP router and
// SQLite transcript store so the board cannot accidentally depend on resident
// session messages or accept client-supplied snapshot content.

use super::*;

struct ResponseBoardTestFiles {
    persistence_path: PathBuf,
    orchestrator_templates_path: PathBuf,
}

impl ResponseBoardTestFiles {
    fn capture(state: &AppState) -> Self {
        Self {
            persistence_path: state.persistence_path.as_ref().clone(),
            orchestrator_templates_path: state.orchestrator_templates_path.as_ref().clone(),
        }
    }
}

impl Drop for ResponseBoardTestFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.persistence_path);
        let _ = fs::remove_file(self.persistence_path.with_extension("sqlite-shm"));
        let _ = fs::remove_file(self.persistence_path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(&self.orchestrator_templates_path);
    }
}

fn persist_response_board_fixture(state: &AppState) {
    let inner = state.inner.lock().expect("state mutex poisoned");
    persist_state(state.persistence_path.as_ref(), &inner)
        .expect("response-board fixture should persist");
}

fn push_response_board_message(state: &AppState, session_id: &str, text: &str) -> String {
    let message_id = state.allocate_message_id();
    state
        .push_message(
            session_id,
            Message::Text {
                attachments: Vec::new(),
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: text.to_owned(),
                expanded_text: None,
                source: None,
            },
        )
        .expect("response-board message should be recorded");
    message_id
}

#[tokio::test]
async fn response_board_snapshots_durable_messages_and_survives_source_pruning() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "Immutable board response");
    persist_response_board_fixture(&state);
    let app = app_router(state.clone());

    let (create_status, created): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "x": 120.0,
                    "y": 80.0
                }))
                .expect("request body should serialize"),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(create_status, StatusCode::CREATED);
    assert_eq!(created["sourceSessionId"], session_id);
    assert_eq!(created["sourceMessageId"], message_id);
    assert_eq!(created["snapshot"]["text"], "Immutable board response");
    assert_eq!(created["x"], 120.0);
    assert_eq!(created["y"], 80.0);

    let card_id = created["id"]
        .as_str()
        .expect("created card id should be a string")
        .to_owned();
    let (patch_status, patched): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "x": 240.0,
                    "y": 160.0,
                    "w": 520.0,
                    "h": 360.0
                }))
                .expect("patch body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(patch_status, StatusCode::OK);
    assert_eq!(patched["w"], 520.0);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner
            .find_session_index(&session_id)
            .expect("source session should exist");
        inner.sessions[session_index].session.messages[0] = Message::Text {
            attachments: Vec::new(),
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Mutated source response".to_owned(),
            expanded_text: None,
            source: None,
        };
        inner.sessions[session_index].message_positions =
            build_message_positions(&inner.sessions[session_index].session.messages);
    }
    persist_response_board_fixture(&state);
    let (_, board_after_mutation): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        board_after_mutation["cards"][0]["snapshot"]["text"],
        "Immutable board response"
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner
            .find_session_index(&session_id)
            .expect("source session should exist");
        inner.sessions.remove(session_index);
    }
    persist_response_board_fixture(&state);

    let restarted = AppState::new_with_paths(
        "/tmp".to_owned(),
        state.persistence_path.as_ref().clone(),
        state.orchestrator_templates_path.as_ref().clone(),
    )
    .expect("state should restart from the same database");
    let restarted_app = app_router(restarted);
    let (list_status, board): (StatusCode, Value) = request_json(
        &restarted_app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(board["cards"].as_array().map(Vec::len), Some(1));
    assert_eq!(board["cards"][0]["id"], created["id"]);
    assert_eq!(board["cards"][0]["x"], 240.0);
    assert_eq!(board["cards"][0]["h"], 360.0);
    assert_eq!(
        board["cards"][0]["snapshot"]["text"],
        "Immutable board response"
    );

    let delete_response = request_response(
        &restarted_app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    let (_, empty_board): (StatusCode, Value) = request_json(
        &restarted_app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(empty_board["cards"].as_array().map(Vec::len), Some(0));
}
