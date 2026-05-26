//! Read-only delegation enforcement tests split from `delegations.rs`.
//!
//! This module owns delegated child write blocking, project/workdir scope
//! containment, approval rejection, and read-only Cursor plan-mode coverage. It
//! deliberately does not own wait fan-in, persistence, or result recovery tests.

use super::delegation_support::test_app_state_with_drained_delegation_codex_runtime;
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-read-only-runtime")
}

fn assert_read_only_delegation_error(value: &Value, title: &str) {
    let error = value["error"]
        .as_str()
        .expect("error response should include a message");
    assert!(error.contains("disabled for read-only delegated sessions"));
    assert!(error.contains(&format!("while read-only delegation `{title}` is running")));
}

fn install_delegation_acp_runtime(
    state: &AppState,
    agent: AcpAgent,
    runtime_id: &str,
) -> mpsc::Receiver<AcpRuntimeCommand> {
    let (runtime, input_rx) = test_acp_runtime_handle(agent, runtime_id);
    state.install_test_acp_runtime_override(agent, runtime);
    input_rx
}

#[tokio::test]
async fn read_only_delegation_blocks_write_capable_surfaces() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate.".to_owned(),
                title: Some("Read-only Enforcement".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("try rename".to_owned()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("read-only child settings update should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "path": "blocked.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let ssh_remote = RemoteConfig {
        id: "ssh-read-only-guard".to_owned(),
        name: "SSH Read-only Guard".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &ssh_remote,
        "/remote/read-only-guard",
        "Remote Read-only Guard",
        "remote-read-only-guard",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let proxy_remote = RemoteConfig {
        id: "remote-read-only-bypass".to_owned(),
        name: "Remote Read-only Bypass".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("127.0.0.1".to_owned()),
        port: Some(22),
        user: None,
    };
    let proxy_remote_project_id = create_test_remote_project(
        &state,
        &proxy_remote,
        "/remote/read-only-bypass",
        "Remote Read-only Bypass",
        "remote-project-read-only-bypass",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": proxy_remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked before proxy",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let child_session_id = created.delegation.child_session_id.clone();
    let child_workdir = created.child_session.workdir.clone();
    for (route, body) in [
        (
            "/api/git/file",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "action": "stage",
                "path": "remote-bypass.txt"
            }),
        ),
        (
            "/api/git/commit",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "message": "blocked commit"
            }),
        ),
        (
            "/api/git/push",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
        (
            "/api/git/sync",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
    ] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let review = default_review_document("read-only-review-guard");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/reviews/read-only-review-guard?sessionId={}&projectId={}",
                child_session_id, proxy_remote_project_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&review).expect("review document should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": child_session_id.as_str(),
                        "workdir": child_workdir.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": child_session_id.as_str(),
                        "projectId": proxy_remote_project_id.as_str(),
                        "workdir": child_workdir.as_str(),
                        "command": "echo blocked before proxy"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_expired_child_link_uses_fallback_error_wording() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Leave a stale child link.".to_owned(),
                title: Some("Expired Link".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .delegations
            .retain(|delegation| delegation.id != created.delegation.id);
        state.commit_locked(&mut inner).unwrap();
    }

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "path": "blocked-expired.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("while an expired read-only delegation is still attached")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_delegation_missing_child_session_still_blocks_scope_writes() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-missing-child-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Missing Child Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Keep blocking this scope if the child disappears.".to_owned(),
                title: Some("Missing Child Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        inner.remove_session_at(child_index);
        state.commit_locked(&mut inner).unwrap();
    }

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&project_id),
            None,
            "project writes",
        )
        .expect_err("missing child session should not make the delegation fail open");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Missing Child Delegation` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_project_writes_without_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate project scope.".to_owned(),
                title: Some("Read-only Project Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let blocked_file = project_root.join("blocked.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": project_id,
                    "path": blocked_file.to_string_lossy(),
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Project Scope");
    assert!(!blocked_file.exists());

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("workdir-only writes should be blocked inside read-only delegation scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let missing_git_workdir = project_root
        .join("missing-git-worktree")
        .to_string_lossy()
        .into_owned();
    for git_route in ["/api/git/push", "/api/git/sync"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(git_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "projectId": project_id.as_str(),
                        "workdir": missing_git_workdir.as_str()
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "projectId": project_id.as_str(),
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn read_only_delegation_blocks_bidirectional_workdir_containment() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-workdir-scope-{}", Uuid::new_v4()));
    let delegated_dir = project_root.join("delegated");
    let nested_write_dir = delegated_dir.join("nested");
    fs::create_dir_all(&nested_write_dir).expect("nested workdir should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Workdir Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate nested project scope.".to_owned(),
                title: Some("Read-only Nested Scope".to_owned()),
                cwd: Some(delegated_dir_label.clone()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let nested_write_dir_label = nested_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(nested_write_dir_label.as_str()),
            "nested workdir write",
        )
        .expect_err("writes below the delegated workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "ancestor workdir write",
        )
        .expect_err("writes from an ancestor workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let sibling_write_dir = project_root.join("sibling");
    fs::create_dir_all(&sibling_write_dir).expect("sibling workdir should exist");
    let sibling_write_dir_label = sibling_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(sibling_write_dir_label.as_str()),
            "sibling workdir write",
        )
        .expect_err("workdir-only writes should inherit the enclosing project root");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn delegation_write_scope_does_not_match_empty_workdir() {
    for workdir in ["", "   \t"] {
        let scope = DelegationWriteScope {
            title: "Empty Workdir".to_owned(),
            project_id: None,
            workdir: workdir.to_owned(),
        };

        assert!(!delegation_write_scope_matches(
            &scope,
            None,
            Some("/tmp/termal-empty-workdir")
        ));
    }
}

#[tokio::test]
async fn read_only_delegation_blocks_git_repo_root_writes_from_sibling_workdir() {
    let state = test_app_state();
    let repo_root =
        std::env::temp_dir().join(format!("termal-read-only-git-root-{}", Uuid::new_v4()));
    let delegated_dir = repo_root.join("delegated");
    let sibling_dir = repo_root.join("sibling");
    fs::create_dir_all(&delegated_dir).expect("delegated workdir should exist");
    fs::create_dir_all(&sibling_dir).expect("sibling workdir should exist");
    init_git_document_test_repo(&repo_root);
    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Git Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate this repository.".to_owned(),
                title: Some("Read-only Git Root".to_owned()),
                cwd: Some(delegated_dir_label),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/git/commit")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "workdir": sibling_dir.to_string_lossy(),
                    "message": "blocked"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Git Root");

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_project_and_workdir_writes_with_parent_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-parent-scope-{}", Uuid::new_v4()));
    let unrelated_root = std::env::temp_dir().join(format!(
        "termal-read-only-unrelated-scope-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Parent Scope");
    let unrelated_project_id =
        create_test_project(&state, &unrelated_root, "Unrelated Project Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let sibling_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate through the parent.".to_owned(),
                title: Some("Read-only Parent Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    for (session_id, name) in [
        (parent_session_id.as_str(), "parent can still rename"),
        (sibling_session_id.as_str(), "sibling can still rename"),
    ] {
        state
            .update_session_settings(
                session_id,
                UpdateSessionSettingsRequest {
                    name: Some(name.to_owned()),
                    model: None,
                    approval_policy: None,
                    reasoning_effort: None,
                    sandbox_mode: None,
                    cursor_mode: None,
                    claude_approval_mode: None,
                    claude_effort: None,
                    gemini_approval_mode: None,
                },
            )
            .expect("parent/sibling settings should not inherit project-wide write blocking");
    }
    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("child rename stays blocked".to_owned()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("read-only child settings should stay blocked"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            Some(&project_id),
            None,
            "file writes",
        )
        .expect_err("parent session id should not bypass read-only project scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("parent session id should not bypass read-only workdir scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let unrelated_root_label = unrelated_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&sibling_session_id),
            Some(&unrelated_project_id),
            Some(unrelated_root_label.as_str()),
            "mixed-scope file writes",
        )
        .expect_err("explicit project/workdir should not hide the session-derived scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    for (session_id, file_name) in [
        (parent_session_id.as_str(), "parent-session-only.txt"),
        (sibling_session_id.as_str(), "sibling-session-only.txt"),
    ] {
        let blocked_file = project_root.join(file_name);
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/api/file")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": session_id,
                        "path": file_name,
                        "content": "blocked",
                        "overwrite": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Parent Scope");
        assert!(!blocked_file.exists());
    }

    let unrelated_file = unrelated_root.join("mixed-project.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": sibling_session_id,
                    "projectId": unrelated_project_id,
                    "path": "mixed-project.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Parent Scope");
    assert!(!unrelated_file.exists());

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(unrelated_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_overlapping_project_id_only_writes() {
    let state = test_app_state();
    let outer_root =
        std::env::temp_dir().join(format!("termal-read-only-outer-{}", Uuid::new_v4()));
    let inner_root = outer_root.join("inner");
    let outer_sibling_root = outer_root.join("sibling");
    let unrelated_root =
        std::env::temp_dir().join(format!("termal-read-only-disjoint-{}", Uuid::new_v4()));
    fs::create_dir_all(&inner_root).expect("inner project should exist");
    fs::create_dir_all(&outer_sibling_root).expect("outer sibling project should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project should exist");
    let outer_project_id = create_test_project(&state, &outer_root, "Read-only Outer");
    let inner_project_id = create_test_project(&state, &inner_root, "Read-only Inner");
    let unrelated_project_id = create_test_project(&state, &unrelated_root, "Unrelated Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &inner_project_id, &inner_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate overlapping project scope.".to_owned(),
                title: Some("Read-only Overlap".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            None,
            "overlapping project writes",
        )
        .expect_err("outer project id should block writes into nested delegated project");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let outer_sibling_root_label = outer_sibling_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            Some(outer_sibling_root_label.as_str()),
            "mixed project/workdir writes",
        )
        .expect_err("outer project root should stay checked when workdir is also supplied");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&unrelated_project_id),
            None,
            "unrelated project writes",
        )
        .expect("unrelated project id should not be blocked");

    let app = app_router(state.clone());
    let blocked_file = inner_root.join("blocked-overlap.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": outer_project_id,
                    "path": blocked_file.to_string_lossy(),
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Overlap");
    assert!(!blocked_file.exists());

    let allowed_file = unrelated_root.join("allowed.txt");
    let (status, _body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": unrelated_project_id,
                    "path": allowed_file.to_string_lossy(),
                    "content": "allowed",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fs::read_to_string(&allowed_file).expect("allowed write should persist"),
        "allowed"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(outer_root);
    let _ = fs::remove_dir_all(unrelated_root);
}

#[test]
fn read_only_delegation_rejects_approval_acceptance() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not approve writes.".to_owned(),
                title: Some("Approval Guard".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    for decision in [
        ApprovalDecision::Accepted,
        ApprovalDecision::AcceptedForSession,
    ] {
        let err = match state.update_approval(
            &created.delegation.child_session_id,
            "approval-1",
            decision,
        ) {
            Ok(_) => panic!("read-only child approval acceptance should be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    let err = match state.update_approval(
        &created.delegation.child_session_id,
        "approval-1",
        ApprovalDecision::Rejected,
    ) {
        Ok(_) => panic!("missing pending approval should still fail normally"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_cursor_delegation_uses_plan_mode() {
    let state = test_app_state();
    let acp_input_rx =
        install_delegation_acp_runtime(&state, AcpAgent::Cursor, "delegation-cursor-plan");
    let parent_session_id = test_session_id(&state, Agent::Cursor);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Inspect without editing.".to_owned(),
                title: Some("Cursor Plan Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Cursor),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("cursor delegation should be created");

    assert_eq!(
        created.delegation.write_policy,
        DelegationWritePolicy::ReadOnly
    );
    assert_eq!(created.delegation.status, DelegationStatus::Running);
    assert_eq!(created.child_session.status, SessionStatus::Active);
    assert_eq!(created.child_session.cursor_mode, Some(CursorMode::Plan));
    match acp_input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor delegation should dispatch through the fake ACP runtime")
    {
        AcpRuntimeCommand::Prompt(command) => {
            assert_eq!(command.cwd, created.delegation.cwd);
            assert_eq!(command.cursor_mode, Some(CursorMode::Plan));
            assert_eq!(command.model, created.child_session.model);
            assert_eq!(command.resume_session_id, None);
            assert!(
                command.prompt.contains("Inspect without editing."),
                "delegation prompt should include the requested task",
            );
        }
        _ => panic!("Cursor delegation should dispatch an ACP prompt command"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}
