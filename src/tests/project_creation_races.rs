//! Project-lifecycle races across local and remote create flows.
//!
//! These tests put deletion at explicit unlocked preflight/network boundaries
//! so stale project snapshots cannot recreate attached sessions or
//! orchestrators after the real deletion sweep has committed.

use super::*;

pub(super) fn remote_config(id: &str) -> RemoteConfig {
    RemoteConfig {
        id: id.to_owned(),
        name: format!("Remote {id}"),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    }
}

pub(super) fn remote_settings_request(remotes: Vec<RemoteConfig>) -> UpdateAppSettingsRequest {
    UpdateAppSettingsRequest {
        default_codex_model: None,
        default_claude_model: None,
        default_cursor_model: None,
        default_gemini_model: None,
        default_opencode_model: None,
        default_codex_reasoning_effort: None,
        default_codex_sandbox_mode: None,
        default_codex_approval_policy: None,
        default_claude_approval_mode: None,
        default_claude_effort: None,
        remotes: Some(remotes),
    }
}

pub(super) fn replace_remote_settings(state: &AppState, replacement: RemoteConfig) {
    state
        .update_app_settings(remote_settings_request(vec![
            RemoteConfig::local(),
            replacement,
        ]))
        .expect("remote settings replacement should succeed");
}

pub(super) fn remove_remote_settings(state: &AppState) {
    state
        .update_app_settings(remote_settings_request(vec![RemoteConfig::local()]))
        .expect("remote settings removal should succeed");
}

pub(super) fn install_post_decode_a_to_b_to_a_cycle(state: &AppState, original: &RemoteConfig) {
    let state_for_hook = state.clone();
    let original_for_hook = original.clone();
    let mut replacement = original.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    state.remote_registry.set_test_after_json_decode(move || {
        replace_remote_settings(&state_for_hook, replacement);
        replace_remote_settings(&state_for_hook, original_for_hook);
    });
}

fn assert_remote_create_callback_is_unlocked(state: &AppState) {
    assert!(
        state.inner.is_not_held_by_current_thread_for_test(),
        "remote create network I/O must run outside the state mutex"
    );
}

fn spawn_remote_create_response_server(
    expected_request_prefix: &'static str,
    response_body: String,
    before_response: impl FnOnce() + Send + 'static,
) -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("remote create race listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let mut before_response = Some(before_response);
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote create race listener");
            let request = read_test_http_request(&mut stream);
            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
                );
                continue;
            }
            assert!(
                request.request_line.starts_with(expected_request_prefix),
                "unexpected request: {}",
                request.request_line
            );
            before_response
                .take()
                .expect("remote create callback should run once")();
            write_test_http_response(
                &mut stream,
                StatusCode::CREATED,
                "application/json",
                &response_body,
            );
            break;
        }
    });
    (port, server)
}

#[derive(Clone, Copy)]
pub(super) enum RemoteRequestFailurePhase {
    BeforeResponseHeaders,
    DuringJsonBody,
}

pub(super) fn spawn_remote_request_failure_after_replacement_server(
    state: AppState,
    replacement: RemoteConfig,
    failure_phase: RemoteRequestFailurePhase,
) -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("remote request failure listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote request failure listener");
            let request = read_test_http_request(&mut stream);
            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
                );
                continue;
            }
            assert!(
                request.request_line.starts_with("GET /authority-failure "),
                "unexpected request: {}",
                request.request_line
            );
            if matches!(failure_phase, RemoteRequestFailurePhase::DuringJsonBody) {
                // `send()` can return after the headers arrive, while JSON
                // decoding remains blocked on this deliberately incomplete
                // body. Publish the replacement in that interval, then close
                // the body short so the decode path has an error to classify.
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 32\r\n\r\n{",
                    )
                    .expect("partial response should write");
                stream.flush().expect("partial response should flush");
            }
            replace_remote_settings(&state, replacement);
            break;
        }
    });
    (port, server)
}

fn remote_project_create_response_body(state: &AppState, remote_project_id: &str) -> String {
    serde_json::to_string(&CreateProjectResponse {
        project_id: remote_project_id.to_owned(),
        state: state.snapshot(),
    })
    .expect("remote project response should encode")
}

fn clear_remote_project_binding(state: &AppState, project_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let project = inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .expect("remote project should exist");
    project.remote_project_id = None;
    state
        .commit_locked(&mut inner)
        .expect("cleared remote project binding should persist");
}

