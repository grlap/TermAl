//! Explicit remote session paths belong to the remote filesystem. Exercise
//! routing through the existing finite HTTP/connection seams, never SSH.
use super::*;
use super::project_creation_races::remote_config;

fn request(project_id: String, workdir: String) -> CreateSessionRequest {
    CreateSessionRequest {
        agent: Some(Agent::Codex), name: Some("Remote directory".to_owned()),
        workdir: Some(workdir), project_id: Some(project_id), model: None,
        approval_policy: None, reasoning_effort: None, sandbox_mode: None,
        cursor_mode: None, claude_approval_mode: None, claude_effort: None,
        gemini_approval_mode: None, opencode_approval_mode: None,
        kimi_approval_mode: None, kimi_mode: None,
    }
}

fn remote_workdir_case(case: &str) {
    let state = test_app_state();
    let remote = remote_config("remote-workdir");
    let root = format!("/Users/termal-remote-fixture-{}/repo", Uuid::new_v4());
    let workdir = if case == "outside" {
        format!("{root}/../outside")
    } else {
        // The owner, not Central, trims and canonicalizes this request.
        format!(" {root}/sub/../work ")
    };
    assert!(!FsPath::new(workdir.trim()).exists(), "the remote-only path is absent locally");
    let project_id = create_test_remote_project(&state, &remote, &root,
        "Remote Directory", "owner-project");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    insert_test_remote_connection(&state, &remote, port, TestRemoteBridgeOwnership::Claimed);
    let connection = state.remote_registry.connections.lock().unwrap()[&remote.id].clone();
    let references = Arc::strong_count(&connection);
    let revision = state.inner.lock().unwrap().revision;
    let sessions = state.inner.lock().unwrap().sessions.len();
    let expected_error = match case {
        "missing" => Some(format!("remote owner: `{}` is not a directory", workdir.trim())),
        "outside" => Some("remote owner: session workdir must stay inside project `Remote Directory`".to_owned()),
        "inside" => None,
        _ => unreachable!(),
    };
    let response = if let Some(error) = &expected_error {
        serde_json::to_string(&json!({"error": error})).unwrap()
    } else {
        let mut remote_state = sample_remote_orchestrator_state("owner-project", &root, 7,
            OrchestratorInstanceStatus::Running);
        let mut session = remote_state.sessions.remove(0);
        session.workdir = format!("{root}/work");
        serde_json::to_string(&CreateSessionResponse {
            session_id: session.id.clone(), session, revision: 7,
            server_instance_id: "remote-test-instance".to_owned(),
        }).unwrap()
    };
    let status = if expected_error.is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    let expected_workdir = workdir.clone();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote workdir listener");
            let incoming = read_test_http_request(&mut stream);
            if incoming.request_line.starts_with("GET /api/health ") {
                write_test_http_response(&mut stream, StatusCode::OK, "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#);
                continue;
            }
            assert!(incoming.request_line.starts_with("POST /api/sessions "));
            let body: Value = serde_json::from_str(&incoming.body).unwrap();
            assert_eq!(body["projectId"], "owner-project");
            assert_eq!(body["workdir"], expected_workdir, "forward original bytes to the owner");
            write_test_http_response(&mut stream, status, "application/json", &response);
            break;
        }
    });
    let result = state.create_session_with_agent_setup_validator(
        request(project_id.clone(), workdir), |_, _| panic!("Central must not preflight a remote directory"));
    join_test_server(server);
    if let Some(expected) = expected_error {
        let error = match result { Err(error) => error, Ok(_) => panic!("owner refusal must survive") };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, expected);
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions.len(), sessions, "a refused create produces no proxy");
        assert_eq!(inner.revision, revision, "a refused create commits no durable state");
        assert!(!inner.remote_applied_revisions.contains_key(&remote.id));
    } else {
        let created = result.expect("the remote-only inside-root path reaches its owner");
        assert_eq!(created.session.project_id.as_deref(), Some(project_id.as_str()));
        assert_eq!(created.session.workdir, format!("{root}/work"));
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions.len(), sessions + 1);
        let index = inner.find_session_index(&created.session_id).unwrap();
        assert_eq!(inner.sessions[index].remote_session_id.as_deref(), Some("remote-session-1"));
    }
    assert_eq!(Arc::strong_count(&connection), references, "request leases do not escape");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_only_inside_root_workdir_reaches_the_owning_host() { remote_workdir_case("inside"); }

#[test]
fn remote_directory_error_keeps_its_cause_without_creating_a_proxy() { remote_workdir_case("missing"); }

#[test]
fn remote_outside_root_refusal_is_preserved_without_creating_a_proxy() { remote_workdir_case("outside"); }

#[test]
fn local_explicit_paths_still_require_a_directory_inside_the_project() {
    let state = test_app_state();
    let root = test_temp_dir().join(format!("session-workdir-{}", Uuid::new_v4()));
    let project_root = root.join("project");
    let inside = project_root.join("inside");
    let outside = root.join("outside");
    fs::create_dir_all(&inside).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let project = create_test_project(&state, &project_root, "Local Directory");
    let inside_string = inside.to_string_lossy().into_owned();
    let created = state.create_session_with_agent_setup_validator(
        request(project.clone(), inside_string.clone()), |_, workdir| {
            assert_eq!(workdir, resolve_session_workdir(&inside_string).unwrap());
            Ok(())
        }).unwrap();
    assert_eq!(created.session.project_id.as_deref(), Some(project.as_str()));
    let count = state.inner.lock().unwrap().sessions.len();
    for path in [outside, project_root.join("missing")] {
        let error = match state.create_session_with_agent_setup_validator(
            request(project.clone(), path.to_string_lossy().into_owned()),
            |_, _| panic!("invalid local directories fail before readiness")) {
            Err(error) => error, Ok(_) => panic!("invalid local directory was accepted"),
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("must stay inside project") || error.message.contains("is not a directory"));
        assert_eq!(state.inner.lock().unwrap().sessions.len(), count);
    }
    remove_test_directory(root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}
