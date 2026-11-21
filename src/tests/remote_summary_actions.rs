// Exercises compatibility on remote mutation responses, after the peer acts.
use super::*;

#[path = "remote_summary_orchestrator_actions.rs"]
mod orchestrator;

type CapturedRequests = Arc<Mutex<Vec<(String, String)>>>;

fn action_server(
    response: Value,
    action: &str,
) -> (u16, CapturedRequests, std::thread::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let port = listener.local_addr().unwrap().port();
    let action = action.to_owned();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "summary action listener");
            let request = read_test_http_request(&mut stream);
            captured
                .lock()
                .expect("requests mutex poisoned")
                .push((request.request_line.clone(), request.body));
            let body = if request.request_line.starts_with("GET /api/health ") {
                json!({"ok": true, "serverInstanceId": "remote-test-instance"})
            } else {
                assert!(request.request_line.starts_with(&action));
                response.clone()
            };
            write_test_http_response(
                &mut stream,
                StatusCode::OK,
                "application/json",
                &body.to_string(),
            );
        }
    });
    (port, requests, server)
}

fn proxy_session_id(state: &AppState) -> String {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .sessions
        .iter()
        .find(|record| record.remote_session_id.as_deref() == Some("remote-session-1"))
        .expect("proxy session should exist")
        .session
        .id
        .clone()
}

fn message_action(fields: Value, should_apply: bool) {
    let (state, remote, snapshot) = fixture();
    let mut initial = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    );
    for session in &mut initial.sessions {
        session.preview = "Before remote action".to_owned();
    }
    state
        .apply_remote_state_snapshot(&remote.id, initial.into_state_response())
        .expect("initial snapshot should apply");
    let local_id = proxy_session_id(&state);
    let response = payload(&snapshot, "state", &fields);
    let (port, requests, server) =
        action_server(response, "POST /api/sessions/remote-session-1/messages ");
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let result = state.proxy_remote_turn_dispatch(
        &local_id,
        SendMessageRequest {
            text: "Exactly one remote prompt".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: None,
            source_mailbox: None,
        },
    );
    join_test_server(server);
    let captured = requests.lock().expect("requests mutex poisoned");
    assert_eq!(captured.len(), 2, "one health probe and exactly one action");
    assert_eq!(
        captured[1].0,
        "POST /api/sessions/remote-session-1/messages HTTP/1.1"
    );
    let sent: Value = serde_json::from_str(&captured[1].1).expect("prompt body should decode");
    assert_eq!(sent["text"], "Exactly one remote prompt");
    drop(captured);
    if should_apply {
        result.expect("successful peer action must not become a local decode failure");
        let applied = state.full_snapshot();
        let sessions = applied
            .sessions
            .iter()
            .filter(|session| session.remote_id.as_deref() == Some(remote.id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(sessions.len(), snapshot.sessions.len());
        let target = sessions
            .iter()
            .find(|session| session.id == local_id)
            .expect("targeted proxy should remain present");
        assert_eq!(target.preview, "Older host summary");
        for session in sessions {
            if session.id != local_id {
                assert_eq!(session.preview, "Before remote action");
            }
            assert_eq!(session.status, SessionStatus::Idle);
            assert!(!session.queue_paused);
            let projected = serde_json::to_value(session).expect("proxy should encode");
            for retired in ["engramControlOff", "engramBootRecoveryPending", "queueHold"] {
                assert!(projected.get(retired).is_none(), "retained {retired}");
            }
        }
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    } else {
        let error = result.expect_err("unknown field beside retired fields must refuse");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(error.message.contains("futureSummaryField"));
        assert!(
            state
                .full_snapshot()
                .sessions
                .iter()
                .all(|session| session.preview == "Before remote action")
        );
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&1));
    }
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_message_action_accepts_retired_summary_response_without_retry() {
    message_action(retired_fields().pop().unwrap(), true);
}

#[test]
fn remote_message_action_refuses_unknown_field_beside_retired_fields() {
    let mut fields = retired_fields().pop().unwrap();
    fields["futureSummaryField"] = json!(true);
    message_action(fields, false);
}

