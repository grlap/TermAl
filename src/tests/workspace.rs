//! Workspace layout HTTP routes and workspace file-watch scope tests.
//!
//! A "workspace layout" is the UI's pane tree plus presentation knobs
//! (control-panel side, theme, font sizes, density) and the ids of the
//! sessions/projects pinned into each pane. The frontend PUTs the full
//! document to `/api/workspaces/{id}` on every edit, GETs it on reload,
//! and lists all saved layouts via `/api/workspaces`; persistence is what
//! lets a browser refresh restore the exact pane arrangement.
//!
//! A `WorkspaceFileWatchScope` is a `(root_path, Option<session_id>)` pair
//! the backend hands to `notify`: project roots watch with no session, and
//! each local session workdir watches with its own session id. The minimum
//! scope set is "one scope per distinct (root, session) tuple" so we never
//! register duplicate watchers or emit duplicate change events.
//!
//! `merge_workspace_file_change_kind` coalesces bursts within the debounce
//! window: a delete immediately followed by a create (or vice versa) is
//! folded into a single Modified event so editors that save via rename
//! look like a normal edit. `prune_nested_workspace_file_watch_roots`
//! drops any root that lives under another already-watched root, because
//! `notify` runs recursively and a nested root would double-emit every
//! change. Session-scoped hints get attached when a path falls under a
//! session workdir so the event routes to the right session transcript
//! instead of firing as a generic project-root fallback.

use super::*;

