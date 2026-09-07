use super::*;

fn create_labeled_workspace_fixture(state: &AppState) -> WorkspaceLayoutDocument {
    state
        .put_workspace_layout(
            "workspace-label-test",
            serde_json::from_value(json!({
                "controlPanelSide": "left",
                "workspace": { "panes": [{ "id": "saved-pane" }] }
            }))
            .unwrap(),
        )
        .unwrap()
        .layout
}

#[tokio::test]
async fn workspace_label_patch_preserves_layout_and_survives_autosave_and_reload() {
    let state = test_app_state();
    let original = create_labeled_workspace_fixture(&state);
    let app = app_router(state.clone());
    let mut events = state.subscribe_events();
    let (status, response): (StatusCode, WorkspaceLayoutResponse) = request_json(
        &app,
        Request::builder()
            .method("PATCH")
            .uri("/api/workspaces/workspace-label-test/label")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"label":"  Backend review  "}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.layout.label.as_deref(), Some("Backend review"));
    assert_eq!(response.layout.workspace, original.workspace);
    assert_eq!(response.layout.revision, original.revision + 1);
    let event: StateResponse = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
    assert_eq!(event.workspaces[0].label.as_deref(), Some("Backend review"));

    // A layout saved by an older tab carries no authoritative label.
    let saved = create_labeled_workspace_fixture(&state);
    assert_eq!(saved.label.as_deref(), Some("Backend review"));
    let reloaded = load_state(&state.persistence_path).unwrap().unwrap();
    assert_eq!(
        reloaded.workspace_layouts["workspace-label-test"]
            .label
            .as_deref(),
        Some("Backend review")
    );
    assert_eq!(
        state.list_workspace_layouts().unwrap().workspaces[0]
            .label
            .as_deref(),
        Some("Backend review")
    );

    state
        .patch_workspace_label(
            "workspace-label-test",
            PatchWorkspaceLabelRequest {
                label: "   ".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(
        state
            .get_workspace_layout("workspace-label-test")
            .unwrap()
            .layout
            .label,
        None
    );
    assert_eq!(
        load_state(&state.persistence_path)
            .unwrap()
            .unwrap()
            .workspace_layouts["workspace-label-test"]
            .label,
        None
    );
}

#[tokio::test]
async fn workspace_label_patch_rejects_invalid_labels_and_missing_workspaces() {
    let state = test_app_state();
    let original = create_labeled_workspace_fixture(&state);
    let app = app_router(state.clone());
    for (id, label, expected) in [
        (
            "workspace-label-test",
            "x".repeat(81),
            StatusCode::BAD_REQUEST,
        ),
        (
            "workspace-label-test",
            "bad\nlabel".to_owned(),
            StatusCode::BAD_REQUEST,
        ),
        ("missing", "Review".to_owned(), StatusCode::NOT_FOUND),
    ] {
        let response = request_response(
            &app,
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/workspaces/{id}/label"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "label": label })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), expected);
    }
    assert_eq!(
        state
            .get_workspace_layout("workspace-label-test")
            .unwrap()
            .layout,
        original
    );
    let response = request_response(
        &app,
        Request::builder()
            .method("PATCH")
            .uri("/api/workspaces/workspace-label-test/label")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn workspace_layouts_saved_without_labels_remain_readable() {
    let document: WorkspaceLayoutDocument = serde_json::from_value(json!({
        "id": "older-layout", "revision": 1, "updatedAt": "2026-09-06 19:00:00",
        "controlPanelSide": "left", "workspace": { "panes": [] }
    }))
    .unwrap();
    assert_eq!(document.label, None);
    let summary = collect_workspace_layout_summaries(std::iter::once(&document));
    assert_eq!(summary[0].label, None);
}
