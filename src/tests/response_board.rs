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

fn attach_response_board_project(
    state: &AppState,
    session_id: &str,
    project_id: &str,
    project_name: &str,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner.projects.push(Project {
        id: project_id.to_owned(),
        name: project_name.to_owned(),
        root_path: format!("/tmp/{project_id}"),
        remote_id: String::new(),
        remote_project_id: None,
    });
    let session_index = inner
        .find_session_index(session_id)
        .expect("fixture session should exist");
    inner.sessions[session_index].session.project_id = Some(project_id.to_owned());
}

#[test]
fn response_board_schema_migrates_legacy_cards_without_rewriting_them() {
    let path = std::env::temp_dir().join(format!(
        "termal-response-board-migration-{}.sqlite",
        Uuid::new_v4()
    ));
    let connection = rusqlite::Connection::open(&path).expect("legacy database should open");
    connection
        .execute_batch(
            "CREATE TABLE board_cards (
               id TEXT PRIMARY KEY,
               x REAL NOT NULL,
               y REAL NOT NULL,
               w REAL NOT NULL,
               h REAL NOT NULL,
               snapshot_json TEXT NOT NULL,
               source_session_id TEXT NOT NULL,
               source_message_id TEXT NOT NULL,
               created_at TEXT NOT NULL
             );
             INSERT INTO board_cards VALUES(
               'legacy-card', 11, 22, 333, 444, 'legacy-snapshot',
               'legacy-session', 'legacy-message', '2026-08-22T00:00:00Z'
             );",
        )
        .expect("legacy response-board schema should initialize");

    ensure_sqlite_response_board_schema(&connection).expect("legacy board should migrate");
    let migrated: (String, String, String, bool, f64, f64, String) = connection
        .query_row(
            "SELECT id, tab_id, placement, has_canvas_position, x, y, snapshot_json
             FROM board_cards WHERE id = 'legacy-card'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("migrated legacy card should remain");
    assert_eq!(
        migrated,
        (
            "legacy-card".to_owned(),
            "response-board-default".to_owned(),
            "placed".to_owned(),
            true,
            11.0,
            22.0,
            "legacy-snapshot".to_owned(),
        )
    );
    let default_tab_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM response_board_tabs
             WHERE id = 'response-board-default'",
            [],
            |row| row.get(0),
        )
        .expect("default tab should exist");
    assert_eq!(default_tab_count, 1);
    drop(connection);
    let _ = fs::remove_file(path);
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

#[tokio::test]
async fn response_board_tabs_stage_cards_idempotently_and_keep_legacy_view_placed_only() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "Staged response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (create_tab_status, tab): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Project A" }))
                    .expect("tab request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(create_tab_status, StatusCode::CREATED);
    assert_eq!(tab["name"], "Project A");
    assert_eq!(tab["kind"], "custom");
    let tab_id = tab["id"]
        .as_str()
        .expect("created tab id should be a string")
        .to_owned();

    let stage_request = || {
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "tabId": tab_id,
                }))
                .expect("stage request should serialize"),
            ))
            .unwrap()
    };
    let (first_stage_status, staged): (StatusCode, Value) =
        request_json(&app, stage_request()).await;
    assert_eq!(first_stage_status, StatusCode::CREATED);
    assert_eq!(staged["tabId"], tab_id);
    assert_eq!(staged["placement"], "staged");
    assert_eq!(staged["hasCanvasPosition"], false);
    assert_eq!(staged["snapshot"]["text"], "Staged response");

    let (second_stage_status, duplicate): (StatusCode, Value) =
        request_json(&app, stage_request()).await;
    assert_eq!(second_stage_status, StatusCode::OK);
    assert_eq!(duplicate["id"], staged["id"]);

    let (_, custom_view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(custom_view["cards"].as_array().map(Vec::len), Some(0));
    assert_eq!(custom_view["stagedCards"].as_array().map(Vec::len), Some(1));

    let (_, legacy_before_place): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        legacy_before_place["cards"].as_array().map(Vec::len),
        Some(0)
    );

    let card_id = staged["id"]
        .as_str()
        .expect("staged card id should be a string");
    let (missing_position_status, missing_position_error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "placement": "placed" }))
                    .expect("positionless place request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(missing_position_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_position_error["error"],
        "x and y are required when placing a response without a saved canvas position"
    );

    let (place_status, placed): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "placement": "placed",
                    "x": 320.0,
                    "y": 240.0,
                    "w": 480.0,
                    "h": 300.0,
                }))
                .expect("place request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(place_status, StatusCode::OK);
    assert_eq!(placed["placement"], "placed");
    assert_eq!(placed["hasCanvasPosition"], true);
    assert_eq!(placed["x"], 320.0);

    let (unplace_status, unplaced): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "placement": "staged" }))
                    .expect("unplace request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(unplace_status, StatusCode::OK);
    assert_eq!(unplaced["placement"], "staged");
    assert_eq!(unplaced["hasCanvasPosition"], true);
    assert_eq!(unplaced["x"], 320.0);
    assert_eq!(unplaced["snapshot"], staged["snapshot"]);

    let delete_tab_response = request_response(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_tab_response.status(), StatusCode::NO_CONTENT);
    let (_, default_view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs/response-board-default")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        default_view["stagedCards"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        default_view["stagedCards"][0]["tabId"],
        "response-board-default"
    );
}