#[test]
fn local_session_rejects_project_deleted_during_readiness_preflight() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-session-project-delete-preflight-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Deleted During Preflight");
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session_with_agent_setup_validator(
        CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Rejected Session".to_owned()),
            workdir: None,
            project_id: Some(project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
        |_agent, _workdir| {
            assert!(
                state.inner.is_not_held_by_current_thread_for_test(),
                "readiness validation must run outside the state mutex"
            );
            state
                .delete_project(&project_id)
                .expect("project deletion during preflight should succeed");
            Ok(())
        },
    ) {
        Ok(_) => panic!("deleted project should reject session creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown project `{project_id}`"));
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&project_id).is_none());
    assert_eq!(inner.sessions.len(), session_count_before);
    drop(inner);

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn local_session_rejects_project_root_changed_during_readiness_preflight() {
    let state = test_app_state();
    let unique = Uuid::new_v4();
    let original_root = std::env::temp_dir().join(format!("termal-session-original-root-{unique}"));
    let replacement_root =
        std::env::temp_dir().join(format!("termal-session-replacement-root-{unique}"));
    fs::create_dir_all(&original_root).expect("original project root should exist");
    fs::create_dir_all(&replacement_root).expect("replacement project root should exist");
    let project_id = create_test_project(&state, &original_root, "Changed During Preflight");
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session_with_agent_setup_validator(
        CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Rejected Changed Project Session".to_owned()),
            workdir: Some(original_root.to_string_lossy().into_owned()),
            project_id: Some(project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
        |_agent, _workdir| {
            assert!(
                state.inner.is_not_held_by_current_thread_for_test(),
                "readiness validation must run outside the state mutex"
            );
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let project = inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("project should exist during readiness preflight");
            project.root_path = replacement_root.to_string_lossy().into_owned();
            Ok(())
        },
    ) {
        Ok(_) => panic!("changed project should reject session creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "project changed while creating the session");
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .len(),
        session_count_before
    );

    let _ = fs::remove_dir_all(original_root);
    let _ = fs::remove_dir_all(replacement_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn projectless_session_survives_unrelated_project_deletion_during_preflight() {
    let state = test_app_state();
    let unique = Uuid::new_v4();
    let workdir = std::env::temp_dir().join(format!("termal-projectless-session-{unique}"));
    let unrelated_root = std::env::temp_dir().join(format!("termal-unrelated-project-{unique}"));
    fs::create_dir_all(&workdir).expect("projectless workdir should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project root should exist");
    let unrelated_project_id = create_test_project(&state, &unrelated_root, "Unrelated Project");

    let response = state
        .create_session_with_agent_setup_validator(
            CreateSessionRequest {
                agent: Some(Agent::Codex),
                name: Some("Projectless Session".to_owned()),
                workdir: Some(workdir.to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
            |_agent, _workdir| {
                assert!(
                    state.inner.is_not_held_by_current_thread_for_test(),
                    "readiness validation must run outside the state mutex"
                );
                state
                    .delete_project(&unrelated_project_id)
                    .expect("unrelated project deletion should succeed");
                Ok(())
            },
        )
        .expect("projectless session creation should not revalidate an unrelated project");

    assert_eq!(response.session.project_id, None);
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(unrelated_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn inferred_project_deletion_during_preflight_falls_back_to_projectless() {
    let state = test_app_state();
    let project_root = state
        .test_temp_root_path()
        .expect("project race tests should own a shared test temp root")
        .join("inferred-project-delete-preflight");
    let workdir = project_root.join("nested");
    fs::create_dir_all(&workdir).expect("nested workdir should exist");
    let project_id = create_test_project(&state, &project_root, "Inferred Project");
    let expected_workdir =
        resolve_session_workdir(&workdir.to_string_lossy()).expect("test workdir should resolve");

    let response = state
        .create_session_with_agent_setup_validator(
            CreateSessionRequest {
                agent: Some(Agent::Codex),
                name: Some("Projectless After Inferred Delete".to_owned()),
                workdir: Some(workdir.to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
            |_agent, _workdir| {
                assert!(
                    state.inner.is_not_held_by_current_thread_for_test(),
                    "readiness validation must run outside the state mutex"
                );
                state
                    .delete_project(&project_id)
                    .expect("inferred project deletion should succeed");
                Ok(())
            },
        )
        .expect("an inferred project deletion should not reject session creation");

    assert_eq!(response.session.project_id, None);
    assert_eq!(response.session.workdir, expected_workdir);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&project_id).is_none());
    drop(inner);
}

#[test]
fn inferred_project_root_drift_during_preflight_falls_back_to_projectless() {
    let state = test_app_state();
    let unique = Uuid::new_v4();
    let project_root = std::env::temp_dir().join(format!("termal-inferred-project-drift-{unique}"));
    let replacement_root =
        std::env::temp_dir().join(format!("termal-inferred-project-replacement-{unique}"));
    let workdir = project_root.join("nested");
    fs::create_dir_all(&workdir).expect("nested workdir should exist");
    fs::create_dir_all(&replacement_root).expect("replacement root should exist");
    let project_id = create_test_project(&state, &project_root, "Inferred Drift Project");
    let expected_workdir =
        resolve_session_workdir(&workdir.to_string_lossy()).expect("test workdir should resolve");

    let response = state
        .create_session_with_agent_setup_validator(
            CreateSessionRequest {
                agent: Some(Agent::Codex),
                name: Some("Projectless After Inferred Drift".to_owned()),
                workdir: Some(workdir.to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            },
            |_agent, _workdir| {
                assert!(
                    state.inner.is_not_held_by_current_thread_for_test(),
                    "readiness validation must run outside the state mutex"
                );
                let mut inner = state.inner.lock().expect("state mutex poisoned");
                let project = inner
                    .projects
                    .iter_mut()
                    .find(|project| project.id == project_id)
                    .expect("inferred project should exist");
                project.root_path = replacement_root.to_string_lossy().into_owned();
                Ok(())
            },
        )
        .expect("inferred project drift should not reject session creation");

    assert_eq!(response.session.project_id, None);
    assert_eq!(response.session.workdir, expected_workdir);
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .expect("inferred project should remain")
            .root_path,
        replacement_root.to_string_lossy()
    );

    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(replacement_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_rejects_project_deleted_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-session-delete-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-repo",
        "Remote Session Project",
        "remote-project-session",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session",
        "/remote/session-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion during remote create should succeed");
        });
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Remote Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id.clone()),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("deleted remote project should reject local proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        format!("unknown project `{local_project_id}`")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&local_project_id).is_none());
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_persist_failure_still_claims_the_current_event_bridge() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = remote_config("remote-session-persist-failure");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-persist-failure",
        "Remote Session Persist Failure",
        "remote-project-session-persist-failure",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-persist-failure",
        "/remote/session-persist-failure",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&remote.id)
        .cloned()
        .expect("test connection should exist");

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-remote-session-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Remote Session With Failed Local Persistence".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("local proxy persistence failure should propagate"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist remote session proxy")
    );
    assert!(
        state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&remote.id),
        "the remote-owned result must remain observable after local persistence fails"
    );
    assert!(
        connection.event_bridge_started.load(Ordering::SeqCst),
        "the already-claimed fixture prevents a detached SSH worker while proving ownership"
    );

    join_test_server(server);
    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn remote_session_create_publishes_prior_dirty_localization_recovery() {
    let persistence_root = TestTempRoot::create("termal-remote-session-dirty-recovery");
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = remote_config("remote-session-dirty-recovery");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-dirty-recovery",
        "Remote Session Dirty Recovery",
        "remote-project-session-dirty-recovery",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-dirty-recovery",
        "/remote/session-dirty-recovery",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);

    let failing_persistence_path = persistence_root.path().join("state.json");
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut peer_state_events = state.subscribe_events();
    let recovered_project_name = "Recovered Remote Project Name";
    let state_for_hook = state.clone();
    let project_id_for_hook = local_project_id.clone();
    let failing_path_for_hook = failing_persistence_path.clone();
    state.remote_registry.set_test_after_json_decode(move || {
        let mut inner = state_for_hook.inner.lock().expect("state mutex poisoned");
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id_for_hook)
            .expect("remote project should exist")
            .name = recovered_project_name.to_owned();
        state_for_hook
            .commit_remote_localization_locked(&mut inner)
            .expect_err("the post-decode persistence failure should arm retry debt");
        assert!(inner.remote_delta_persist_dirty);
        drop(inner);
        fs::remove_dir_all(&failing_path_for_hook)
            .expect("the create commit should be able to replace the failing directory");
    });

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Remote Session After Dirty Localization".to_owned()),
            workdir: None,
            project_id: Some(local_project_id.clone()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .expect("the remote create should persist and publish the recovered snapshot");
    assert!(
        !state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_delta_persist_dirty
    );

    let peer_snapshot: StateResponse = serde_json::from_str(
        &peer_state_events
            .try_recv()
            .expect("the successful create should publish the recovered snapshot"),
    )
    .expect("peer recovery snapshot should decode");
    assert_eq!(
        peer_snapshot
            .projects
            .iter()
            .find(|project| project.id == local_project_id)
            .expect("peer snapshot should include the recovered project")
            .name,
        recovered_project_name
    );
    assert!(
        peer_snapshot
            .sessions
            .iter()
            .any(|session| session.id == created.session_id),
        "the recovery snapshot should include the newly created remote session `{}`; observed {:?}",
        created.session_id,
        peer_snapshot
            .sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>()
    );

    let reloaded = load_state(failing_persistence_path.as_path())
        .expect("persisted state should load")
        .expect("persisted state should exist");
    assert_eq!(
        reloaded
            .projects
            .iter()
            .find(|project| project.id == local_project_id)
            .expect("reloaded state should include the recovered project")
            .name,
        recovered_project_name
    );
    assert!(
        reloaded
            .sessions
            .iter()
            .any(|record| record.session.id == created.session_id),
        "the newly created remote session should survive reload"
    );
    assert!(!reloaded.remote_delta_persist_dirty);

    join_test_server(server);
    let _ = fs::remove_file(&failing_persistence_path);
    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn remote_orchestrator_persist_failure_still_claims_the_current_event_bridge() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = remote_config("remote-orchestrator-persist-failure");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-persist-failure",
        "Remote Orchestrator Persist Failure",
        "remote-project-orchestrator-persist-failure",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator-persist-failure",
        "/remote/orchestrator-persist-failure",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let recovery_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator-persist-failure",
        "/remote/orchestrator-persist-failure",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_orchestrator_id = remote_state.orchestrators[0].id.clone();
    let response_body = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/orchestrators ", response_body, || {});
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&remote.id)
        .cloned()
        .expect("test connection should exist");

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-remote-orchestrator-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let error = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id,
        project_id: Some(local_project_id.clone()),
        template: None,
    }) {
        Ok(_) => panic!("local orchestrator persistence failure should propagate"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist remote orchestrator proxy")
    );
    assert!(
        state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&remote.id),
        "the remote-owned result must remain observable after local persistence fails"
    );
    assert!(
        connection.event_bridge_started.load(Ordering::SeqCst),
        "the already-claimed fixture prevents a detached SSH worker while proving ownership"
    );

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(
            inner.remote_snapshot_applied_revisions.get(&remote.id),
            Some(&2),
            "the failed create already consumed the response snapshot in memory"
        );
        assert!(
            inner.remote_delta_persist_dirty,
            "the failed localization must retain a full-state persistence obligation"
        );
    }

    join_test_server(server);
    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    state.persistence_path = Arc::new(original_persistence_path.as_path().to_path_buf());
    state
        .apply_remote_state_snapshot(&remote.id, recovery_state.into_state_response())
        .expect("an equal-revision snapshot should retry the failed localization persistence");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.remote_delta_persist_dirty);
    drop(inner);
    let reloaded = load_state(original_persistence_path.as_path())
        .expect("recovered orchestrator state should load from SQLite")
        .expect("recovered orchestrator state should be persisted");
    assert!(
        reloaded.orchestrator_instances.iter().any(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref()
                    == Some(remote_orchestrator_id.as_str())
                && instance.project_id == local_project_id
        }),
        "the remote orchestrator localized before the failure must survive restart"
    );
    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn remote_orchestrator_rejects_project_deleted_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-orchestrator-delete-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-repo",
        "Remote Orchestrator Project",
        "remote-project-orchestrator",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator",
        "/remote/orchestrator-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let response_body = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/orchestrators ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion during remote create should succeed");
        });
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let (session_count_before, orchestrator_count_before) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.orchestrator_instances.len())
    };

    let error = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id,
        project_id: Some(local_project_id.clone()),
        template: None,
    }) {
        Ok(_) => panic!("deleted remote project should reject orchestrator localization"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(
        error.message,
        format!("unknown project `{local_project_id}`")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&local_project_id).is_none());
    assert_eq!(inner.sessions.len(), session_count_before);
    assert_eq!(
        inner.orchestrator_instances.len(),
        orchestrator_count_before
    );
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_rejects_project_rebound_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-session-rebound-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/rebound-repo",
        "Remote Rebound Project",
        "remote-project-original",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-original",
        "/remote/rebound-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
            let project = inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id_for_server)
                .expect("remote project should exist");
            project.remote_project_id = Some("remote-project-rebound".to_owned());
        });
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Rebound Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id.clone()),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("rebound remote project should reject local proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_orchestrator_rejects_project_rebound_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-orchestrator-rebound-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-rebound-repo",
        "Remote Orchestrator Rebound Project",
        "remote-project-original",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-original",
        "/remote/orchestrator-rebound-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let response_body = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/orchestrators ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
            let project = inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id_for_server)
                .expect("remote project should exist");
            project.remote_project_id = Some("remote-project-rebound".to_owned());
        });
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let (session_count_before, orchestrator_count_before) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.orchestrator_instances.len())
    };

    let error = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id,
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("rebound remote project should reject orchestrator localization"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert_eq!(
        inner.orchestrator_instances.len(),
        orchestrator_count_before
    );
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_rejects_remote_removed_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-session-removal-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-removal-repo",
        "Remote Session Removal Project",
        "remote-project-session-removal",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-removal",
        "/remote/session-removal-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion should allow remote removal");
            remove_remote_settings(&state_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Removed Remote Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("removed remote should reject local session proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id),
        "removed remote connection must not be recreated by stale bridge startup"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_orchestrator_rejects_remote_removed_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-orchestrator-removal-race");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-removal-repo",
        "Remote Orchestrator Removal Project",
        "remote-project-orchestrator-removal",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator-removal",
        "/remote/orchestrator-removal-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let response_body = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/orchestrators ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion should allow remote removal");
            remove_remote_settings(&state_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let (session_count_before, orchestrator_count_before) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.orchestrator_instances.len())
    };

    let error = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id,
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("removed remote should reject orchestrator localization"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert_eq!(
        inner.orchestrator_instances.len(),
        orchestrator_count_before
    );
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id),
        "removed remote connection must not be recreated by stale bridge startup"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_rejects_endpoint_replaced_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-session-endpoint-race");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-endpoint-repo",
        "Remote Session Endpoint Project",
        "remote-project-session-endpoint",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-endpoint",
        "/remote/session-endpoint-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let state_for_server = state.clone();
    let replacement_for_server = replacement.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            replace_remote_settings(&state_for_server, replacement_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Endpoint Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("replaced endpoint should reject local session proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    let configs = state
        .remote_registry
        .configs
        .lock()
        .expect("remote registry config mutex poisoned");
    assert_eq!(configs.get(&remote.id), Some(&replacement));
    drop(configs);
    assert!(
        state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .get(&remote.id)
            .is_none(),
        "request-only fixtures must not manufacture a replacement bridge connection"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_orchestrator_rejects_endpoint_replaced_during_create_request() {
    let state = test_app_state();
    let remote = remote_config("remote-orchestrator-endpoint-race");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-endpoint-repo",
        "Remote Orchestrator Endpoint Project",
        "remote-project-orchestrator-endpoint",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator-endpoint",
        "/remote/orchestrator-endpoint-repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let response_body = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");
    let state_for_server = state.clone();
    let replacement_for_server = replacement.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/orchestrators ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            replace_remote_settings(&state_for_server, replacement_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let (session_count_before, orchestrator_count_before) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        (inner.sessions.len(), inner.orchestrator_instances.len())
    };

    let error = match state.create_orchestrator_instance(CreateOrchestratorInstanceRequest {
        template_id: template.id,
        project_id: Some(local_project_id),
        template: None,
    }) {
        Ok(_) => panic!("replaced endpoint should reject orchestrator localization"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert_eq!(
        inner.orchestrator_instances.len(),
        orchestrator_count_before
    );
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    let configs = state
        .remote_registry
        .configs
        .lock()
        .expect("remote registry config mutex poisoned");
    assert_eq!(configs.get(&remote.id), Some(&replacement));
    drop(configs);
    assert!(
        state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .get(&remote.id)
            .is_none(),
        "request-only fixtures must not manufacture a replacement bridge connection"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_create_rejects_remote_removed_during_request() {
    let state = test_app_state();
    let remote = remote_config("remote-project-create-removal-race");
    replace_remote_settings(&state, remote.clone());
    let response_body = remote_project_create_response_body(&state, "remote-created-project");
    let state_for_server = state.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            remove_remote_settings(&state_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Rejected Remote Project".to_owned()),
        root_path: "/remote/project-create-removal".to_owned(),
        remote_id: remote.id.clone(),
    }) {
        Ok(_) => panic!("removed remote should reject local project proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .projects
            .iter()
            .all(|project| project.root_path != "/remote/project-create-removal")
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_create_rejects_endpoint_replaced_during_request() {
    let state = test_app_state();
    let remote = remote_config("remote-project-create-endpoint-race");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    replace_remote_settings(&state, remote.clone());
    let response_body = remote_project_create_response_body(&state, "remote-created-project");
    let state_for_server = state.clone();
    let replacement_for_server = replacement.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            replace_remote_settings(&state_for_server, replacement_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Rejected Endpoint Project".to_owned()),
        root_path: "/remote/project-create-endpoint".to_owned(),
        remote_id: remote.id.clone(),
    }) {
        Ok(_) => panic!("replaced endpoint should reject local project proxy creation"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .projects
            .iter()
            .all(|project| project.root_path != "/remote/project-create-endpoint")
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_create_rejects_concurrent_binding_winner() {
    let state = test_app_state();
    let remote = remote_config("remote-project-create-first-writer");
    let root_path = "/remote/project-create-first-writer";
    replace_remote_settings(&state, remote.clone());
    let response_body = remote_project_create_response_body(&state, "losing-binding");
    let state_for_server = state.clone();
    let remote_for_server = remote.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
            let project = inner.create_project(
                Some("Concurrent Winner".to_owned()),
                root_path.to_owned(),
                remote_for_server.id.clone(),
            );
            let index = inner
                .projects
                .iter()
                .position(|candidate| candidate.id == project.id)
                .expect("concurrent project should exist");
            inner.projects[index].remote_project_id = Some("winning-binding".to_owned());
            state_for_server
                .commit_locked(&mut inner)
                .expect("winning project binding should persist");
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Losing Project Create".to_owned()),
        root_path: root_path.to_owned(),
        remote_id: remote.id.clone(),
    }) {
        Ok(_) => panic!("a later remote binding must not overwrite the winner"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let project = inner
        .projects
        .iter()
        .find(|project| project.remote_id == remote.id && project.root_path == root_path)
        .expect("winning project should remain");
    assert_eq!(
        project.remote_project_id.as_deref(),
        Some("winning-binding")
    );
    drop(inner);
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_existing_fast_path_rejects_concurrent_deletion() {
    let state = test_app_state();
    let remote = remote_config("remote-project-existing-delete");
    let root_path = "/remote/project-existing-delete";
    let project_id = create_test_remote_project(
        &state,
        &remote,
        root_path,
        "Remote Project Existing Delete",
        "remote-project-existing-delete",
    );
    let state_for_hook = state.clone();
    let project_id_for_hook = project_id.clone();
    state
        .remote_registry
        .set_test_before_existing_remote_project_revalidation(move || {
            state_for_hook
                .delete_project(&project_id_for_hook)
                .expect("project deletion at the fast-path boundary should succeed");
        });

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Remote Project Existing Delete".to_owned()),
        root_path: root_path.to_owned(),
        remote_id: remote.id.clone(),
    }) {
        Ok(_) => panic!("a deleted existing project must not be returned as created"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE);
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .is_none()
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn existing_remote_project_binding_rejects_concurrent_deletion() {
    let state = test_app_state();
    let remote = remote_config("remote-project-binding-existing-delete");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/project-binding-existing-delete",
        "Remote Project Binding Existing Delete",
        "remote-project-binding-existing-delete",
    );
    let state_for_hook = state.clone();
    let project_id_for_hook = project_id.clone();
    state
        .remote_registry
        .set_test_before_existing_remote_project_revalidation(move || {
            state_for_hook
                .delete_project(&project_id_for_hook)
                .expect("project deletion at the binding fast-path boundary should succeed");
        });

    let error = match state.ensure_remote_project_binding(&project_id) {
        Ok(_) => panic!("a deleted project must not return its captured remote binding"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, format!("unknown project `{project_id}`"));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .is_none()
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_idempotent_retry_persists_failed_localization_and_publishes_snapshot() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = remote_config("remote-project-create-persist-retry");
    let root_path = "/remote/project-create-persist-retry";
    let remote_project_id = "remote-project-persist-retry";
    replace_remote_settings(&state, remote.clone());
    let response_body = remote_project_create_response_body(&state, remote_project_id);
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, || {});
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-remote-project-persist-retry-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut peer_state_events = state.subscribe_events();

    let error = match state.create_project(CreateProjectRequest {
        name: Some("Remote Project Persist Retry".to_owned()),
        root_path: root_path.to_owned(),
        remote_id: remote.id.clone(),
    }) {
        Ok(_) => panic!("the first remote project localization should fail to persist"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist project"));
    let local_project_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.remote_delta_persist_dirty);
        let project = inner
            .projects
            .iter()
            .find(|project| project.remote_id == remote.id && project.root_path == root_path)
            .expect("the failed localization should remain authoritative in memory");
        assert_eq!(
            project.remote_project_id.as_deref(),
            Some(remote_project_id)
        );
        project.id.clone()
    };
    assert!(
        peer_state_events.try_recv().is_err(),
        "a failed localization must not publish a state snapshot"
    );
    join_test_server(server);

    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    state.persistence_path = original_persistence_path.clone();
    let retry = state
        .create_project(CreateProjectRequest {
            name: Some("Remote Project Persist Retry".to_owned()),
            root_path: root_path.to_owned(),
            remote_id: remote.id.clone(),
        })
        .expect("an idempotent retry should persist the in-memory project localization");
    assert_eq!(retry.project_id, local_project_id);
    assert!(
        !state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_delta_persist_dirty
    );

    let peer_snapshot: StateResponse = serde_json::from_str(
        &peer_state_events
            .try_recv()
            .expect("the persistence retry should publish the missed snapshot"),
    )
    .expect("peer project snapshot should decode");
    let peer_project = peer_snapshot
        .projects
        .iter()
        .find(|project| project.id == local_project_id)
        .expect("the peer snapshot should include the retried project");
    assert_eq!(
        peer_project.remote_project_id.as_deref(),
        Some(remote_project_id)
    );

    let reloaded = load_state(original_persistence_path.as_path())
        .expect("persisted state should load")
        .expect("persisted state should exist");
    let reloaded_project = reloaded
        .projects
        .iter()
        .find(|project| project.id == local_project_id)
        .expect("the retried project should survive reload");
    assert_eq!(
        reloaded_project.remote_project_id.as_deref(),
        Some(remote_project_id)
    );
    assert!(!reloaded.remote_delta_persist_dirty);

    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn lazy_remote_project_binding_rejects_endpoint_replaced_during_request() {
    let state = test_app_state();
    let remote = remote_config("remote-project-binding-endpoint-race");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/lazy-binding-endpoint",
        "Lazy Binding Endpoint",
        "initial-binding",
    );
    clear_remote_project_binding(&state, &project_id);
    let response_body = remote_project_create_response_body(&state, "late-binding");
    let state_for_server = state.clone();
    let replacement_for_server = replacement.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            replace_remote_settings(&state_for_server, replacement_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.ensure_remote_project_binding(&project_id) {
        Ok(_) => panic!("replaced endpoint should reject lazy binding persistence"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .expect("local project should remain")
            .remote_project_id
            .is_none()
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn lazy_remote_project_binding_rejects_project_rebound_during_request() {
    let state = test_app_state();
    let remote = remote_config("remote-project-binding-rebound-race");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/lazy-binding-rebound",
        "Lazy Binding Rebound",
        "initial-binding",
    );
    clear_remote_project_binding(&state, &project_id);
    let response_body = remote_project_create_response_body(&state, "late-binding");
    let state_for_server = state.clone();
    let project_id_for_server = project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
            let project = inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id_for_server)
                .expect("local project should exist");
            project.remote_id = LOCAL_REMOTE_ID.to_owned();
            state_for_server
                .commit_locked(&mut inner)
                .expect("project rebound should persist");
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.ensure_remote_project_binding(&project_id) {
        Ok(_) => panic!("rebound project should reject lazy binding persistence"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE);
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .expect("local project should remain")
            .remote_project_id
            .is_none()
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn lazy_remote_project_binding_rejects_remote_removed_during_request() {
    let state = test_app_state();
    let remote = remote_config("remote-project-binding-removal-race");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/lazy-binding-removal",
        "Lazy Binding Removal",
        "initial-binding",
    );
    clear_remote_project_binding(&state, &project_id);
    let response_body = remote_project_create_response_body(&state, "late-binding");
    let state_for_server = state.clone();
    let project_id_for_server = project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion should succeed");
            remove_remote_settings(&state_for_server);
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.ensure_remote_project_binding(&project_id) {
        Ok(_) => panic!("removed authority should reject lazy binding persistence"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_project(&project_id)
            .is_none()
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn lazy_remote_project_binding_preserves_concurrent_winner() {
    let state = test_app_state();
    let remote = remote_config("remote-project-binding-first-writer");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/lazy-binding-first-writer",
        "Lazy Binding First Writer",
        "initial-binding",
    );
    clear_remote_project_binding(&state, &project_id);
    let response_body = remote_project_create_response_body(&state, "losing-binding");
    let state_for_server = state.clone();
    let project_id_for_server = project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            let mut inner = state_for_server.inner.lock().expect("state mutex poisoned");
            let project = inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id_for_server)
                .expect("local project should exist");
            project.remote_project_id = Some("winning-binding".to_owned());
            state_for_server
                .commit_locked(&mut inner)
                .expect("winning binding should persist");
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let binding = state
        .ensure_remote_project_binding(&project_id)
        .expect("concurrent winner should converge")
        .expect("remote project should return a binding");

    assert_eq!(binding.remote_project_id, "winning-binding");
    assert_eq!(binding.remote, remote);
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_lazy_binding_preserves_bad_request_when_project_is_deleted() {
    let state = test_app_state();
    let remote = remote_config("remote-session-lazy-binding-delete");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-lazy-binding-delete",
        "Remote Session Lazy Binding Delete",
        "initial-binding",
    );
    clear_remote_project_binding(&state, &project_id);
    let response_body = remote_project_create_response_body(&state, "late-binding");
    let state_for_server = state.clone();
    let project_id_for_server = project_id.clone();
    let (port, server) =
        spawn_remote_create_response_server("POST /api/projects ", response_body, move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion during lazy binding should succeed");
        });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Lazy Binding Session".to_owned()),
        workdir: None,
        project_id: Some(project_id.clone()),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("deleted project must reject remote session lazy binding"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown project `{project_id}`"));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_remote(&remote.id)
            .is_some(),
        "the race removes only the project, not remote authority"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn non_create_state_response_is_rejected_after_endpoint_replacement() {
    let state = test_app_state();
    let remote = remote_config("remote-action-endpoint-swap");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/action-endpoint-swap",
        "Remote Action Endpoint Swap",
        "remote-project-action-endpoint-swap",
    );
    let initial_state = sample_remote_orchestrator_state(
        "remote-project-action-endpoint-swap",
        "/remote/action-endpoint-swap",
        2,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote state should localize");
    let local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("localized remote session should exist");
        inner.sessions[index].session.id.clone()
    };
    let initial_status = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&local_session_id)
            .expect("localized session should exist");
        inner.sessions[index].session.status
    };

    let mut stale_response = sample_remote_orchestrator_state(
        "remote-project-action-endpoint-swap",
        "/remote/action-endpoint-swap",
        3,
        OrchestratorInstanceStatus::Running,
    );
    stale_response.sessions[0].status = SessionStatus::Error;
    let response_body =
        serde_json::to_string(&stale_response).expect("remote state response should encode");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let state_for_server = state.clone();
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/stop ",
        response_body,
        move || replace_remote_settings(&state_for_server, replacement),
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&remote.id)
        .expect("test connection should exist")
        .event_bridge_started
        .store(false, Ordering::SeqCst);

    let error = match state.proxy_remote_stop_session(&local_session_id) {
        Ok(_) => panic!("an old-endpoint response must fail closed"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
    let retained_status = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&local_session_id)
            .expect("localized session should remain");
        inner.sessions[index].session.status
    };
    assert_eq!(retained_status, initial_status);
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .get(&remote.id)
            .copied(),
        None
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn non_create_state_response_is_rejected_after_post_decode_a_to_b_to_a_cycle() {
    let state = test_app_state();
    let remote = remote_config("remote-action-post-decode-cycle");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/action-post-decode-cycle",
        "Remote Action Post Decode Cycle",
        "remote-project-action-post-decode-cycle",
    );
    let initial_state = sample_remote_orchestrator_state(
        "remote-project-action-post-decode-cycle",
        "/remote/action-post-decode-cycle",
        2,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote state should localize");
    let (local_session_id, initial_status) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("localized remote session should exist");
        (
            inner.sessions[index].session.id.clone(),
            inner.sessions[index].session.status,
        )
    };

    let mut stale_response = sample_remote_orchestrator_state(
        "remote-project-action-post-decode-cycle",
        "/remote/action-post-decode-cycle",
        3,
        OrchestratorInstanceStatus::Running,
    );
    stale_response.sessions[0].status = SessionStatus::Error;
    let response_body =
        serde_json::to_string(&stale_response).expect("remote state response should encode");
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/stop ",
        response_body,
        || {},
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    install_post_decode_a_to_b_to_a_cycle(&state, &remote);

    let error = match state.proxy_remote_stop_session(&local_session_id) {
        Ok(_) => panic!("a pre-cycle response must not localize after A -> B -> A"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&local_session_id)
        .expect("localized session should remain");
    assert_eq!(inner.sessions[index].session.status, initial_status);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    assert_eq!(
        state
            .remote_registry
            .configs
            .lock()
            .expect("remote registry config mutex poisoned")
            .get(&remote.id),
        Some(&remote)
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn non_create_state_response_preserves_unknown_remote_after_post_decode_removal() {
    let state = test_app_state();
    let remote = remote_config("remote-action-post-decode-removal");
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/action-post-decode-removal",
        "Remote Action Post Decode Removal",
        "remote-project-action-post-decode-removal",
    );
    let initial_state = sample_remote_orchestrator_state(
        "remote-project-action-post-decode-removal",
        "/remote/action-post-decode-removal",
        2,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote state should localize");
    let (local_session_id, initial_status) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("localized remote session should exist");
        (
            inner.sessions[index].session.id.clone(),
            inner.sessions[index].session.status,
        )
    };

    let mut stale_response = sample_remote_orchestrator_state(
        "remote-project-action-post-decode-removal",
        "/remote/action-post-decode-removal",
        3,
        OrchestratorInstanceStatus::Running,
    );
    stale_response.sessions[0].status = SessionStatus::Error;
    let response_body =
        serde_json::to_string(&stale_response).expect("remote state response should encode");
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/stop ",
        response_body,
        || {},
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let state_for_hook = state.clone();
    state.remote_registry.set_test_after_json_decode(move || {
        state_for_hook
            .delete_project(&project_id)
            .expect("project deletion should detach the remote");
        remove_remote_settings(&state_for_hook);
    });

    let error = match state.proxy_remote_stop_session(&local_session_id) {
        Ok(_) => panic!("a removed remote response must not localize"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&local_session_id)
        .expect("detached remote session should remain as history");
    assert_eq!(inner.sessions[index].session.status, initial_status);
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_create_is_rejected_after_post_decode_a_to_b_to_a_cycle() {
    let state = test_app_state();
    let remote = remote_config("remote-session-post-decode-cycle");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-post-decode-cycle",
        "Remote Session Post Decode Cycle",
        "remote-project-session-post-decode-cycle",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-post-decode-cycle",
        "/remote/session-post-decode-cycle",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "old-remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    install_post_decode_a_to_b_to_a_cycle(&state, &remote);
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Post Decode Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("a pre-cycle create response must not localize after A -> B -> A"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    drop(inner);
    assert!(
        !state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&remote.id),
        "a retired response lease must not leave a desired bridge subscription"
    );
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id),
        "a retired response lease must not create a fresh A bridge"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_session_create_does_not_claim_replacement_bridge_after_json_decode() {
    let state = test_app_state();
    let remote = remote_config("remote-session-post-decode-replacement");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-post-decode-replacement",
        "Remote Session Post Decode Replacement",
        "remote-project-session-post-decode-replacement",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-post-decode-replacement",
        "/remote/session-post-decode-replacement",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: remote_session.id.clone(),
        session: remote_session,
        revision: 2,
        server_instance_id: "old-remote-server".to_owned(),
    })
    .expect("remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let original_connection = state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .get(&remote.id)
        .cloned()
        .expect("original request connection should exist");
    let state_for_hook = state.clone();
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let replacement_for_hook = replacement.clone();
    state.remote_registry.set_test_after_json_decode(move || {
        replace_remote_settings(&state_for_hook, replacement_for_hook);
    });
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Post Decode Replacement Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("an A response must not claim or localize through endpoint B"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .len(),
        session_count_before
    );
    assert!(original_connection.retired.load(Ordering::SeqCst));
    assert_eq!(
        state
            .remote_registry
            .configs
            .lock()
            .expect("remote registry config mutex poisoned")
            .get(&remote.id),
        Some(&replacement)
    );
    assert!(
        !state
            .remote_registry
            .desired_event_bridges
            .lock()
            .expect("remote event bridge subscription mutex poisoned")
            .contains(&remote.id),
        "the stale A response must not subscribe endpoint B"
    );
    assert!(
        !state
            .remote_registry
            .connections
            .lock()
            .expect("remote registry mutex poisoned")
            .contains_key(&remote.id),
        "the stale A response must not create a B connection or bridge"
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn malformed_remote_session_response_prefers_post_decode_cycle_conflict() {
    let state = test_app_state();
    let remote = remote_config("remote-session-malformed-post-decode-cycle");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-malformed-post-decode-cycle",
        "Remote Session Malformed Post Decode Cycle",
        "remote-project-session-malformed-post-decode-cycle",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-malformed-post-decode-cycle",
        "/remote/session-malformed-post-decode-cycle",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: "different-remote-session-id".to_owned(),
        session: remote_session,
        revision: 2,
        server_instance_id: "old-remote-server".to_owned(),
    })
    .expect("malformed remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    install_post_decode_a_to_b_to_a_cycle(&state, &remote);
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Malformed Post Decode Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("retired malformed response must not localize"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .len(),
        session_count_before
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn malformed_remote_session_response_prefers_post_decode_removal_error() {
    let state = test_app_state();
    let remote = remote_config("remote-session-malformed-post-decode-removal");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/session-malformed-post-decode-removal",
        "Remote Session Malformed Post Decode Removal",
        "remote-project-session-malformed-post-decode-removal",
    );
    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-session-malformed-post-decode-removal",
        "/remote/session-malformed-post-decode-removal",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let remote_session = remote_state.sessions.remove(0);
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: "different-remote-session-id".to_owned(),
        session: remote_session,
        revision: 2,
        server_instance_id: "removed-remote-server".to_owned(),
    })
    .expect("malformed remote session response should encode");
    let (port, server) =
        spawn_remote_create_response_server("POST /api/sessions ", response_body, || {});
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let state_for_hook = state.clone();
    let project_for_hook = local_project_id.clone();
    state.remote_registry.set_test_after_json_decode(move || {
        state_for_hook
            .delete_project(&project_for_hook)
            .expect("project deletion should detach the remote");
        remove_remote_settings(&state_for_hook);
    });
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.create_session(CreateSessionRequest {
        agent: Some(Agent::Codex),
        name: Some("Rejected Malformed Removed Session".to_owned()),
        workdir: None,
        project_id: Some(local_project_id),
        model: None,
        approval_policy: None,
        reasoning_effort: None,
        sandbox_mode: None,
        cursor_mode: None,
        claude_approval_mode: None,
        claude_effort: None,
        gemini_approval_mode: None,
    }) {
        Ok(_) => panic!("removed malformed response must not localize"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .len(),
        session_count_before
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_codex_fork_inherits_source_attachment_after_project_deletion() {
    let state = test_app_state();
    let remote = remote_config("remote-codex-fork-project-delete");
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/codex-fork-delete",
        "Remote Codex Fork Project",
        "remote-project-codex-fork-delete",
    );
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-codex-fork-delete",
        "/remote/codex-fork-delete",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let mut forked_remote_session = remote_state.sessions[0].clone();
    state
        .apply_remote_state_snapshot(&remote.id, remote_state.into_state_response())
        .expect("source remote state should localize");
    let source_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("source remote proxy should exist");
        inner.sessions[index].session.id.clone()
    };

    forked_remote_session.id = "remote-session-forked".to_owned();
    forked_remote_session.name = "Forked After Project Delete".to_owned();
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: forked_remote_session.id.clone(),
        session: forked_remote_session,
        revision: 3,
        server_instance_id: "remote-server".to_owned(),
    })
    .expect("remote fork response should encode");
    let state_for_server = state.clone();
    let project_id_for_server = local_project_id.clone();
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/codex/thread/fork ",
        response_body,
        move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            state_for_server
                .delete_project(&project_id_for_server)
                .expect("project deletion during remote fork should succeed");
        },
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let forked = state
        .proxy_remote_fork_codex_thread(&source_local_session_id)
        .expect("fork should inherit the source session's post-delete attachment");

    assert_eq!(forked.session.project_id, None);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.find_project(&local_project_id).is_none());
    let source_index = inner
        .find_session_index(&source_local_session_id)
        .expect("source proxy should remain after project deletion");
    let forked_index = inner
        .find_session_index(&forked.session_id)
        .expect("forked proxy should be persisted");
    assert_eq!(inner.sessions[source_index].session.project_id, None);
    assert_eq!(
        inner.sessions[forked_index].session.project_id,
        inner.sessions[source_index].session.project_id
    );
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_codex_fork_rejects_endpoint_replacement_before_localization() {
    let state = test_app_state();
    let remote = remote_config("remote-codex-fork-endpoint-replaced");
    let mut replacement = remote.clone();
    replacement.host = Some("replacement.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    create_test_remote_project(
        &state,
        &remote,
        "/remote/codex-fork-endpoint-replaced",
        "Remote Codex Fork Endpoint Project",
        "remote-project-codex-fork-endpoint-replaced",
    );
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-codex-fork-endpoint-replaced",
        "/remote/codex-fork-endpoint-replaced",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let mut forked_remote_session = remote_state.sessions[0].clone();
    state
        .apply_remote_state_snapshot(&remote.id, remote_state.into_state_response())
        .expect("source remote state should localize");
    let source_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("source remote proxy should exist");
        inner.sessions[index].session.id.clone()
    };

    forked_remote_session.id = "remote-session-forked-endpoint-replaced".to_owned();
    forked_remote_session.name = "Forked Across Endpoint Replacement".to_owned();
    let forked_remote_session_id = forked_remote_session.id.clone();
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: forked_remote_session_id.clone(),
        session: forked_remote_session,
        revision: 3,
        server_instance_id: "old-remote-server".to_owned(),
    })
    .expect("remote fork response should encode");
    let state_for_server = state.clone();
    let replacement_for_server = replacement.clone();
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/codex/thread/fork ",
        response_body,
        move || {
            assert_remote_create_callback_is_unlocked(&state_for_server);
            replace_remote_settings(&state_for_server, replacement_for_server);
        },
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.proxy_remote_fork_codex_thread(&source_local_session_id) {
        Ok(_) => panic!("an old endpoint fork response must not localize"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(
        inner
            .find_remote_session_index(&replacement.id, &forked_remote_session_id)
            .is_none(),
        "the stale fork response must not create a local proxy"
    );
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_codex_fork_is_rejected_after_post_decode_a_to_b_to_a_cycle() {
    let state = test_app_state();
    let remote = remote_config("remote-codex-fork-post-decode-cycle");
    create_test_remote_project(
        &state,
        &remote,
        "/remote/codex-fork-post-decode-cycle",
        "Remote Codex Fork Post Decode Cycle",
        "remote-project-codex-fork-post-decode-cycle",
    );
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-codex-fork-post-decode-cycle",
        "/remote/codex-fork-post-decode-cycle",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let mut forked_remote_session = remote_state.sessions[0].clone();
    state
        .apply_remote_state_snapshot(&remote.id, remote_state.into_state_response())
        .expect("source remote state should localize");
    let source_local_session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("source remote proxy should exist");
        inner.sessions[index].session.id.clone()
    };

    forked_remote_session.id = "remote-session-forked-post-decode-cycle".to_owned();
    forked_remote_session.name = "Forked Across Post Decode Cycle".to_owned();
    let forked_remote_session_id = forked_remote_session.id.clone();
    let response_body = serde_json::to_string(&CreateSessionResponse {
        session_id: forked_remote_session_id.clone(),
        session: forked_remote_session,
        revision: 3,
        server_instance_id: "old-remote-server".to_owned(),
    })
    .expect("remote fork response should encode");
    let (port, server) = spawn_remote_create_response_server(
        "POST /api/sessions/remote-session-1/codex/thread/fork ",
        response_body,
        || {},
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    install_post_decode_a_to_b_to_a_cycle(&state, &remote);
    let session_count_before = state
        .inner
        .lock()
        .expect("state mutex poisoned")
        .sessions
        .len();

    let error = match state.proxy_remote_fork_codex_thread(&source_local_session_id) {
        Ok(_) => panic!("a pre-cycle fork response must not localize after A -> B -> A"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_DURING_CREATE);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), session_count_before);
    assert!(
        inner
            .find_remote_session_index(&remote.id, &forked_remote_session_id)
            .is_none(),
        "the pre-cycle fork response must not create a local proxy"
    );
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
