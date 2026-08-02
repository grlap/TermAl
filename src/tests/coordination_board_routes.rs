//! Coordination-board HTTP surface authorization coverage (tm-uwx.7.3).
//!
//! Pins `resolve_board_scope_for_session`'s backend-authority gates: the
//! caller must be a LOCAL ROOT session in a LOCAL project. The two
//! delegation-child evidence sources are INDEPENDENT — rejection fires on
//! parent marker OR durable-index membership (root standing requires marker
//! null AND index absence), mirroring `mailbox_peer_names`. MCP tool
//! filtering is defense-in-depth; these tests prove the backend rejects on
//! its own (surface review, mailbox #219/#222/#224).

use super::*;

fn board_test_delegation_record(
    id: &str,
    parent_session_id: &str,
    child_session_id: &str,
) -> DelegationRecord {
    DelegationRecord {
        id: id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        child_session_id: child_session_id.to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Reviewer".to_owned(),
        prompt: "Review the patch.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: None,
        completed_at: None,
        result: None,
        result_parser_version: 0,
    }
}

fn assign_session_project(state: &AppState, session_id: &str, project_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    inner.sessions[index].session.project_id = Some(project_id.to_owned());
}

#[test]
fn board_scope_resolves_for_a_local_root_session_in_a_local_project() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Board Project");
    assign_session_project(&state, &session_id, &project_id);

    let (scope, author_name) = resolve_board_scope_for_session(&state, &session_id)
        .expect("local root session in local project should resolve");
    assert_eq!(scope, project_id);
    assert_eq!(author_name, "Test");
}

#[test]
fn board_scope_rejects_a_delegation_child_by_parent_marker() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Board Project");
    assign_session_project(&state, &session_id, &project_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].session.parent_delegation_id =
            Some("delegation-board-marker".to_owned());
    }

    let error = resolve_board_scope_for_session(&state, &session_id)
        .expect_err("marker-linked delegation child must be rejected");
    assert!(
        error.message.contains("local root session"),
        "unexpected rejection message: {}",
        error.message
    );
}

#[test]
fn board_scope_rejects_a_delegation_child_known_only_to_the_durable_index() {
    // Independent-evidence fallback: a child whose parent marker was lost
    // (the repair-lag window) must still be rejected via the durable
    // delegation index — marker absence alone must never grant root standing.
    let state = test_app_state();
    let parent_id = test_session_id(&state, Agent::Codex);
    let child_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Board Project");
    assign_session_project(&state, &child_id, &project_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.delegations.push(board_test_delegation_record(
            "delegation-board-index-only",
            &parent_id,
            &child_id,
        ));
        let index = inner
            .find_session_index(&child_id)
            .expect("child session should exist");
        inner.sessions[index].session.parent_delegation_id = None;
    }

    let error = resolve_board_scope_for_session(&state, &child_id)
        .expect_err("index-linked delegation child must be rejected despite a null marker");
    assert!(error.message.contains("local root session"));
}

#[test]
fn board_scope_rejects_a_remote_proxy_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Board Project");
    assign_session_project(&state, &session_id, &project_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].remote_id = Some("remote-1".to_owned());
        inner.sessions[index].remote_session_id = Some("session-remote-9".to_owned());
    }

    let error = resolve_board_scope_for_session(&state, &session_id)
        .expect_err("remote proxy sessions must be rejected");
    assert!(error.message.contains("local root session"));
}

#[test]
fn board_scope_rejects_a_session_without_a_project() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);

    let error = resolve_board_scope_for_session(&state, &session_id)
        .expect_err("sessions outside any project have no board scope");
    assert!(error.message.contains("no project"));
}

#[test]
fn board_scope_rejects_a_remote_project() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .create_project(
                Some("Remote Project".to_owned()),
                "/tmp/remote".to_owned(),
                "remote-1".to_owned(),
            )
            .id
    };
    assign_session_project(&state, &session_id, &project_id);

    let error = resolve_board_scope_for_session(&state, &session_id)
        .expect_err("remote projects are rejected in local-authoritative v1");
    assert!(error.message.contains("local-authoritative"));
}