#[tokio::test]
async fn response_board_stage_route_places_transcript_drops_atomically() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "Atomic board response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (tab_status, tab): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Atomic" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(tab_status, StatusCode::CREATED);
    let tab_id = tab["id"].as_str().unwrap();

    let place_request = |x: f64, y: f64| {
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "tabId": tab_id,
                    "placement": "placed",
                    "x": x,
                    "y": y,
                }))
                .unwrap(),
            ))
            .unwrap()
    };
    let (first_status, first): (StatusCode, Value) =
        request_json(&app, place_request(120.0, 80.0)).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(first["placement"], "placed");
    assert_eq!(first["hasCanvasPosition"], true);
    assert_eq!(first["x"], 120.0);

    let (repeat_status, repeated): (StatusCode, Value) =
        request_json(&app, place_request(240.0, 160.0)).await;
    assert_eq!(repeat_status, StatusCode::OK);
    assert_eq!(repeated["id"], first["id"]);
    assert_eq!(repeated["x"], 240.0);

    let (_, view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(view["cards"].as_array().map(Vec::len), Some(1));
    assert_eq!(view["stagedCards"].as_array().map(Vec::len), Some(0));

    let (invalid_status, invalid): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "tabId": tab_id,
                    "placement": "placed",
                    "x": 10.0,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid["error"],
        "x and y are required when placement is placed"
    );
}

#[tokio::test]
async fn response_board_rejects_duplicate_source_on_legacy_create_and_same_tab_place() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let persistence_path = state.persistence_path.as_ref().clone();
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "One source card");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (stage_status, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(stage_status, StatusCode::CREATED);

    let (legacy_status, legacy_error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "x": 10.0,
                    "y": 20.0,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(legacy_status, StatusCode::CONFLICT);
    assert_eq!(
        legacy_error["error"],
        "that response is already on the response board"
    );

    let connection = rusqlite::Connection::open(&persistence_path).unwrap();
    connection
        .execute(
            "INSERT INTO board_cards(
               id, tab_id, placement, has_canvas_position, x, y, w, h,
               snapshot_json, source_session_id, source_message_id, created_at
             ) VALUES (
               'legacy-placed-duplicate', ?1, 'placed', 1, 30, 40, ?2, ?3,
               ?4, ?5, ?6, '2026-08-23T00:00:00Z'
             )",
            rusqlite::params![
                RESPONSE_BOARD_DEFAULT_TAB_ID,
                RESPONSE_BOARD_DEFAULT_WIDTH,
                RESPONSE_BOARD_DEFAULT_HEIGHT,
                serde_json::to_string(&staged["snapshot"]).unwrap(),
                session_id,
                message_id,
            ],
        )
        .unwrap();
    drop(connection);

    let staged_id = staged["id"].as_str().unwrap();
    let (place_status, place_error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{staged_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "placement": "placed",
                    "x": 50.0,
                    "y": 60.0,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(place_status, StatusCode::CONFLICT);
    assert_eq!(
        place_error["error"],
        "that response is already pinned in the destination tab"
    );
}

#[tokio::test]
async fn response_board_repin_reuses_a_card_after_it_moves_to_another_tab() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "One durable card");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (_, tab): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Research" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let tab_id = tab["id"].as_str().unwrap();
    let stage_request = || {
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "tabId": tab_id,
                }))
                .unwrap(),
            ))
            .unwrap()
    };
    let (stage_status, staged): (StatusCode, Value) = request_json(&app, stage_request()).await;
    assert_eq!(stage_status, StatusCode::CREATED);
    let card_id = staged["id"].as_str().unwrap();

    let (move_status, moved): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "tabId": RESPONSE_BOARD_DEFAULT_TAB_ID,
                    "placement": "placed",
                    "x": 120.0,
                    "y": 80.0,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(move_status, StatusCode::OK);
    assert_eq!(moved["tabId"], RESPONSE_BOARD_DEFAULT_TAB_ID);

    let (repin_status, repinned): (StatusCode, Value) = request_json(&app, stage_request()).await;
    assert_eq!(repin_status, StatusCode::OK);
    assert_eq!(repinned["id"], staged["id"]);
    assert_eq!(repinned["tabId"], tab_id);
    assert_eq!(repinned["placement"], "staged");
}

