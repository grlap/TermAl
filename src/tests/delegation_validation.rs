//! Delegation input and route validation tests split from `delegations.rs`.
//!
//! This module owns prompt, cwd, and unsupported route-mode rejection coverage.
//! It deliberately does not own metadata defaulting, delegation lifecycle, or
//! result recovery tests.

use super::delegation_support::test_app_state_with_drained_delegation_codex_runtime;
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-validation-runtime")
}

#[cfg(any(unix, windows))]
fn create_test_dir_symlink(target: &FsPath, link: &FsPath) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
}

#[cfg(windows)]
fn windows_symlink_privilege_unavailable(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(1314)
}

fn expect_delegation_cwd_rejected(
    state: &AppState,
    parent_session_id: &str,
    cwd: String,
) -> ApiError {
    match state.create_read_only_delegation(
        parent_session_id,
        CreateDelegationRequest {
            prompt: "Validate delegation cwd.".to_owned(),
            title: Some("Cwd Validation".to_owned()),
            cwd: Some(cwd),
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("invalid delegation cwd should be rejected"),
        Err(err) => err,
    }
}

#[test]
fn delegation_whitespace_prompt_is_rejected() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "   \t\n".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("whitespace-only prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("cannot be empty"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_rejects_traversal_cwd_outside_project() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-delegation-cwd-root-{}", Uuid::new_v4()));
    let inside_dir = project_root.join("inside");
    let outside_root =
        std::env::temp_dir().join(format!("termal-delegation-cwd-outside-{}", Uuid::new_v4()));
    fs::create_dir_all(&inside_dir).expect("project workdir should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Cwd Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &inside_dir);
    let traversal_cwd = inside_dir.join("..").join("..").join(
        outside_root
            .file_name()
            .expect("outside root should have name"),
    );

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        traversal_cwd.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_drive_relative_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let drive = std::env::current_dir()
        .expect("current dir should resolve")
        .components()
        .find_map(|component| match component {
            std::path::Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    Some((drive as char).to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or('C');
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        format!("{drive}:termal-drive-relative-cwd"),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("drive-relative"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_unc_cwd_before_metadata_lookup() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        r"\\server\share\termal-delegation-cwd".to_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("UNC"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_device_namespace_unc_alias_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        r"\\.\UNC\server\share\termal-delegation-cwd".to_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("device namespace"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_globalroot_and_mup_cwd() {
    for cwd in [
        r"\\?\GLOBALROOT\Device\Mup\server\share\termal-delegation-cwd",
        r"\\?\Mup\server\share\termal-delegation-cwd",
    ] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let err = expect_delegation_cwd_rejected(&state, &parent_session_id, cwd.to_owned());
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("device namespace"));

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_verbatim_drive_relative_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let drive = std::env::current_dir()
        .expect("current dir should resolve")
        .components()
        .find_map(|component| match component {
            std::path::Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    Some((drive as char).to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or('C');
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        format!(r"\\?\{drive}:termal-drive-relative-cwd"),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("drive-relative"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(unix)]
#[test]
fn delegation_rejects_symlinked_cwd_escape_from_project() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-root-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Symlink Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let link_path = project_root.join("outside-link");
    create_test_dir_symlink(&outside_root, &link_path).expect("test symlink should be created");

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        link_path.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_dir(&link_path);
    let _ = fs::remove_file(&link_path);
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_symlinked_cwd_escape_from_project() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-root-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Symlink Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let link_path = project_root.join("outside-link");
    if let Err(err) = create_test_dir_symlink(&outside_root, &link_path) {
        let _ = fs::remove_file(state.persistence_path.as_path());
        let _ = fs::remove_dir_all(project_root);
        let _ = fs::remove_dir_all(outside_root);
        if windows_symlink_privilege_unavailable(&err) {
            eprintln!("skipping Windows symlink cwd escape test: {err}");
            return;
        }
        panic!("test symlink should be created: {err}");
    }

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        link_path.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_dir(&link_path);
    let _ = fs::remove_file(&link_path);
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[tokio::test]
async fn delegation_route_rejects_remote_proxy_parent_without_local_project() {
    for (remote_id, remote_session_id) in [
        (Some("ssh-review"), Some("remote-parent-1")),
        (Some("ssh-review"), None),
        (None, Some("remote-parent-1")),
    ] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let parent_index = inner
                .find_visible_session_index(&parent_session_id)
                .expect("parent session should exist");
            let parent = inner
                .session_mut_by_index(parent_index)
                .expect("parent session index should be valid");
            parent.session.project_id = None;
            parent.remote_id = remote_id.map(str::to_owned);
            parent.remote_session_id = remote_session_id.map(str::to_owned);
        }

        let app = app_router(state.clone());
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{parent_session_id}/delegations"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"prompt":"review remote parent","writePolicy":{"kind":"readOnly"}}"#,
                ))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("delegations for remote-backed sessions are not implemented")
        );
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.delegations.is_empty());
        assert_eq!(inner.sessions.len(), 1);
        drop(inner);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[tokio::test]
async fn delegation_route_rejects_worker_and_shared_worktree_policy() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (worker_status, worker_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try worker","mode":"worker","writePolicy":{"kind":"readOnly"}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(worker_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        worker_body["error"]
            .as_str()
            .unwrap()
            .contains("worker delegations are not implemented")
    );

    let (policy_status, policy_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try write","writePolicy":{"kind":"sharedWorktree","ownedPaths":["src"]}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(policy_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        policy_body["error"]
            .as_str()
            .unwrap()
            .contains("sharedWorktree delegation write policy")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