#[test]
fn project_deletion_with_a_disabled_board_store_stays_silent_and_succeeds() {
    // Canonical test AppStates carry the zero-fd disabled board store; the
    // lifecycle cleanup must treat Disabled as an expected no-op (mailbox
    // #222-B), not surface an error or log noise.
    let state = test_app_state();
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Doomed Project");

    state
        .delete_project(&project_id)
        .expect("project deletion must succeed with a disabled board store");
}

#[test]
fn project_deletion_defers_retryable_cleanup_and_replays_later() {
    let base = test_app_state();
    let board_temp_root = BoardRouteTempRoot::new("retryable-delete");
    let board_store = Arc::new(
        CoordinationBoardStore::open(&board_temp_root.database_path())
            .expect("board cleanup test store should open"),
    );
    let state = AppState {
        coordination_board_store: board_store.clone(),
        ..base
    };
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Deferred Cleanup Project");
    state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id.clone(),
            key: "status.cleanup".to_owned(),
            value: Some(json!(true)),
            expected_revision: 0,
            author_session_id: "session-cleanup-test".to_owned(),
            author_name: "Cleanup Test".to_owned(),
            idempotency_key: "cleanup-seed".to_owned(),
            state_stamp: None,
        })
        .expect("cleanup fixture should persist");

    let connection_blocker = board_store
        .connection()
        .expect("test should hold the private board connection");
    state
        .delete_project(&project_id)
        .expect("typed retryable cleanup must not turn a durable project deletion into a 500");
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .contains(&project_id),
        "retryable cleanup must remain durably queued"
    );
    drop(connection_blocker);

    state
        .replay_pending_coordination_scope_deletions()
        .expect("deferred cleanup should replay once the board connection is available");
    assert!(
        !state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .contains(&project_id),
        "successful replay must clear only the completed outbox item"
    );
    let error = board_store
        .get(&project_id, "status.cleanup")
        .expect_err("replayed cleanup must fence and remove the board scope");
    assert_eq!(
        error
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("scope fence should return the typed board error")
            .kind,
        CoordinationBoardStoreErrorKind::NotFound
    );
}

#[test]
fn cleanup_replay_retains_nonretryable_failures_and_persists_completed_scopes() {
    let base = test_app_state();
    let board_temp_root = BoardRouteTempRoot::new("nonretryable-cleanup");
    let board_store = Arc::new(
        CoordinationBoardStore::open(&board_temp_root.database_path())
            .expect("board cleanup test store should open"),
    );
    let state = AppState {
        coordination_board_store: board_store.clone(),
        ..base
    };
    let completed_scope = "project-completed-cleanup";
    let invalid_scope = format!("z{}", "x".repeat(COORDINATION_BOARD_MAX_SCOPE_ID_BYTES));
    board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: completed_scope.to_owned(),
            key: "status.cleanup".to_owned(),
            value: Some(json!(true)),
            expected_revision: 0,
            author_session_id: "session-cleanup-test".to_owned(),
            author_name: "Cleanup Test".to_owned(),
            idempotency_key: "cleanup-nonretryable-seed".to_owned(),
            state_stamp: None,
        })
        .expect("completed-scope fixture should persist");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .pending_coordination_scope_deletions
            .insert(completed_scope.to_owned());
        inner
            .pending_coordination_scope_deletions
            .insert(invalid_scope.clone());
    }

    state
        .replay_pending_coordination_scope_deletions()
        .expect("nonretryable secondary cleanup must not fail the durable primary deletion");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        !inner
            .pending_coordination_scope_deletions
            .contains(completed_scope),
        "successful cleanup before a later failure must still leave the outbox"
    );
    assert!(
        inner
            .pending_coordination_scope_deletions
            .contains(&invalid_scope),
        "nonretryable cleanup failure must remain durably queued"
    );
    drop(inner);
    let error = board_store
        .get(completed_scope, "status.cleanup")
        .expect_err("completed cleanup must fence and remove the board scope");
    assert_eq!(
        error
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("scope fence should return the typed board error")
            .kind,
        CoordinationBoardStoreErrorKind::NotFound
    );
}

