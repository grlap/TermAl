//! Remote orchestrator proxy and snapshot localization tests.
//!
//! Split out of remote.rs so orchestrator-specific remote behavior can be
//! reviewed without the session hydration and terminal proxy coverage.

use super::*;

// Pins the end-to-end create-orchestrator flow for remote projects: the
// request is rewritten with the remote's own project id and forwarded
// as POST /api/orchestrators, and the response is localized (new local
// orchestrator id, template_snapshot project_id rewritten to local).
// Guards against the UI seeing raw remote ids or sending local ids that
// the remote cannot resolve.
#[test]
fn create_orchestrator_instance_proxies_remote_projects_and_localizes_response() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_orchestrator = remote_state.orchestrators[0].clone();
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_orchestrator,
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");

    let captured = Arc::new(Mutex::new(None::<(String, String)>));
    let captured_for_server = captured.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            requests_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());
            let body = String::from_utf8_lossy(&buffer[body_start..body_start + content_length])
                .to_string();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                *captured_for_server.lock().expect("capture mutex poisoned") =
                    Some((request_line.clone(), body));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote.clone(),
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("remote orchestrator should be created");

    assert_ne!(response.orchestrator.id, "remote-orchestrator-created");
    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert_eq!(response.orchestrator.template_id, template.id);
    assert_eq!(
        response
            .orchestrator
            .template_snapshot
            .project_id
            .as_deref(),
        Some(response.orchestrator.project_id.as_str())
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let (request_line, body) = captured
        .lock()
        .expect("capture mutex poisoned")
        .clone()
        .expect("captured request should exist");
    assert!(request_line.starts_with("POST /api/orchestrators "));
    let parsed_body: Value = serde_json::from_str(&body).expect("request body should decode");
    assert_eq!(
        parsed_body["templateId"],
        Value::String(template.id.clone())
    );
    assert_eq!(
        parsed_body["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        parsed_body["template"]["name"],
        Value::String(template.name)
    );

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that create_remote_orchestrator_proxy localizes the response
// orchestrator, registers the remote_orchestrator_id, and writes the
// returned revision into the applied-revision watermark so that later
// delta/snapshot replays at the same revision are skipped.
// Guards against creating a remote orchestrator but leaving the local
// watermark behind, which would cause the same state to re-apply.
#[test]
fn create_remote_orchestrator_proxy_localizes_launch_and_notes_revision() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote.clone(),
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let response = state
        .create_remote_orchestrator_proxy(&template, &project)
        .expect("remote orchestrator should be localized");

    assert_eq!(
        response.orchestrator.remote_id.as_deref(),
        Some(remote.id.as_str())
    );
    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 2));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 3));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that create_remote_orchestrator_proxy returns BAD_GATEWAY and