#[tokio::test]
async fn response_board_return_to_staging_enforces_the_global_capacity() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id =
        push_response_board_message(&state, &session_id, "Placed response at staging limit");
    persist_response_board_fixture(&state);
    let app = app_router(state.clone());

    let (create_status, placed): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "x": 40.0,
                    "y": 60.0,
                }))
                .expect("placed-card request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    let card_id = placed["id"]
        .as_str()
        .expect("placed card id should be a string")
        .to_owned();

    let connection = rusqlite::Connection::open(state.persistence_path.as_ref())
        .expect("response-board database should open");
    let snapshot_json: String = connection
        .query_row(
            "SELECT snapshot_json FROM board_cards WHERE id = ?1",
            rusqlite::params![card_id],
            |row| row.get(0),
        )
        .expect("placed-card snapshot should exist");
    let transaction = connection
        .unchecked_transaction()
        .expect("capacity fixture transaction should start");
    for index in 0..RESPONSE_BOARD_MAX_CARDS {
        transaction
            .execute(
                "INSERT INTO board_cards(
                   id, tab_id, placement, has_canvas_position, x, y, w, h,
                   snapshot_json, source_session_id, source_message_id, created_at
                 ) VALUES (?1, ?2, 'staged', 0, 0, 0, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    format!("staging-capacity-card-{index}"),
                    RESPONSE_BOARD_DEFAULT_TAB_ID,
                    RESPONSE_BOARD_DEFAULT_WIDTH,
                    RESPONSE_BOARD_DEFAULT_HEIGHT,
                    snapshot_json,
                    format!("staging-capacity-session-{index}"),
                    format!("staging-capacity-message-{index}"),
                    format!("2026-08-22T00:00:{index:02}Z"),
                ],
            )
            .expect("staged capacity fixture should insert");
    }
    transaction
        .commit()
        .expect("staged capacity fixture should commit");
    drop(connection);

    let (return_status, error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{card_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "placement": "staged" }))
                    .expect("return-to-staging request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(return_status, StatusCode::CONFLICT);
    assert_eq!(
        error["error"],
        format!("response-board staging is limited to {RESPONSE_BOARD_MAX_CARDS} cards")
    );

    let (_, default_view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs/response-board-default")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(default_view["cards"].as_array().map(Vec::len), Some(1));
    assert_eq!(default_view["cards"][0]["id"], card_id);
    assert_eq!(
        default_view["stagedCards"].as_array().map(Vec::len),
        Some(RESPONSE_BOARD_MAX_CARDS as usize),
    );
}

#[tokio::test]
async fn response_board_return_to_staging_rejects_a_duplicate_staged_source() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let persistence_path = state.persistence_path.as_ref().clone();
    let session_id = test_session_id(&state, Agent::Codex);
    let message_id = push_response_board_message(&state, &session_id, "Duplicate source");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (_, destination_tab): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Destination" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let destination_tab_id = destination_tab["id"].as_str().unwrap();
    let (_, placed): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "x": 10.0,
                    "y": 20.0,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let placed_id = placed["id"].as_str().unwrap();

    let connection = rusqlite::Connection::open(&persistence_path).unwrap();
    connection
        .execute(
            "INSERT INTO board_cards(
               id, tab_id, placement, has_canvas_position, x, y, w, h,
               snapshot_json, source_session_id, source_message_id, created_at
             ) VALUES (
               'legacy-staged-duplicate', ?1, 'staged', 0, 0, 0, ?2, ?3,
               ?4, ?5, ?6, '2026-08-23T00:00:00Z'
             )",
            rusqlite::params![
                destination_tab_id,
                RESPONSE_BOARD_DEFAULT_WIDTH,
                RESPONSE_BOARD_DEFAULT_HEIGHT,
                serde_json::to_string(&placed["snapshot"]).unwrap(),
                session_id,
                message_id,
            ],
        )
        .unwrap();
    drop(connection);

    let (move_status, moved): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{placed_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "tabId": destination_tab_id })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(move_status, StatusCode::OK);
    assert_eq!(moved["tabId"], destination_tab_id);

    let (return_status, error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/cards/{placed_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "placement": "staged" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(return_status, StatusCode::CONFLICT);
    assert_eq!(
        error["error"],
        "that response is already waiting in staging"
    );
}

#[tokio::test]
async fn response_board_legacy_cards_migrate_into_the_default_tab_as_placed() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Claude);
    let message_id = push_response_board_message(&state, &session_id, "Legacy response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (create_status, card): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "x": 40.0,
                    "y": 60.0,
                }))
                .expect("legacy create request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    assert_eq!(card["placement"], "placed");

    let (tabs_status, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(tabs_status, StatusCode::OK);
    assert_eq!(tabs["tabs"].as_array().map(Vec::len), Some(1));
    assert_eq!(tabs["tabs"][0]["id"], "response-board-default");
    assert_eq!(tabs["tabs"][0]["placedCardCount"], 1);
    assert_eq!(tabs["stagedCardCount"], 0);
}

#[tokio::test]
async fn response_board_project_pins_create_one_project_tab_and_reuse_the_card() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-response-board".to_owned();
    attach_response_board_project(&state, &session_id, &project_id, "Project Response Board");
    let message_id = push_response_board_message(&state, &session_id, "Project response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let stage_request = || {
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .expect("project stage request should serialize"),
            ))
            .unwrap()
    };
    let (first_status, first): (StatusCode, Value) = request_json(&app, stage_request()).await;
    let (second_status, second): (StatusCode, Value) = request_json(&app, stage_request()).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(first["id"], second["id"]);

    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let project_tab = tabs["tabs"]
        .as_array()
        .and_then(|tabs| tabs.iter().find(|tab| tab["projectId"] == project_id))
        .expect("project tab should be created");
    assert_eq!(project_tab["name"], "Project Response Board");
    assert_eq!(project_tab["kind"], "projectDefault");
    assert_eq!(tabs["stagedCardCount"], 1);
    assert_eq!(first["tabId"], project_tab["id"]);

    let (_, default_view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs/response-board-default")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(default_view["cards"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        default_view["stagedCards"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(default_view["stagedCards"][0]["id"], first["id"]);
}