#[test]
fn project_deletion_cascades_the_coordination_board_scope() {
    let state = test_app_state();
    let board_temp_root = BoardRouteTempRoot::new("cascade");
    let state = AppState {
        coordination_board_store: Arc::new(
            CoordinationBoardStore::open(&board_temp_root.database_path())
                .expect("board cascade test store should open"),
        ),
        ..state
    };
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Cascade Project");

    let receipt = state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id.clone(),
            key: "activity.rust-suite".to_owned(),
            value: Some(json!({ "holder": "Fable" })),
            expected_revision: 0,
            author_session_id: "session-cascade-test".to_owned(),
            author_name: "Fable".to_owned(),
            idempotency_key: "cascade-set-1".to_owned(),
            state_stamp: None,
        })
        .expect("seeding the scope should succeed");
    assert_eq!(receipt.revision, 1);

    state
        .delete_project(&project_id)
        .expect("project deletion should succeed");

    let error = state
        .coordination_board_store
        .list(&CoordinationBoardListRequest {
            scope_project_id: project_id.clone(),
            ..CoordinationBoardListRequest::default()
        })
        .expect_err("the durable deletion fence must reject the removed scope");
    assert_eq!(
        error
            .downcast_ref::<CoordinationBoardStoreError>()
            .expect("deleted scope should return the typed board error")
            .kind,
        CoordinationBoardStoreErrorKind::NotFound
    );
}

#[test]
fn project_deletion_persist_failure_preserves_board_data_for_restart_recovery() {
    let base = test_app_state();
    let board_temp_root = BoardRouteTempRoot::new("persist-failure");
    let invalid_primary_path = board_temp_root.0.join("termal.sqlite");
    fs::create_dir_all(&invalid_primary_path)
        .expect("a directory at the primary file path should force persist failure");
    let state = AppState {
        persistence_path: Arc::new(invalid_primary_path),
        coordination_board_store: Arc::new(
            CoordinationBoardStore::open(&board_temp_root.0.join("coordination.sqlite"))
                .expect("board store should open"),
        ),
        ..base
    };
    let project_id = {
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .create_project(
                Some("Persist Failure Project".to_owned()),
                "/tmp".to_owned(),
                default_local_remote_id(),
            )
            .id
    };
    state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id.clone(),
            key: "status.preserved".to_owned(),
            value: Some(json!(true)),
            expected_revision: 0,
            author_session_id: "session-persist-failure".to_owned(),
            author_name: "Persist Failure".to_owned(),
            idempotency_key: "persist-failure-seed".to_owned(),
            state_stamp: None,
        })
        .expect("board fixture should persist");

    let error = match state.delete_project(&project_id) {
        Ok(_) => panic!("primary persistence failure must fail project deletion"),
        Err(error) => error,
    };
    assert!(error.message.contains("failed to remove project"));
    assert_eq!(
        state
            .coordination_board_store
            .get(&project_id, "status.preserved")
            .expect("board data must survive until the primary deletion is durable")
            .value,
        json!(true)
    );
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .contains(&project_id),
        "the durable-intent outbox must remain pending after primary persist failure"
    );
}

#[test]
fn board_scope_rejects_a_hidden_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Board Project");
    assign_session_project(&state, &session_id, &project_id);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].hidden = true;
    }

    let error = resolve_board_scope_for_session(&state, &session_id)
        .expect_err("hidden sessions must be rejected");
    assert!(error.message.contains("local root session"));
}

/// Removes the per-test board temp directory (SQLite main/WAL/SHM files) on
/// drop, so route tests cannot leak artifacts even on panic (review, mailbox
/// #240).
struct BoardRouteTempRoot(PathBuf);

impl BoardRouteTempRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("termal-board-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("board route test root should exist");
        Self(path)
    }

    fn database_path(&self) -> PathBuf {
        self.0.join("termal.sqlite")
    }
}

impl Drop for BoardRouteTempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn board_route_test_state() -> (AppState, String, BoardRouteTempRoot) {
    let base = test_app_state();
    let board_temp_root = BoardRouteTempRoot::new("routes");
    let state = AppState {
        coordination_board_store: Arc::new(
            CoordinationBoardStore::open(&board_temp_root.database_path())
                .expect("board route test store should open"),
        ),
        ..base
    };
    let session_id = test_session_id(&state, Agent::Claude);
    let project_id = create_test_project(&state, FsPath::new("/tmp"), "Wire Project");
    assign_session_project(&state, &session_id, &project_id);
    (state, session_id, board_temp_root)
}