// leaves next_session_number, orchestrator instances, session records,
// the applied-revision watermark, and the persisted state file all
// unchanged when the localization step fails.
// Guards against partial writes on failed proxy creation that would
// surface as orphaned sessions or a poisoned watermark.
#[test]
fn create_remote_orchestrator_proxy_rolls_back_on_localization_failure() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;
    let project = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(&local_project_id)
            .cloned()
            .expect("remote project should exist")
    };
    let persisted_before = fs::read(state.persistence_path.as_path())
        .expect("initial state should already be persisted");
    let initial_next_session_number = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.next_session_number
    };

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-broken".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    remote_state
        .sessions
        .retain(|session| session.id != "remote-session-1");
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).expect("request should read");
            assert!(bytes_read > 0, "request should contain data");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote.clone(),
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let err = match state.create_remote_orchestrator_proxy(&template, &project) {
        Ok(_) => panic!("invalid remote orchestrator should fail localization"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message
            .contains("remote orchestrator could not be localized")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.next_session_number, initial_next_session_number);
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-broken")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .is_none()
    );
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 2));
    drop(inner);

    let persisted_after = fs::read(state.persistence_path.as_path())
        .expect("rolled back state should stay persisted");
    assert_eq!(persisted_after, persisted_before);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a stale create-orchestrator response (revision below the
// already-applied remote revision) still materializes the launched
// orchestrator locally rather than being dropped as a stale snapshot.
// Guards against newly-launched remote orchestrators disappearing from
// the UI when an unrelated delta has bumped the revision in the meantime.
#[test]
fn create_orchestrator_instance_materializes_stale_remote_launch_response() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let template = state
        .create_orchestrator_template(OrchestratorTemplateDraft {
            project_id: Some(local_project_id.clone()),
            ..sample_orchestrator_template_draft()
        })
        .expect("template should be created")
        .template;

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.orchestrators[0].id = "remote-orchestrator-created".to_owned();
    remote_state.orchestrators[0].template_id = template.id.clone();
    remote_state.orchestrators[0].template_snapshot = OrchestratorTemplate {
        id: template.id.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: Some("remote-project-1".to_owned()),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
        created_at: template.created_at.clone(),
        updated_at: template.updated_at.clone(),
    };
    let remote_response = serde_json::to_string(&CreateOrchestratorInstanceResponse {
        orchestrator: remote_state.orchestrators[0].clone(),
        state: remote_state.into_state_response(),
    })
    .expect("remote orchestrator response should encode");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let headers = String::from_utf8_lossy(&buffer[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value.trim())
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buffer.len() < body_start + content_length {
                let bytes_read = stream.read(&mut chunk).expect("request body should read");
                if bytes_read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..bytes_read]);
            }

            let request_head = String::from_utf8_lossy(&buffer[..body_start]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/orchestrators ") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("orchestrator response should write");
                continue;
            }

            panic!("unexpected request: {request_line}");
        }
    });

    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote.clone(),
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(true),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 3);
    }

    let response = state
        .create_orchestrator_instance(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(local_project_id.clone()),
            template: None,
        })
        .expect("stale launch response should still materialize the orchestrator");

    assert_eq!(
        response.orchestrator.remote_orchestrator_id.as_deref(),
        Some("remote-orchestrator-created")
    );
    assert_eq!(response.orchestrator.project_id, local_project_id);
    assert!(
        response
            .state
            .orchestrators
            .iter()
            .any(|instance| instance.id == response.orchestrator.id)
    );
    assert_eq!(
        response.orchestrator.session_instances.len(),
        template.sessions.len()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .find_remote_orchestrator_index(&remote.id, "remote-orchestrator-created")
            .is_some()
    );
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a snapshot whose orchestrator references a remote project
// with no corresponding local project mapping applies without error
// but leaves orchestrator_instances and sessions empty.
// Guards against assigning an empty-string local project id to
// orchestrators or sessions when the remote/local project pairing is
// missing.
#[test]
fn remote_snapshot_sync_skips_orchestrators_without_a_local_project_mapping() {
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
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
        state
            .commit_locked(&mut inner)
            .expect("remote should persist");
    }

    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-unmapped",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            )
            .into_state_response(),
        )
        .expect("snapshot should still apply even when orchestration localization fails");

    let snapshot = state.full_snapshot();
    assert!(snapshot.orchestrators.is_empty());
    assert!(snapshot.sessions.is_empty());

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.orchestrator_instances.is_empty());
    assert!(inner.sessions.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that pause / resume / stop on a mirrored orchestrator each
// issue POST /api/orchestrators/{remote_id}/{action} to the remote (with
// a preceding health check), apply the returned state snapshot locally,
// and update the UI-visible status accordingly.
// Guards against lifecycle actions being applied only locally (which
// would diverge from the remote) or silently swallowing the proxy error.
#[test]
fn remote_orchestrator_lifecycle_actions_proxy_to_remote_backend_and_resync_local_state() {
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
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    state
        .apply_remote_state_snapshot(
            &remote.id,
            sample_remote_orchestrator_state(
                "remote-project-1",
                "/remote/repo",
                1,
                OrchestratorInstanceStatus::Running,
            )
            .into_state_response(),
        )
        .expect("initial remote snapshot should apply");
    let local_orchestrator_id = state
        .snapshot()
        .orchestrators
        .into_iter()
        .find(|instance| {
            instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("remote orchestrator should be mirrored")
        .id;

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let paused_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Paused,
    ))
    .expect("paused state should encode");
    let resumed_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    ))
    .expect("resumed state should encode");
    let stopped_state = serde_json::to_string(&sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        4,
        OrchestratorInstanceStatus::Stopped,
    ))
    .expect("stopped state should encode");
    let server = std::thread::spawn(move || {
        let mut action_responses = vec![
            (
                "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
                paused_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
                resumed_state,
            ),
            (
                "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
                stopped_state,
            ),
        ]
        .into_iter();
        for _ in 0..6 {
            let mut stream = accept_test_connection(&listener, "test listener");
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let bytes_read = stream.read(&mut chunk).expect("request should read");
                assert!(bytes_read > 0, "request closed before headers completed");
                buffer.extend_from_slice(&chunk[..bytes_read]);
                if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end;
                }
            };
            let request_head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let request_line = request_head
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
            captured_for_server
                .lock()
                .expect("capture mutex poisoned")
                .push(request_line.clone());

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            let (expected_request_line, response_body) = action_responses
                .next()
                .expect("action response should still be queued");
            assert_eq!(request_line, expected_request_line);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    )
                    .as_bytes(),
                )
                .expect("state response should write");
        }
    });
    state
        .remote_registry
        .connections
        .lock()
        .expect("remote registry mutex poisoned")
        .insert(
            remote.id.clone(),
            Arc::new(RemoteConnection {
                config: remote.clone(),
                authority_generation: 0,
                retired: AtomicBool::new(false),
                state_continuity_generation: AtomicU64::new(1),
                forwarded_port: port,
                process: Mutex::new(None),
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let paused = state
        .pause_orchestrator_instance(&local_orchestrator_id)
        .expect("pause should proxy successfully");
    assert_eq!(
        paused
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("paused orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Paused
    );

    let resumed = state
        .resume_orchestrator_instance(&local_orchestrator_id)
        .expect("resume should proxy successfully");
    assert_eq!(
        resumed
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("resumed orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Running
    );

    let stopped = state
        .stop_orchestrator_instance(&local_orchestrator_id)
        .expect("stop should proxy successfully");
    assert_eq!(
        stopped
            .orchestrators
            .iter()
            .find(|instance| instance.id == local_orchestrator_id)
            .expect("stopped orchestrator should be present")
            .status,
        OrchestratorInstanceStatus::Stopped
    );

    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/resume HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "POST /api/orchestrators/remote-orchestrator-1/stop HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that when a later snapshot introduces a new orchestrator whose
// localization fails, existing mirrored orchestrators for that remote
// survive intact rather than being cleared in the rollback.
// Guards against an "all or nothing" rollback that wipes healthy
// mirrored orchestrators on a single bad delta.
#[test]
fn remote_snapshot_sync_preserves_existing_orchestrators_when_localization_fails() {
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
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    let mut second_orchestrator = initial_state.orchestrators[0].clone();
    second_orchestrator.id = "remote-orchestrator-2".to_owned();
    second_orchestrator.status = OrchestratorInstanceStatus::Paused;
    initial_state.orchestrators.push(second_orchestrator);
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote snapshot should apply");

    let initial_remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        initial_remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state.orchestrators[0].id = "remote-orchestrator-2".to_owned();
    let mut invalid_orchestrator = invalid_state.orchestrators[0].clone();
    invalid_orchestrator.id = "remote-orchestrator-3".to_owned();
    invalid_orchestrator.session_instances[0].session_id = "missing-remote-session".to_owned();
    invalid_state.orchestrators.push(invalid_orchestrator);

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state.into_state_response())
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let remote_orchestrator_ids = state
        .snapshot()
        .orchestrators
        .into_iter()
        .filter(|instance| instance.remote_id.as_deref() == Some(remote.id.as_str()))
        .filter_map(|instance| instance.remote_orchestrator_id)
        .collect::<HashSet<_>>();
    assert_eq!(
        remote_orchestrator_ids,
        [
            "remote-orchestrator-1".to_owned(),
            "remote-orchestrator-2".to_owned()
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    );
    assert!(!remote_orchestrator_ids.contains("remote-orchestrator-3"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that sessions referenced by a surviving mirrored orchestrator
// are not pruned by session retention logic, even when the incoming
// snapshot drops those session ids because its orchestrator fails to
// localize.
// Guards against the retention pass removing proxy sessions still in
// active use by a mirrored orchestrator.
#[test]
fn remote_snapshot_sync_preserves_sessions_referenced_by_existing_orchestrators_when_localization_fails()
 {
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
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    state
        .apply_remote_state_snapshot(&remote.id, initial_state.into_state_response())
        .expect("initial remote snapshot should apply");

    let (preserved_local_session_id, preserved_preview) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote mirrored session should exist");
        (
            inner.sessions[index].session.id.clone(),
            inner.sessions[index].session.preview.clone(),
        )
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-1");

    state
        .apply_remote_state_snapshot(&remote.id, invalid_state.into_state_response())
        .expect("remote snapshot should still apply when orchestrator localization fails");

    let snapshot = state.full_snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == preserved_local_session_id)
            .expect("referenced mirrored session should remain")
            .preview,
        preserved_preview
    );
    let preserved_orchestrator = snapshot
        .orchestrators
        .iter()
        .find(|instance| {
            instance.remote_id.as_deref() == Some(remote.id.as_str())
                && instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
        })
        .expect("existing mirrored orchestrator should remain");
    assert!(
        preserved_orchestrator
            .session_instances
            .iter()
            .any(|instance| instance.session_id == preserved_local_session_id)
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that sync_remote_state_for_target, given a focused target,
// updates the single focused session even when orchestrator
// localization fails, without creating proxy records for other
// sessions in the payload and without writing any orchestrator entry
// with this remote's id to persisted state.
// Guards against a focused resync accidentally doing a full sync's
// work when its orchestrator leg fails.
#[test]
fn focused_remote_state_sync_rolls_back_proxy_sessions_when_orchestrator_localization_fails() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Before focused sync.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        local_session_id
    };
    let initial_session_count = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions.len()
    };

    let mut invalid_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    invalid_state
        .sessions
        .retain(|session| session.id != "remote-session-3");
    invalid_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Focused sync updated.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    let response_lease = state
        .remote_registry
        .connection(&target.remote)
        .expect("focused sync lease should resolve");
    state
        .sync_remote_state_for_target(
            &target,
            invalid_state.into_state_response(),
            &response_lease,
        )
        .expect("focused remote sync should preserve the target session update");

    let snapshot = state.full_snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Focused sync updated."
    );
    assert!(
        !snapshot
            .orchestrators
            .iter()
            .any(|instance| { instance.remote_id.as_deref() == Some(remote.id.as_str()) })
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.sessions.len(), initial_session_count);
    assert!(
        inner
            .find_remote_session_index(&remote.id, "remote-session-2")
            .is_none()
    );
    drop(inner);

    let persisted_connection =
        rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    let persisted_sessions =
        load_session_records_from_sqlite(&persisted_connection, state.persistence_path.as_path())
            .expect("persisted sessions should load");
    assert_eq!(persisted_sessions.len(), initial_session_count);
    assert!(
        !persisted_sessions.iter().any(|candidate| {
            candidate.remote_session_id.as_deref() == Some("remote-session-2")
        })
    );
    let persisted_focused = persisted_sessions
        .iter()
        .find(|candidate| candidate.remote_session_id.as_deref() == Some("remote-session-1"))
        .expect("focused mirrored session should persist");
    assert_eq!(persisted_focused.session.preview, "Focused sync updated.");
    let persisted = sqlite_metadata_state_value(state.persistence_path.as_path());
    let persisted_orchestrator_instances = persisted["orchestratorInstances"].as_array();
    assert!(persisted_orchestrator_instances.map_or(true, |instances| {
        !instances
            .iter()
            .any(|instance| instance["remoteId"] == Value::String(remote.id.clone()))
    }));
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that sync_remote_state_for_target is a no-op when the payload's
// revision is older than the applied-revision watermark: the target
// session's preview stays at the newer value already mirrored locally.
// Guards against a stale focused fetch from an in-flight request
// clobbering a fresher update.
#[test]
fn focused_remote_state_sync_skips_stale_revision() {
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
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut initial_remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    initial_remote_session.preview = "Newest preview.".to_owned();

    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &initial_remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused remote session should persist");
        inner.note_remote_applied_revision(&remote.id, 3);
        local_session_id
    };

    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    stale_state
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("focused session should remain in the payload")
        .preview = "Stale preview should be skipped.".to_owned();

    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: "remote-session-1".to_owned(),
    };
    let response_lease = state
        .remote_registry
        .connection(&target.remote)
        .expect("stale sync lease should resolve");
    state
        .sync_remote_state_for_target(&target, stale_state.into_state_response(), &response_lease)
        .expect("stale focused sync should be ignored");

    let snapshot = state.full_snapshot();
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("focused local session should remain")
            .preview,
        "Newest preview."
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.should_skip_remote_applied_revision(&remote.id, 3));
    assert!(!inner.should_skip_remote_applied_revision(&remote.id, 4));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn equal_remote_orchestrator_action_snapshot_retries_dirty_persistence() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = RemoteConfig {
        id: "ssh-orchestrator-dirty-retry".to_owned(),
        name: "Orchestrator Dirty Retry".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/orchestrator-dirty-retry",
        "Orchestrator Dirty Retry",
        "remote-project-orchestrator-dirty-retry",
    );
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-orchestrator-dirty-retry",
        "/remote/orchestrator-dirty-retry",
        2,
        OrchestratorInstanceStatus::Running,
    );
    let response_body =
        serde_json::to_string(&remote_state).expect("equal orchestrator state should encode");
    state
        .apply_remote_state_snapshot(&remote.id, remote_state.into_state_response())
        .expect("initial orchestrator state should apply");
    let (local_orchestrator_id, local_session_id) = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let session_index = inner
            .find_remote_session_index(&remote.id, "remote-session-1")
            .expect("remote session should be mirrored");
        (
            inner
                .orchestrator_instances
                .iter()
                .find(|instance| {
                    instance.remote_orchestrator_id.as_deref() == Some("remote-orchestrator-1")
                })
                .expect("remote orchestrator should be mirrored")
                .id
                .clone(),
            inner.sessions[session_index].session.id.clone(),
        )
    };
    let target = state
        .remote_orchestrator_target(&local_orchestrator_id)
        .expect("remote orchestrator target should resolve")
        .expect("orchestrator should be remote");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "orchestrator dirty retry listener");
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
            assert_eq!(
                request.request_line,
                "POST /api/orchestrators/remote-orchestrator-1/pause HTTP/1.1"
            );
            write_test_http_response(
                &mut stream,
                StatusCode::OK,
                "application/json",
                &response_body,
            );
        }
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    state.shutdown_persist_blocking();

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-orchestrator-dirty-retry-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut peer_state_events = state.subscribe_events();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&local_session_id)
            .expect("mirrored session should exist");
        inner
            .session_mut_by_index(index)
            .expect("mirrored session index should remain valid")
            .session
            .preview = "Dirty orchestrator-owned preview.".to_owned();
        state
            .commit_remote_localization_locked(&mut inner)
            .expect_err("the forced persistence failure should arm retry debt");
        assert!(inner.remote_delta_persist_dirty);
    }
    assert!(
        peer_state_events.try_recv().is_err(),
        "the failed localization must not publish a snapshot"
    );

    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    state.persistence_path = original_persistence_path.clone();
    let snapshot = state
        .proxy_remote_orchestrator_state_action(target, "pause")
        .expect("an equal action snapshot should discharge persistence debt");
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("returned snapshot should include the mirrored session")
            .preview,
        "Dirty orchestrator-owned preview."
    );
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
            .expect("the dirty retry should publish the missed snapshot"),
    )
    .expect("peer snapshot should decode");
    assert_eq!(
        peer_snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("peer snapshot should include the mirrored session")
            .preview,
        "Dirty orchestrator-owned preview."
    );
    let reloaded = load_state(original_persistence_path.as_path())
        .expect("retried orchestrator state should load")
        .expect("retried orchestrator state should exist");
    assert_eq!(
        reloaded
            .sessions
            .iter()
            .find(|record| record.session.id == local_session_id)
            .expect("mirrored session should survive reload")
            .session
            .preview,
        "Dirty orchestrator-owned preview."
    );

    join_test_server(server);
    let _ = fs::remove_file(original_persistence_path.as_path());
}