#[tokio::test]
async fn response_board_project_tabs_preserve_names_beyond_the_custom_tab_limit() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-long-response-board-name".to_owned();
    let project_name = format!("Projekt {}", "ż".repeat(64));
    assert!(project_name.len() > RESPONSE_BOARD_MAX_TAB_NAME_BYTES);
    attach_response_board_project(&state, &session_id, &project_id, &project_name);
    let message_id = push_response_board_message(&state, &session_id, "Long-name response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (stage_status, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .expect("project stage request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(stage_status, StatusCode::CREATED);

    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let project_tab = tabs["tabs"]
        .as_array()
        .and_then(|tabs| tabs.iter().find(|tab| tab["projectId"] == project_id))
        .expect("project tab should be created");
    assert_eq!(project_tab["name"], project_name);
    assert_eq!(staged["tabId"], project_tab["id"]);
}

#[tokio::test]
async fn response_board_project_tabs_cannot_be_renamed_through_the_api() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-fixed-response-board-name".to_owned();
    let project_name = "Fixed Project Board";
    attach_response_board_project(&state, &session_id, &project_id, project_name);
    let message_id = push_response_board_message(&state, &session_id, "Fixed-name response");
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (stage_status, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .expect("project stage request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(stage_status, StatusCode::CREATED);
    let tab_id = staged["tabId"].as_str().expect("project tab id");

    let (rename_status, error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Renamed" }))
                    .expect("rename request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(rename_status, StatusCode::CONFLICT);
    assert_eq!(
        error["error"],
        "project response-board tabs cannot be renamed"
    );

    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let project_tab = tabs["tabs"]
        .as_array()
        .and_then(|tabs| tabs.iter().find(|tab| tab["id"] == tab_id))
        .expect("project tab should remain");
    assert_eq!(project_tab["name"], project_name);
}

#[tokio::test]
async fn response_board_project_tabs_render_the_live_project_name() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-live-response-board-name".to_owned();
    attach_response_board_project(&state, &session_id, &project_id, "Original Project");
    let message_id = push_response_board_message(&state, &session_id, "Live-name response");
    persist_response_board_fixture(&state);
    let app = app_router(state.clone());

    let (_, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let tab_id = staged["tabId"].as_str().unwrap().to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should remain resident")
            .name = "Renamed Project".to_owned();
    }

    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let listed_tab = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["id"] == tab_id)
        .unwrap();
    assert_eq!(listed_tab["name"], "Renamed Project");

    let (_, view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(view["tab"]["name"], "Renamed Project");
}

#[tokio::test]
async fn response_board_project_tab_becomes_custom_when_its_project_is_deleted() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-deleted-response-board".to_owned();
    attach_response_board_project(&state, &session_id, &project_id, "Deleted Project Board");
    let message_id = push_response_board_message(&state, &session_id, "Keep this response");
    persist_response_board_fixture(&state);
    let app = app_router(state.clone());

    let (_, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let tab_id = staged["tabId"].as_str().unwrap().to_owned();
    {
        let mut connection = rusqlite::Connection::open(state.persistence_path.as_ref()).unwrap();
        let transaction = connection.transaction().unwrap();
        for index in 0..RESPONSE_BOARD_MAX_CUSTOM_TABS {
            transaction
                .execute(
                    "INSERT INTO response_board_tabs(
                       id, name, kind, project_id, sort_order, created_at
                     ) VALUES (?1, ?2, 'custom', NULL, ?3, ?4)",
                    rusqlite::params![
                        format!("pre-detach-custom-{index}"),
                        format!("Pre-detach {index}"),
                        10_000 + index as i64,
                        format!("2026-08-23T01:00:{index:02}Z"),
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should remain resident")
            .name = "Final Deleted Project Name".to_owned();
    }

    state
        .delete_project(&project_id)
        .expect("project deletion should detach its board tab");

    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let detached_tab = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["id"] == tab_id)
        .expect("detached tab should preserve its cards and identity");
    assert_eq!(detached_tab["name"], "Final Deleted Project Name");
    assert_eq!(detached_tab["kind"], "custom");
    assert_eq!(detached_tab["projectId"], Value::Null);
    assert_eq!(
        tabs["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tab| { tab["kind"] == "custom" && tab["id"] != RESPONSE_BOARD_DEFAULT_TAB_ID })
            .count(),
        RESPONSE_BOARD_MAX_CUSTOM_TABS + 1,
        "project deletion must preserve the detached tab even at the creation cap",
    );

    let (_, view): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/response-board/tabs/{tab_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(view["stagedCards"][0]["id"], staged["id"]);
}

#[tokio::test]
async fn response_board_project_tab_detachment_failure_is_durable_and_retryable() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let session_id = test_session_id(&state, Agent::Codex);
    let project_id = "project-deferred-response-board-detach".to_owned();
    attach_response_board_project(&state, &session_id, &project_id, "Initial name");
    let message_id = push_response_board_message(&state, &session_id, "Keep this response");
    persist_response_board_fixture(&state);
    let app = app_router(state.clone());
    let (_, staged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/cards/stage")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "sessionId": session_id,
                    "messageId": message_id,
                    "projectId": project_id,
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    let tab_id = staged["tabId"].as_str().unwrap().to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .expect("project should remain resident")
            .name = "Final retry name".to_owned();
    }
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_response_board_project_detach
             BEFORE UPDATE OF kind ON response_board_tabs
             WHEN NEW.kind = 'custom'
             BEGIN
               SELECT RAISE(ABORT, 'injected response-board detach failure');
             END;",
        )
        .unwrap();

    state
        .delete_project(&project_id)
        .expect("secondary tab failure must not reverse the committed project deletion");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .projects
                .iter()
                .all(|project| project.id != project_id)
        );
        assert_eq!(
            inner
                .pending_response_board_project_detachments
                .get(&project_id)
                .map(String::as_str),
            Some("Final retry name")
        );
    }
    connection
        .execute_batch("DROP TRIGGER fail_response_board_project_detach;")
        .unwrap();
    drop(connection);

    state.replay_pending_response_board_project_detachments();
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_response_board_project_detachments
            .is_empty()
    );
    let (_, tabs): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/response-board/tabs")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let detached = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["id"] == tab_id)
        .expect("retried tab should remain available");
    assert_eq!(detached["name"], "Final retry name");
    assert_eq!(detached["kind"], "custom");
    assert_eq!(detached["projectId"], Value::Null);
}