#[test]
fn board_author_snapshot_normalizes_legacy_session_names_without_blocking_writes() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].session.name = format!("\nReview\t{}", "é".repeat(200));
    }

    let (scope_project_id, author_name) =
        resolve_board_scope_for_session(&state, &session_id).expect("board scope should resolve");
    assert!(!author_name.chars().any(char::is_control));
    assert!(author_name.len() <= COORDINATION_BOARD_MAX_AUTHOR_NAME_BYTES);
    assert!(
        author_name.starts_with("Review "),
        "control characters should become readable separators: {author_name:?}"
    );

    let receipt = state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id,
            key: "author.snapshot".to_owned(),
            value: Some(json!(true)),
            expected_revision: 0,
            author_session_id: session_id,
            author_name: author_name.clone(),
            idempotency_key: "normalized-author".to_owned(),
            state_stamp: None,
        })
        .expect("legacy display names must not block an otherwise valid board write");
    assert_eq!(receipt.author_name, author_name);
}

// Pins the HTTP wire boundary for the value/delete discriminator: explicit
// JSON null is a SET (the custom deserializer preserves it), absent value
// with `delete: true` is a DELETE — the store only ever sees the already
// disambiguated Option, so this is the only layer that can prove the JSON
// contract (surface review, mailbox #237).
#[tokio::test]
async fn board_set_route_treats_explicit_null_as_a_set_and_delete_as_a_delete() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let app = app_router(state.clone());

    let (status, receipt): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": null,
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-null-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(receipt["value"], Value::Null);
    assert_eq!(
        receipt["deleted"],
        Value::Bool(false),
        "null is a value, not a delete"
    );
    assert_eq!(receipt["revision"], json!(1));

    let (status, duplicate): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": null,
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-null-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an idempotent replay must not claim to create the key again"
    );
    assert_eq!(duplicate["duplicate"], Value::Bool(true));
    assert_eq!(duplicate["revision"], json!(1));

    let (status, receipt): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "delete": true,
                    "expectedRevision": 1,
                    "idempotencyKey": "wire-delete-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(receipt["deleted"], Value::Bool(true));
    assert_eq!(receipt["revision"], json!(2));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn board_set_route_rejects_ambiguous_value_delete_combinations() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let app = app_router(state.clone());

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": 1,
                    "delete": true,
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-both-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("mutually exclusive")),
        "both value and delete must be rejected: {body}"
    );

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-neither-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("JSON null is a value")),
        "neither value nor delete must be rejected: {body}"
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn board_set_route_reports_authorization_before_mutation_shape() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("session should exist");
        inner.sessions[index].session.parent_delegation_id =
            Some("delegation-board-child".to_owned());
    }
    let app = app_router(state);

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "expectedRevision": 0,
                    "idempotencyKey": "unauthorized-malformed"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("local root session")),
        "authorization should be the strongest applicable rejection: {body}"
    );
}