// Pins the PUT/GET/list contract for a workspace layout: create stamps
// revision=1, update stamps revision=2 with the new body echoed back,
// subsequent GET and list entries must agree on revision and updated_at.
// Guards against regressions in round-trip fidelity of the full document
// (panes, side, theme, style, font sizes, density).
#[tokio::test]
async fn workspace_layout_routes_round_trip_put_get_and_list() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let initial_workspace = json!({
        "panes": [
            {
                "id": "pane-1",
                "kind": "session",
                "sessionId": "session-1"
            }
        ]
    });
    let initial_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "themeId": "terminal",
        "styleId": "style-terminal",
        "fontSizePx": 14,
        "editorFontSizePx": 15,
        "densityPercent": 90,
        "workspace": initial_workspace.clone()
    }))
    .expect("workspace layout body should serialize");
    let (create_status, create_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(initial_body))
            .unwrap(),
    )
    .await;

    assert_eq!(create_status, StatusCode::OK);
    assert_eq!(create_response.layout.id, "workspace-1");
    assert_eq!(create_response.layout.revision, 1);
    assert_eq!(
        create_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Left
    );
    assert_eq!(create_response.layout.theme_id.as_deref(), Some("terminal"));
    assert_eq!(
        create_response.layout.style_id.as_deref(),
        Some("style-terminal")
    );
    assert_eq!(create_response.layout.font_size_px, Some(14));
    assert_eq!(create_response.layout.editor_font_size_px, Some(15));
    assert_eq!(create_response.layout.density_percent, Some(90));
    assert_eq!(create_response.layout.workspace, initial_workspace);
    assert!(!create_response.layout.updated_at.is_empty());

    let updated_workspace = json!({
        "activePaneId": "pane-2",
        "panes": [
            {
                "id": "pane-2",
                "kind": "source",
                "sourcePath": "src/lib.rs"
            }
        ]
    });
    let update_body = serde_json::to_vec(&json!({
        "controlPanelSide": "right",
        "themeId": "frost",
        "styleId": "style-editorial",
        "fontSizePx": 16,
        "editorFontSizePx": 17,
        "densityPercent": 110,
        "workspace": updated_workspace.clone()
    }))
    .expect("updated workspace layout body should serialize");
    let (update_status, update_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(update_body))
            .unwrap(),
    )
    .await;

    assert_eq!(update_status, StatusCode::OK);
    assert_eq!(update_response.layout.id, "workspace-1");
    assert_eq!(update_response.layout.revision, 2);
    assert_eq!(
        update_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(update_response.layout.theme_id.as_deref(), Some("frost"));
    assert_eq!(
        update_response.layout.style_id.as_deref(),
        Some("style-editorial")
    );
    assert_eq!(update_response.layout.font_size_px, Some(16));
    assert_eq!(update_response.layout.editor_font_size_px, Some(17));
    assert_eq!(update_response.layout.density_percent, Some(110));
    assert_eq!(update_response.layout.workspace, updated_workspace.clone());
    assert!(!update_response.layout.updated_at.is_empty());

    let (get_status, get_response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(get_response.layout.id, "workspace-1");
    assert_eq!(get_response.layout.revision, 2);
    assert_eq!(
        get_response.layout.control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(get_response.layout.theme_id.as_deref(), Some("frost"));
    assert_eq!(
        get_response.layout.style_id.as_deref(),
        Some("style-editorial")
    );
    assert_eq!(get_response.layout.font_size_px, Some(16));
    assert_eq!(get_response.layout.editor_font_size_px, Some(17));
    assert_eq!(get_response.layout.density_percent, Some(110));
    assert_eq!(get_response.layout.workspace, updated_workspace);
    assert!(!get_response.layout.updated_at.is_empty());
    assert_eq!(
        get_response.layout.updated_at,
        update_response.layout.updated_at
    );

    let (list_status, list_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_response.workspaces.len(), 1);
    let summary = &list_response.workspaces[0];
    assert_eq!(summary.id, "workspace-1");
    assert_eq!(summary.revision, 2);
    assert_eq!(summary.control_panel_side, WorkspaceControlPanelSide::Right);
    assert_eq!(summary.theme_id.as_deref(), Some("frost"));
    assert_eq!(summary.style_id.as_deref(), Some("style-editorial"));
    assert_eq!(summary.font_size_px, Some(16));
    assert_eq!(summary.editor_font_size_px, Some(17));
    assert_eq!(summary.density_percent, Some(110));
    assert!(!summary.updated_at.is_empty());
    assert_eq!(summary.updated_at, get_response.layout.updated_at);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins ordering of `/api/workspaces`: newest `updated_at` first, with
// ties broken by ascending workspace id so the list is deterministic.
// Guards against regressions where the frontend's "recent workspaces"
// surface would shuffle or lose its tie-break rule.
#[tokio::test]
async fn workspace_layout_list_route_orders_workspaces_by_updated_at_desc() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let layout_body = |side: &str| {
        serde_json::to_vec(&json!({
            "controlPanelSide": side,
            "workspace": { "panes": [] }
        }))
        .expect("workspace layout body should serialize")
    };

    for workspace_id in ["workspace-b", "workspace-a", "workspace-c"] {
        let (status, _response): (StatusCode, WorkspaceLayoutResponse) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header("content-type", "application/json")
                .body(Body::from(layout_body("left")))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .workspace_layouts
            .get_mut("workspace-b")
            .expect("workspace-b should exist")
            .updated_at = "2026-04-02 08:30:00".to_owned();
        inner
            .workspace_layouts
            .get_mut("workspace-a")
            .expect("workspace-a should exist")
            .updated_at = "2026-04-02 08:30:00".to_owned();
        inner
            .workspace_layouts
            .get_mut("workspace-c")
            .expect("workspace-c should exist")
            .updated_at = "2026-04-03 09:45:00".to_owned();
    }

    let (status, response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // Matching timestamps fall back to ascending workspace ID, so the tied
    // 2026-04-02 entries must appear as `workspace-a` before `workspace-b`.
    assert_eq!(
        response
            .workspaces
            .iter()
            .map(|workspace| workspace.id.as_str())
            .collect::<Vec<_>>(),
        vec!["workspace-c", "workspace-a", "workspace-b"]
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins DELETE semantics: removing one layout returns the remaining
// summaries in the response body, and a follow-up GET on the deleted id
// returns 404 with the canonical "workspace layout not found" error.
// Guards against leaving stale documents in memory after deletion.
#[tokio::test]
async fn delete_workspace_layout_route_removes_saved_workspace() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let layout_body = serde_json::to_vec(&json!({
        "controlPanelSide": "left",
        "workspace": { "panes": [] }
    }))
    .expect("workspace layout body should serialize");

    for workspace_id in ["workspace-1", "workspace-2"] {
        let (status, _response): (StatusCode, WorkspaceLayoutResponse) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header("content-type", "application/json")
                .body(Body::from(layout_body.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (delete_status, delete_response): (StatusCode, WorkspaceLayoutsResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_response.workspaces.len(), 1);
    assert_eq!(delete_response.workspaces[0].id, "workspace-2");

    let (get_status, get_error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/workspace-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(get_status, StatusCode::NOT_FOUND);
    assert_eq!(get_error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that DELETE on an unknown workspace id returns 404 with the
// standard error payload rather than silently succeeding.
// Guards against accidental idempotent deletes masking client bugs.
#[tokio::test]
async fn delete_workspace_layout_route_returns_not_found_for_missing_id() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let (status, error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/workspaces/missing-workspace")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that GET on an unknown workspace id returns 404 with the same
// error payload shape as DELETE, so the frontend reload path can
// distinguish "never saved" from other failures.
// Guards against falling back to empty defaults or 500 responses.
#[tokio::test]
async fn get_workspace_layout_route_returns_not_found_for_missing_id() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let (status, error): (StatusCode, ErrorResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri("/api/workspaces/missing-workspace")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.error, "workspace layout not found");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins 422 rejection for three failure modes: missing controlPanelSide,
// missing workspace, and an invalid controlPanelSide enum variant. Each
// rejection must name the offending field so clients can surface it.
// Guards against silently storing partial or malformed layouts.
#[tokio::test]
async fn put_workspace_layout_route_rejects_malformed_payloads() {
    let state = test_app_state();
    let app = app_router(state.clone());
    let missing_control_panel_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "workspace": {} }))
                    .expect("missing-control-panel workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        missing_control_panel_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let missing_control_panel_body =
        to_bytes(missing_control_panel_response.into_body(), usize::MAX)
            .await
            .expect("missing-control-panel rejection body should read");
    let missing_control_panel_text = String::from_utf8(missing_control_panel_body.to_vec())
        .expect("missing-control-panel rejection body should be UTF-8");
    assert!(missing_control_panel_text.contains("missing field"));
    assert!(missing_control_panel_text.contains("controlPanelSide"));

    let missing_workspace_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "controlPanelSide": "left" }))
                    .expect("missing-workspace workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        missing_workspace_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let missing_workspace_body = to_bytes(missing_workspace_response.into_body(), usize::MAX)
        .await
        .expect("missing-workspace rejection body should read");
    let missing_workspace_text = String::from_utf8(missing_workspace_body.to_vec())
        .expect("missing-workspace rejection body should be UTF-8");
    assert!(missing_workspace_text.contains("missing field"));
    assert!(missing_workspace_text.contains("missing field `workspace`"));

    let invalid_enum_response = request_response(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/workspaces/workspace-1")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "controlPanelSide": "middle",
                    "workspace": {}
                }))
                .expect("invalid-enum workspace body should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        invalid_enum_response.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let invalid_enum_body = to_bytes(invalid_enum_response.into_body(), usize::MAX)
        .await
        .expect("invalid-enum rejection body should read");
    let invalid_enum_text = String::from_utf8(invalid_enum_body.to_vec())
        .expect("invalid-enum rejection body should be UTF-8");
    assert!(invalid_enum_text.contains("unknown variant"));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a second PUT on the same workspace bumps both the layout's
// own revision and the global state revision, and publishes a fresh
// StateResponse snapshot on the event stream so subscribed clients
// observe the update.
// Guards against stale caches or lost-update races on workspace edits.
#[test]
fn updating_existing_workspace_layout_advances_global_revision_and_publishes_state() {
    let state = test_app_state();
    state
        .put_workspace_layout(
            "workspace-1",
            PutWorkspaceLayoutRequest {
                control_panel_side: WorkspaceControlPanelSide::Left,
                theme_id: None,
                style_id: None,
                font_size_px: None,
                editor_font_size_px: None,
                density_percent: None,
                workspace: json!({ "panes": [] }),
            },
        )
        .expect("initial workspace layout should save");

    let revision_after_create = state.inner.lock().expect("state mutex poisoned").revision;
    let mut state_events = state.subscribe_events();
    let updated = state
        .put_workspace_layout(
            "workspace-1",
            PutWorkspaceLayoutRequest {
                control_panel_side: WorkspaceControlPanelSide::Right,
                theme_id: Some("ink".to_owned()),
                style_id: None,
                font_size_px: None,
                editor_font_size_px: None,
                density_percent: None,
                workspace: json!({
                    "panes": [
                        {
                            "id": "pane-1",
                            "tabs": []
                        }
                    ]
                }),
            },
        )
        .expect("updated workspace layout should save");

    let published_revision = state.inner.lock().expect("state mutex poisoned").revision;
    assert_eq!(updated.layout.revision, 2);
    assert_eq!(published_revision, revision_after_create + 1);

    let published_state: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("workspace update should publish a state snapshot"),
    )
    .expect("state event should decode");
    assert_eq!(published_state.revision, published_revision);
    assert_eq!(published_state.workspaces.len(), 1);
    assert_eq!(published_state.workspaces[0].id, "workspace-1");
    assert_eq!(published_state.workspaces[0].revision, 2);
    assert_eq!(
        published_state.workspaces[0].control_panel_side,
        WorkspaceControlPanelSide::Right
    );
    assert_eq!(
        state
            .get_workspace_layout("workspace-1")
            .expect("saved workspace layout should be readable")
            .layout
            .revision,
        2
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}


// Pins that a Delete+Create pair in either order folds into Modified
// when coalesced inside the debounce window.
// Guards against misreporting atomic-rename saves (common on editors
// that write to a tempfile and rename over the target) as delete/create
// churn instead of a single modification.
#[test]
fn merge_workspace_file_change_kind_treats_delete_create_as_modified() {
    assert_eq!(
        merge_workspace_file_change_kind(
            WorkspaceFileChangeKind::Deleted,
            WorkspaceFileChangeKind::Created,
        ),
        WorkspaceFileChangeKind::Modified,
    );
    assert_eq!(
        merge_workspace_file_change_kind(
            WorkspaceFileChangeKind::Created,
            WorkspaceFileChangeKind::Deleted,
        ),
        WorkspaceFileChangeKind::Modified,
    );
}

fn canonical_test_watch_path(path: &FsPath) -> PathBuf {
    normalize_user_facing_path(&fs::canonicalize(path).expect("test path should canonicalize"))
}

// Pins that collect_workspace_file_watch_scopes emits an unscoped
// entry for every local project root and a session-scoped entry for
// every local session workdir.
// Guards against losing watch coverage for either projects or sessions,
// which would make the frontend miss file-change notifications.
#[test]
fn workspace_file_watch_scopes_include_project_and_session_roots() {
    let state = test_app_state();
    let root = std::env::temp_dir().join(format!("termal-watch-scopes-{}", Uuid::new_v4()));
    let project_root = root.join("project");
    let session_root = root.join("session");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&session_root).unwrap();

    create_test_project(&state, &project_root, "Watch Project");
    let session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Watch Session".to_owned()),
            session_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        record.session.id
    };

    let scopes = collect_workspace_file_watch_scopes(&state)
        .into_iter()
        .map(|scope| (scope.root_path, scope.session_id))
        .collect::<Vec<_>>();

    assert!(scopes.contains(&(canonical_test_watch_path(&project_root), None)));
    assert!(scopes.contains(&(canonical_test_watch_path(&session_root), Some(session_id),)));

    fs::remove_dir_all(root).unwrap();
}

// Pins that a root contained inside another root gets dropped
// regardless of input order, leaving only the outermost root.
// Guards against registering two recursive `notify` watchers on
// overlapping trees, which would double-emit every change event.
#[test]
fn workspace_file_watch_roots_prune_nested_roots() {
    let root = std::env::temp_dir().join(format!("termal-watch-nested-{}", Uuid::new_v4()));
    let nested = root.join("packages").join("app");
    fs::create_dir_all(&nested).unwrap();
    let root = canonical_test_watch_path(&root);
    let nested = canonical_test_watch_path(&nested);

    assert_eq!(
        prune_nested_workspace_file_watch_roots(vec![nested.clone(), root.clone()]),
        vec![root.clone()],
    );
    assert_eq!(
        prune_nested_workspace_file_watch_roots(vec![root.clone(), nested]),
        vec![root.clone()],
    );
    fs::remove_dir_all(root).unwrap();
}

// Pins that a changed path under overlapping scopes emits one event per
// unique (root, session) pair, favoring the deepest matching root and
// deduping exact-duplicate scopes.
// Guards against duplicate session notifications and against routing a
// session-scoped change to the wrong session transcript.
#[test]
fn workspace_file_changes_from_path_uses_specific_unique_scopes() {
    let root = std::env::temp_dir().join(format!("termal-watch-change-{}", Uuid::new_v4()));
    let nested = root.join("packages").join("app");
    let changed_file = nested.join("src").join("main.rs");
    fs::create_dir_all(changed_file.parent().unwrap()).unwrap();
    fs::write(&changed_file, "fn main() {}\n").unwrap();
    let root_path = canonical_test_watch_path(&root);
    let nested_path = canonical_test_watch_path(&nested);
    let changed_path = canonical_test_watch_path(&changed_file)
        .to_string_lossy()
        .into_owned();

    let changes = workspace_file_changes_from_path(
        &changed_file,
        WorkspaceFileChangeKind::Modified,
        &[
            WorkspaceFileWatchScope {
                root_path: root_path.clone(),
                session_id: None,
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-1".to_owned()),
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-1".to_owned()),
            },
            WorkspaceFileWatchScope {
                root_path: nested_path.clone(),
                session_id: Some("session-2".to_owned()),
            },
        ],
    );

    assert_eq!(changes.len(), 3);
    assert!(changes.iter().all(|change| change.path == changed_path));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(nested_path.to_string_lossy().as_ref())
            && change.session_id.as_deref() == Some("session-1")
    }));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(nested_path.to_string_lossy().as_ref())
            && change.session_id.as_deref() == Some("session-2")
    }));
    assert!(changes.iter().any(|change| {
        change.root_path.as_deref() == Some(root_path.to_string_lossy().as_ref())
            && change.session_id.is_none()
    }));

    fs::remove_dir_all(root).unwrap();
}

// Pins that when no watch scope matches a path the event still fires
// once with `root_path = None` and `session_id = None`.
// Guards against silently dropping changes for files that briefly fall
// outside known roots (e.g. generated files, mid-reconcile scopes).
#[test]
fn workspace_file_changes_from_path_emits_unscoped_fallback() {
    let root = std::env::temp_dir().join(format!("termal-watch-fallback-{}", Uuid::new_v4()));
    let changed_file = root.join("generated.rs");
    fs::create_dir_all(&root).unwrap();
    fs::write(&changed_file, "fn generated() {}\n").unwrap();
    let changed_path = canonical_test_watch_path(&changed_file)
        .to_string_lossy()
        .into_owned();

    let changes =
        workspace_file_changes_from_path(&changed_file, WorkspaceFileChangeKind::Created, &[]);

    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, changed_path);
    assert_eq!(changes[0].kind, WorkspaceFileChangeKind::Created);
    assert_eq!(changes[0].root_path, None);
    assert_eq!(changes[0].session_id, None);

    fs::remove_dir_all(root).unwrap();
}