#[test]
fn focused_remote_state_sync_stale_revision_retries_dirty_persistence() {
    let mut state = test_app_state();
    let original_persistence_path = state.persistence_path.clone();
    let remote = RemoteConfig {
        id: "ssh-focused-dirty-retry".to_owned(),
        name: "Focused Dirty Retry".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/focused-dirty-retry",
        "Focused Dirty Retry",
        "remote-project-focused-dirty-retry",
    );
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-focused-dirty-retry",
        "/remote/focused-dirty-retry",
        3,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    remote_session.preview = "Persisted preview.".to_owned();
    let local_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let local_session_id = upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("initial focused state should persist");
        local_session_id
    };
    state.shutdown_persist_blocking();

    let failing_persistence_path =
        std::env::temp_dir().join(format!("termal-focused-dirty-retry-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing_persistence_path)
        .expect("a directory at the persistence path should force failure");
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut peer_state_events = state.subscribe_events();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&local_session_id)
            .expect("focused proxy should exist");
        inner
            .session_mut_by_index(index)
            .expect("focused proxy index should remain valid")
            .session
            .preview = "Dirty authoritative preview.".to_owned();
        inner.note_remote_applied_revision(&remote.id, 3);
        state
            .commit_remote_localization_locked(&mut inner)
            .expect_err("the forced persistence failure should arm retry debt");
        assert!(inner.remote_delta_persist_dirty);
    }
    assert!(
        peer_state_events.try_recv().is_err(),
        "the failed localization must not publish a snapshot"
    );

    fs::remove_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should be removable");
    state.persistence_path = original_persistence_path.clone();
    let target = RemoteSessionTarget {
        local_session_id: local_session_id.clone(),
        remote: remote.clone(),
        remote_session_id: remote_session.id.clone(),
    };
    let response_lease = state
        .remote_registry
        .connection(&remote)
        .expect("focused sync lease should resolve");
    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-focused-dirty-retry",
        "/remote/focused-dirty-retry",
        2,
        OrchestratorInstanceStatus::Running,
    );
    stale_state
        .sessions
        .iter_mut()
        .find(|session| session.id == remote_session.id)
        .expect("stale focused session should exist")
        .preview = "Stale preview must stay ignored.".to_owned();
    state
        .sync_remote_state_for_target(&target, stale_state.into_state_response(), &response_lease)
        .expect("a stale response should still discharge persistence debt");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.remote_delta_persist_dirty);
    let index = inner
        .find_session_index(&local_session_id)
        .expect("focused proxy should remain");
    assert_eq!(
        inner
            .session_by_index(index)
            .expect("focused proxy index should remain valid")
            .session
            .preview,
        "Dirty authoritative preview."
    );
    drop(inner);
    let peer_snapshot: StateResponse = serde_json::from_str(
        &peer_state_events
            .try_recv()
            .expect("the dirty retry should publish the missed snapshot"),
    )
    .expect("peer snapshot should decode");
    assert_eq!(
        peer_snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("peer snapshot should include the focused proxy")
            .preview,
        "Dirty authoritative preview."
    );
    let reloaded = load_state(original_persistence_path.as_path())
        .expect("retried focused state should load")
        .expect("retried focused state should exist");
    assert_eq!(
        reloaded
            .sessions
            .iter()
            .find(|record| record.session.id == local_session_id)
            .expect("focused proxy should survive reload")
            .session
            .preview,
        "Dirty authoritative preview."
    );

    let _ = fs::remove_file(original_persistence_path.as_path());
}