#[tokio::test]
async fn board_set_route_maps_a_cas_conflict_to_409_with_detail() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let app = app_router(state.clone());

    let (status, _): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": { "round": 1 },
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-seed-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": { "round": 2 },
                    "expectedRevision": 5,
                    "idempotencyKey": "wire-conflict-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let message = body["error"].as_str().unwrap_or_default();
    assert!(
        message.contains("revision conflict") && message.contains("detail"),
        "conflict must carry current-head detail for CAS repair: {body}"
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn board_get_and_list_routes_pin_pagination_unchanged_and_tombstone_contracts() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let (project_id, author_name) =
        resolve_board_scope_for_session(&state, &session_id).expect("board scope should resolve");
    for (key, idempotency_key) in [
        ("status.alpha", "wire-list-alpha"),
        ("status.beta", "wire-list-beta"),
    ] {
        state
            .coordination_board_store
            .set(&CoordinationBoardSetInput {
                scope_project_id: project_id.clone(),
                key: key.to_owned(),
                value: Some(json!({ "ready": true })),
                expected_revision: 0,
                author_session_id: session_id.clone(),
                author_name: author_name.clone(),
                idempotency_key: idempotency_key.to_owned(),
                state_stamp: None,
            })
            .expect("board fixture should persist");
    }
    let app = app_router(state.clone());

    let (status, first_page): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/sessions/{session_id}/board?limit=1"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first_page["generation"], json!(2));
    assert_eq!(first_page["entries"][0]["key"], json!("status.alpha"));
    assert_eq!(first_page["nextAfterKey"], json!("status.alpha"));

    let (status, second_page): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!(
                "/api/sessions/{session_id}/board?afterKey=status.alpha&snapshotGeneration=2&limit=1"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_page["generation"], json!(2));
    assert_eq!(second_page["entries"][0]["key"], json!("status.beta"));
    assert!(second_page.get("nextAfterKey").is_none());

    let (status, unchanged): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!(
                "/api/sessions/{session_id}/board?knownGeneration=2"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unchanged["unchanged"], Value::Bool(true));
    assert_eq!(unchanged["entries"], json!([]));

    let (status, head): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/sessions/{session_id}/board/keys/status.beta"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head["key"], json!("status.beta"));
    assert_eq!(head["revision"], json!(1));
    assert_eq!(
        head["updatedAtGeneration"],
        json!(2),
        "the head must expose when this key was last written"
    );
    assert_eq!(
        head["scopeGeneration"],
        json!(2),
        "get must separately expose the current whole-scope generation"
    );

    state
        .coordination_board_store
        .set(&CoordinationBoardSetInput {
            scope_project_id: project_id,
            key: "status.beta".to_owned(),
            value: None,
            expected_revision: 1,
            author_session_id: session_id.clone(),
            author_name,
            idempotency_key: "wire-get-tombstone".to_owned(),
            state_stamp: None,
        })
        .expect("board tombstone should persist");
    let (status, tombstone_error): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/sessions/{session_id}/board/keys/status.beta"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        tombstone_error["error"]
            .as_str()
            .is_some_and(|message| message.contains("\"deleted\":true")),
        "tombstone 404 should carry reconciliation detail: {tombstone_error}"
    );
}

#[tokio::test]
async fn board_list_route_maps_query_rejections_to_api_error_json() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let app = app_router(state);

    for query in ["limit=abc", "unknownField=1"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/sessions/{session_id}/board?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|message| message.contains("invalid coordination board list query")),
            "query rejection must use the ApiError JSON envelope: {body}"
        );
    }
}

#[tokio::test]
async fn board_set_route_maps_json_rejections_to_api_error_json() {
    let (state, session_id, _board_temp_root) = board_route_test_state();
    let app = app_router(state);
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.gate",
                    "value": true,
                    "expectedRevision": 0,
                    "idempotencyKey": "json-rejection",
                    "stateStamps": "typo"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("invalid coordination board set JSON")),
        "JSON rejection must use the ApiError envelope: {body}"
    );
}

#[tokio::test]
async fn board_set_route_preserves_retryable_503_no_commit_guidance() {
    let (base_state, session_id, board_temp_root) = board_route_test_state();
    let coordination_path = board_temp_root.database_path();
    let state = AppState {
        coordination_board_store: Arc::new(
            CoordinationBoardStore::open_with_write_admission_timeout(
                &coordination_path,
                Duration::ZERO,
            )
            .expect("zero-deadline board store should open"),
        ),
        ..base_state
    };
    let writer_lock = sqlite_state_write_lock(&coordination_path);
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = lock_sqlite_state_writer(&writer_lock);
        locked_tx
            .send(())
            .expect("writer-lock observer should remain connected");
        release_rx
            .recv()
            .expect("writer-lock holder should be released");
    });
    locked_rx
        .recv()
        .expect("test should hold the shared coordination writer boundary");

    let app = app_router(state);
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/board/set"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "key": "status.busy",
                    "value": true,
                    "expectedRevision": 0,
                    "idempotencyKey": "wire-busy-1"
                }))
                .unwrap(),
            ))
            .unwrap(),
    )
    .await;

    release_tx
        .send(())
        .expect("writer-lock holder should release");
    holder.join().expect("writer-lock holder should join");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body["error"].as_str().is_some_and(|message| {
            message.contains(
                "no coordination board write was committed by this operation, so retry the same request",
            )
        }),
        "503 must preserve the exact safe-retry contract used by the MCP bridge: {body}"
    );
}
