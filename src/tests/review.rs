// Review document persistence and HTTP route tests — atomic replace,
// directory-sync failure handling, Windows-specific replace fallback,
// change-set-id validation (empty, whitespace, overlong, invalid chars,
// dots-only), and HTTP handlers validating change-set IDs before remote
// proxying.
//
// Extracted from tests.rs — contiguous cluster (previously lines
// 3460-3975) covering both the `persist_review_document` /
// `resolve_review_document_path` validation helpers and the
// `review_handlers_validate_*` / `review_read_routes_wait_for_*` HTTP
// integration tests.

use super::*;

// Tests that review persistence replaces the target without leaving temp files behind.
#[test]
fn persist_review_document_replaces_target_without_leaving_temp_files() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-atomic-write-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let change_set_id = "change-set-atomic-write";
    let review_path = resolve_review_document_path(&review_root, change_set_id)
        .expect("review path should resolve");
    let initial_review = default_review_document(change_set_id);
    persist_review_document(&review_path, &initial_review)
        .expect("initial review document should persist");

    let mut updated_review = initial_review.clone();
    updated_review.revision = 1;
    persist_review_document(&review_path, &updated_review)
        .expect("updated review document should persist");

    let loaded_review =
        load_review_document(&review_path, change_set_id).expect("review document should load");
    assert_eq!(loaded_review, updated_review);

    let review_dir = review_path
        .parent()
        .expect("review file should have a parent");
    let mut entry_names = fs::read_dir(review_dir)
        .expect("review directory should list")
        .map(|entry| {
            entry
                .expect("review directory entry should read")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entry_names.sort();
    assert_eq!(entry_names, vec!["change-set-atomic-write.json".to_owned()]);

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that the Windows replace helper overwrites an existing target in place.
#[cfg(windows)]
#[test]
fn replace_review_document_file_replaces_existing_target_on_windows() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-windows-replace-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let review_path = review_root.join("review.json");
    let temp_path = review_root.join("review.tmp");

    fs::write(&review_path, b"original review").expect("existing review file should be written");
    fs::write(&temp_path, b"updated review").expect("temp review file should be written");

    replace_review_document_file(&temp_path, &review_path)
        .expect("existing review file should be replaced");

    assert_eq!(
        fs::read(&review_path).expect("replaced review file should read"),
        b"updated review"
    );
    assert!(
        !temp_path.exists(),
        "replacement temp file should be moved away"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that a directory-sync failure after replacement does not surface as a write failure.
#[test]
fn persist_review_document_succeeds_when_directory_sync_fails_after_replace() {
    let review_root = std::env::temp_dir().join(format!(
        "termal-review-directory-sync-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let change_set_id = "change-set-directory-sync";
    let review_path = resolve_review_document_path(&review_root, change_set_id)
        .expect("review path should resolve");
    let initial_review = default_review_document(change_set_id);
    persist_review_document_with_directory_sync(&review_path, &initial_review, |_| Ok(()))
        .expect("initial review document should persist");

    let mut updated_review = initial_review.clone();
    updated_review.revision = 1;
    let result = persist_review_document_with_directory_sync(&review_path, &updated_review, |_| {
        Err(ApiError::internal("simulated directory sync failure"))
    });
    assert!(
        result.is_ok(),
        "post-rename directory sync failures should not fail the write"
    );

    let loaded_review =
        load_review_document(&review_path, change_set_id).expect("review document should load");
    assert_eq!(loaded_review, updated_review);

    let review_dir = review_path
        .parent()
        .expect("review file should have a parent");
    let mut entry_names = fs::read_dir(review_dir)
        .expect("review directory should list")
        .map(|entry| {
            entry
                .expect("review directory entry should read")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    entry_names.sort();
    assert_eq!(
        entry_names,
        vec!["change-set-directory-sync.json".to_owned()]
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject empty values.
#[test]
fn resolve_review_document_path_rejects_empty_change_set_ids() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-empty-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "") {
        Ok(_) => panic!("empty change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, "changeSetId cannot be empty");

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject surrounding whitespace.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_with_surrounding_whitespace() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-whitespace-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, " change-set-whitespace ") {
        Ok(_) => panic!("surrounding whitespace should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may not have leading or trailing whitespace"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject overly long values.
#[test]
fn resolve_review_document_path_rejects_overlong_change_set_ids() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-long-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");
    let too_long_change_set_id = "a".repeat(MAX_REVIEW_CHANGE_SET_ID_LEN + 1);

    let error = match resolve_review_document_path(&review_root, &too_long_change_set_id) {
        Ok(_) => panic!("overlong change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        format!("changeSetId is too long (max {MAX_REVIEW_CHANGE_SET_ID_LEN} bytes)")
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review handlers validate change-set IDs before remote proxying.
#[tokio::test]
async fn review_handlers_validate_change_set_ids_before_remote_proxying() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-lab".to_owned(),
        name: "SSH Lab".to_owned(),
        transport: RemoteTransport::Local,
        enabled: true,
        host: None,
        port: None,
        user: None,
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let error = get_review(
        AxumPath(" change-set-remote ".to_owned()),
        Query(ReviewQuery {
            project_id: Some(project_id),
            session_id: None,
        }),
        State(state),
    )
    .await
    .expect_err("invalid remote review change-set ID should be rejected before proxying");

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may not have leading or trailing whitespace"
    );
}
// Tests that review change-set IDs reject invalid characters.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_with_invalid_characters() {
    let review_root =
        std::env::temp_dir().join(format!("termal-review-invalid-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "change/set-invalid") {
        Ok(_) => panic!("invalid-character change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId may only contain letters, numbers, '.', '-', and '_'"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review change-set IDs reject pure-dot values.
#[test]
fn resolve_review_document_path_rejects_change_set_ids_consisting_entirely_of_dots() {
    let review_root = std::env::temp_dir().join(format!("termal-review-dot-id-{}", Uuid::new_v4()));
    fs::create_dir_all(&review_root).expect("review root should exist");

    let error = match resolve_review_document_path(&review_root, "..") {
        Ok(_) => panic!("pure-dot change-set IDs should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "changeSetId must not consist entirely of dots"
    );

    let _ = fs::remove_dir_all(&review_root);
}

// Tests that review read routes wait for the review document lock.
#[tokio::test]
async fn review_read_routes_wait_for_review_document_lock() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-review-lock-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("review lock project root should exist");
    let project = state
        .create_project(CreateProjectRequest {
            name: Some("Review Lock Project".to_owned()),
            root_path: project_root.to_string_lossy().into_owned(),
            remote_id: default_local_remote_id(),
        })
        .expect("review lock project should be created");
    let project_id = project.project_id;
    let app = app_router(state.clone());
    let change_set_id = "change-set-locked-read";
    let review_guard = state
        .review_documents_lock
        .lock()
        .expect("review documents mutex poisoned");
    let review_app = app.clone();
    let review_future = request_json::<ReviewDocumentResponse>(
        &review_app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/reviews/{change_set_id}?projectId={project_id}"
            ))
            .body(Body::empty())
            .unwrap(),
    );
    tokio::pin!(review_future);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut review_future)
            .await
            .is_err()
    );
    let summary_app = app.clone();
    let summary_future = request_json::<ReviewSummaryResponse>(
        &summary_app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/reviews/{change_set_id}/summary?projectId={project_id}"
            ))
            .body(Body::empty())
            .unwrap(),
    );
    tokio::pin!(summary_future);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut summary_future)
            .await
            .is_err()
    );
    drop(review_guard);
    let (review_status, review_response) = review_future.await;
    assert_eq!(review_status, StatusCode::OK);
    assert_eq!(review_response.review.change_set_id, change_set_id);
    assert_eq!(review_response.review.revision, 0);
    assert!(review_response.review.files.is_empty());
    assert!(review_response.review.threads.is_empty());
    assert!(
        review_response
            .review_file_path
            .ends_with("change-set-locked-read.json")
    );
    let (summary_status, summary_response) = summary_future.await;
    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(summary_response.change_set_id, change_set_id);
    assert_eq!(summary_response.thread_count, 0);
    assert_eq!(summary_response.open_thread_count, 0);
    assert_eq!(summary_response.resolved_thread_count, 0);
    assert_eq!(summary_response.comment_count, 0);
    assert!(!summary_response.has_threads);
    assert!(
        summary_response
            .review_file_path
            .ends_with("change-set-locked-read.json")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(&project_root);
}

// Tests that update session settings route updates session name.
#[tokio::test]
async fn update_session_settings_route_updates_session_name() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "name": "Route Updated Session"
    }))
    .expect("settings route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/settings"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.name, "Route Updated Session");
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that send message route accepts and queues prompt for busy session.
#[tokio::test]
async fn send_message_route_accepts_and_queues_prompt_for_busy_session() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "text": "Queued route prompt",
        "expandedText": "Expanded queued route prompt"
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
        .expect("queued session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pending_prompts.len(), 1);
    assert_eq!(session.pending_prompts[0].text, "Queued route prompt");
    assert_eq!(
        session.pending_prompts[0].expanded_text.as_deref(),
        Some("Expanded queued route prompt")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
// Tests that submit approval route updates Claude session and delivers runtime response.
#[tokio::test]
async fn submit_approval_route_updates_claude_session_and_delivers_runtime_response() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-approval-route");
    let message_id = "approval-route-1".to_owned();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    state
        .push_message(
            &session_id,
            Message::Approval {
                id: message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Claude needs approval".to_owned(),
                command: "Edit src/main.rs".to_owned(),
                command_language: None,
                detail: "Need to update the route tests.".to_owned(),
                decision: ApprovalDecision::Pending,
            },
        )
        .expect("approval message should be recorded");
    state
        .register_claude_pending_approval(
            &session_id,
            message_id.clone(),
            ClaudePendingApproval {
                permission_mode_for_session: Some("acceptEdits".to_owned()),
                request_id: "claude-route-request".to_owned(),
                tool_input: json!({
                    "path": "src/main.rs"
                }),
            },
        )
        .expect("pending Claude approval should be registered");
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "decision": "acceptedForSession"
    }))
    .expect("approval route body should serialize");
    let (status, response): (StatusCode, StateResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session_id}/approvals/{message_id}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = response
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Claude session should be present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(
        session.preview,
        approval_preview_text("Claude", ApprovalDecision::AcceptedForSession)
    );
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Approval { id, decision, .. }
            if id == &message_id && *decision == ApprovalDecision::AcceptedForSession
    )));
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::SetPermissionMode(mode)) => {
            assert_eq!(mode, "acceptEdits");
        }
        Ok(_) => panic!("expected Claude permission-mode update"),
        Err(err) => panic!("Claude permission-mode update should arrive: {err}"),
    }
    match input_rx.recv_timeout(Duration::from_millis(50)) {
        Ok(ClaudeRuntimeCommand::PermissionResponse(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        })) => {
            assert_eq!(request_id, "claude-route-request");
            assert_eq!(updated_input, json!({ "path": "src/main.rs" }));
        }
        Ok(_) => panic!("expected Claude permission response"),
        Err(err) => panic!("Claude permission response should arrive: {err}"),
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(record.pending_claude_approvals.is_empty());
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