// Pins that a snapshot whose session list omits a previously mirrored
// remote session drops that local proxy record, while leaving remote
// sessions still present and purely local sessions alone.
// Guards against the retention pass being too aggressive (wiping
// unrelated sessions) or too lenient (leaving zombies behind).
#[test]
fn remote_snapshot_sync_removes_missing_proxy_sessions() {
    let state = test_app_state();
    let (kept_local_session_id, removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let kept = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let kept_index = inner
            .find_session_index(&kept.session.id)
            .expect("kept session should exist");
        inner.sessions[kept_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[kept_index].remote_session_id = Some("remote-session-keep".to_owned());

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("removed session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (kept.session.id, removed.session.id, local.session.id)
    };

    let mut remote_state = state.summary_snapshot();
    let mut remote_session = remote_state
        .sessions
        .iter()
        .find(|session| session.id == kept_local_session_id)
        .cloned()
        .expect("kept session should be present in the snapshot");
    remote_session.id = "remote-session-keep".to_owned();
    remote_session.preview = "Remote session still exists.".to_owned();
    remote_state.sessions = vec![remote_session];

    state
        .apply_remote_state_snapshot("ssh-lab", remote_state)
        .expect("remote snapshot should apply");

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == kept_local_session_id)
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.id == removed_local_session_id)
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == local_session_id)
    );
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == kept_local_session_id)
            .expect("kept session should remain")
            .preview,
        "Remote session still exists."
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}