#[test]
fn remote_stop_and_resume_project_the_owners_queue_pause() {
    let (state, remote, mut initial) = fixture();
    let target = initial
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("remote parent summary");
    target.message_count = 0;
    target.has_runnable_delegation_result = true;
    let mut stopped = initial.clone();
    stopped.revision += 1;
    let target = stopped
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("stopped parent summary");
    target.queue_paused = true;
    let mut resumed = initial.clone();
    resumed.revision = stopped.revision + 1;
    let target = resumed
        .sessions
        .iter_mut()
        .find(|session| session.id == "remote-session-1")
        .expect("resumed parent summary");
    target.has_runnable_delegation_result = false;
    state
        .apply_remote_state_snapshot(&remote.id, initial)
        .expect("initial snapshot should apply");
    let local_id = proxy_session_id(&state);
    let assert_queue = |snapshot: StateResponse, paused, runnable| {
        let summary = snapshot
            .sessions
            .iter()
            .find(|session| session.id == local_id)
            .expect("proxy summary");
        assert_eq!(summary.queue_paused, paused);
        assert_eq!(summary.has_runnable_delegation_result, runnable);
        let detail = state
            .get_session(&local_id)
            .expect("cached proxy detail")
            .session;
        assert_eq!(detail.queue_paused, paused);
        assert_eq!(detail.has_runnable_delegation_result, runnable);
    };
    assert_queue(state.summary_snapshot(), false, true);

    let (port, _, server) = action_server(
        serde_json::to_value(stopped).expect("Stop response"),
        "POST /api/sessions/remote-session-1/stop ",
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let stop = state.request_stop_session(&local_id);
    join_test_server(server);
    assert_queue(stop.expect("Stop should succeed"), true, true);
    assert_queue(state.summary_snapshot(), true, true);

    let (port, _, server) = action_server(
        serde_json::to_value(resumed).expect("Resume response"),
        "POST /api/sessions/remote-session-1/queue/resume ",
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let resume = state.resume_session_queue(&local_id);
    join_test_server(server);
    assert_queue(resume.expect("Resume should succeed"), false, false);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_operator_route_does_not_forward_automated_prompts() {
    let (state, remote, mut snapshot) = fixture();
    state
        .apply_remote_state_snapshot(&remote.id, snapshot.clone())
        .expect("initial snapshot should apply");
    let local_id = proxy_session_id(&state);
    snapshot.revision += 1;
    let (port, requests, server) = action_server(
        serde_json::to_value(snapshot).expect("send response"),
        "POST /api/sessions/remote-session-1/messages ",
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let send = |text: &str, source_session_id, source_mailbox| {
        dispatch_turn_and_snapshot(
            &state,
            &local_id,
            SendMessageRequest {
                text: text.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id,
                source_mailbox,
            },
        )
    };
    for (source_session_id, source_mailbox) in [
        (Some("peer-session".to_owned()), None),
        (
            None,
            Some(MailboxMessageSource {
                mailbox_id: "mailbox-1".to_owned(),
                message_id: "message-1".to_owned(),
                sequence: 1,
                unread_count: 1,
            }),
        ),
    ] {
        let Err(error) = send("Automated prompt", source_session_id, source_mailbox) else {
            panic!("automated prompt must be refused at the proxy");
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(requests.lock().expect("requests mutex poisoned").is_empty());
    }
    assert!(send("Operator prompt", None, None).is_ok());
    join_test_server(server);
    let requests = requests.lock().expect("requests mutex poisoned");
    assert_eq!(
        requests.len(),
        2,
        "only the health probe and operator action"
    );
    let forwarded: Value = serde_json::from_str(&requests[1].1).expect("forwarded prompt");
    assert_eq!(forwarded["text"], "Operator prompt");
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_project_creation_accepts_retired_nested_state_response() {
    let (state, remote, snapshot) = fixture();
    let response = json!({
        "projectId": "created-remote-project",
        "state": payload(&snapshot, "state", &retired_fields().pop().unwrap())
    });
    let (port, requests, server) = action_server(response, "POST /api/projects ");
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let result = state.create_remote_project_proxy(
        CreateProjectRequest {
            name: Some("Created remotely".to_owned()),
            root_path: "/remote/new-project".to_owned(),
            remote_id: remote.id.clone(),
        },
        remote.clone(),
        "/remote/new-project".to_owned(),
    );
    join_test_server(server);
    let captured = requests.lock().expect("requests mutex poisoned");
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[1].0, "POST /api/projects HTTP/1.1");
    drop(captured);
    let response = result.expect("remote create with a historical nested state must succeed");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let project = inner
        .find_project(&response.project_id)
        .expect("created local proxy");
    assert_eq!(
        project.remote_project_id.as_deref(),
        Some("created-remote-project")
    );
    assert_eq!(project.remote_id, remote.id);
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn unknown_fields_beside_retired_fields_still_refuse_all_snapshot_paths() {
    let mut fields = retired_fields().pop().unwrap();
    fields["futureSummaryField"] = json!(true);
    for kind in ["state", "sessionCreated", "orchestratorsUpdated"] {
        let (state, remote, snapshot) = fixture();
        let value = payload(&snapshot, kind, &fields);
        let event = if kind == "state" { "state" } else { "delta" };
        let error = dispatch_remote_event(&state, &remote.id, event, &[value.to_string()])
            .expect_err("unknown field next to retired fields must refuse");
        assert!(format!("{error:#}").contains("futureSummaryField"));
        assert_unapplied(&state, &remote);
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
    http_resync(fields, false);
}