#[tokio::test]
async fn response_board_custom_tabs_can_be_reordered_renamed_and_deleted_when_empty() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let mut created_ids = Vec::new();
    for name in ["Research", "Launch"] {
        let (status, tab): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/response-board/tabs")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "name": name }))
                        .expect("tab request should serialize"),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        created_ids.push(tab["id"].as_str().expect("tab id").to_owned());
    }

    let desired_order = json!(["response-board-default", created_ids[1], created_ids[0]]);
    for invalid_order in [
        json!(["response-board-default", created_ids[0]]),
        json!(["response-board-default", created_ids[0], created_ids[0],]),
        json!([
            "response-board-default",
            created_ids[0],
            "unknown-response-board-tab",
        ]),
    ] {
        let (status, error): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/response-board/tabs/reorder")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "tabIds": invalid_order })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error["error"],
            "tabIds must contain every response-board tab exactly once"
        );
    }
    let (reorder_status, reordered): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs/reorder")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "tabIds": desired_order }))
                    .expect("reorder request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(reorder_status, StatusCode::OK);
    assert_eq!(
        reordered["tabs"]
            .as_array()
            .expect("tabs")
            .iter()
            .map(|tab| tab["id"].clone())
            .collect::<Vec<_>>(),
        desired_order.as_array().expect("desired order").clone(),
    );

    let (rename_status, renamed): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/response-board/tabs/{}", created_ids[0]))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Findings" }))
                    .expect("rename request should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(rename_status, StatusCode::OK);
    assert_eq!(renamed["name"], "Findings");

    let delete_response = request_response(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/response-board/tabs/{}", created_ids[0]))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn response_board_rejects_control_characters_in_tab_names() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    persist_response_board_fixture(&state);
    let app = app_router(state);

    let (status, error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "Broken\nTab" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"],
        "tab name must not contain control characters"
    );
}

#[tokio::test]
async fn response_board_rejects_custom_tabs_past_the_global_limit() {
    let state = test_app_state();
    let _files = ResponseBoardTestFiles::capture(&state);
    let persistence_path = state.persistence_path.as_ref().clone();
    persist_response_board_fixture(&state);
    let mut connection = rusqlite::Connection::open(&persistence_path).unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..RESPONSE_BOARD_MAX_CUSTOM_TABS {
        transaction
            .execute(
                "INSERT INTO response_board_tabs(
                   id, name, kind, project_id, sort_order, created_at
                 ) VALUES (?1, ?2, 'custom', NULL, ?3, ?4)",
                rusqlite::params![
                    format!("custom-tab-{index}"),
                    format!("Custom {index}"),
                    index as i64,
                    format!("2026-08-23T00:00:{index:02}Z"),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let app = app_router(state);

    let (status, error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/response-board/tabs")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "name": "One too many" })).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        error["error"],
        format!("response board supports at most {RESPONSE_BOARD_MAX_CUSTOM_TABS} custom tabs")
    );
}
