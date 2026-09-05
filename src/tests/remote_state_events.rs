//! Remote state-event, SSE parser, and lagged-resync tests.
//!
//! Split out of remote.rs to keep remote event-stream recovery coverage in a
//! focused module.

use super::remote::{
    make_remote_session_summary_only, materialize_remote_proxy_session_transcript_for_test,
    remote_text_message, spawn_remote_session_response_server, spawn_remote_state_response_server,
};
use super::remote_delta_replay::local_replay_test_remote;
use super::*;

// Pins the current TermAl-to-TermAl wire contract at deserialization time.
// Remote peers must send both their process identity and the byte boundary
// needed to apply an incremental text append safely; neither field has a
// compatibility default.
#[test]
fn current_remote_wire_requires_instance_id_and_text_delta_offset() {
    let health_error = match serde_json::from_value::<HealthResponse>(json!({
        "ok": true,
    })) {
        Ok(_) => panic!("current health responses must carry serverInstanceId"),
        Err(error) => error,
    };
    assert!(
        health_error
            .to_string()
            .contains("missing field `serverInstanceId`")
    );
    let empty_health_error = match serde_json::from_value::<HealthResponse>(json!({
        "ok": true,
        "serverInstanceId": ""
    })) {
        Ok(_) => panic!("current health responses must carry a non-empty serverInstanceId"),
        Err(error) => error,
    };
    assert!(
        empty_health_error
            .to_string()
            .contains("serverInstanceId must be a non-empty string")
    );

    let mut empty_state = serde_json::to_value(
        sample_remote_orchestrator_state(
            "remote-project-1",
            "/remote/repo",
            1,
            OrchestratorInstanceStatus::Running,
        )
        .as_state_response(),
    )
    .expect("remote state should encode");
    empty_state["serverInstanceId"] = json!("");
    let empty_state_error = match serde_json::from_value::<StateResponse>(empty_state) {
        Ok(_) => panic!("current state responses must carry a non-empty serverInstanceId"),
        Err(error) => error,
    };
    assert!(
        empty_state_error
            .to_string()
            .contains("serverInstanceId must be a non-empty string")
    );

    let delta_error = match serde_json::from_value::<DeltaEvent>(json!({
        "type": "textDelta",
        "revision": 2,
        "sessionId": "remote-session-1",
        "messageId": "remote-message-1",
        "messageIndex": 0,
        "messageCount": 1,
        "delta": " world"
    })) {
        Ok(_) => panic!("current text deltas must carry textStartByte"),
        Err(error) => error,
    };
    assert!(
        delta_error
            .to_string()
            .contains("missing field `textStartByte`")
    );
}

// Pins the live SSE consumer recovery path, not only the low-level delta
// validator. A byte-offset gap must reject the incremental mutation and fetch
// one bounded authoritative tail before any corrupt text can become visible.
#[test]
fn remote_text_delta_gap_triggers_bounded_authoritative_tail_repair() {
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

    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Hello".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("remote-message-1", "Hello")];
    initial_state.sessions[0].messages_loaded = true;
    initial_state.sessions[0].message_count = 1;
    let initial_session = initial_state
        .sessions
        .into_iter()
        .find(|session| session.id == "remote-session-1")
        .expect("initial remote session should exist");
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: initial_session.id.clone(),
                session: test_state_session_summary_from_session(&initial_session),
            },
        )
        .expect("initial remote session delta should apply");
    materialize_remote_proxy_session_transcript_for_test(&state, &remote, &initial_session);

    let mut repaired_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    );
    repaired_state.sessions[0].preview = "Hello world".to_owned();
    repaired_state.sessions[0].messages =
        vec![remote_text_message("remote-message-1", "Hello world")];
    repaired_state.sessions[0].messages_loaded = true;
    repaired_state.sessions[0].message_count = 1;
    repaired_state.sessions[0].session_mutation_stamp = Some(11);
    let repaired_session = repaired_state
        .sessions
        .into_iter()
        .find(|session| session.id == "remote-session-1")
        .expect("repaired remote session should exist");
    let (port, requests, server) = spawn_remote_session_response_server(SessionResponse {
        revision: 3,
        session: repaired_session,
        server_instance_id: "remote-instance".to_owned(),
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let discontinuous_delta = DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        // The producer had already emitted one byte that this proxy missed.
        // Appending at local byte 5 would silently produce corrupt text.
        text_start_byte: 6,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(11),
    };
    let delta_data_lines =
        vec![serde_json::to_string(&discontinuous_delta).expect("text delta should encode")];
    dispatch_remote_event(&state, &remote.id, "delta", &delta_data_lines)
        .expect("a rejected text delta should recover from an authoritative bounded tail");

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert_eq!(
        request_lines,
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/sessions/remote-session-1?tail=64 HTTP/1.1".to_owned(),
        ],
        "a byte gap should trigger exactly one bounded session-tail fetch"
    );
    join_test_server(server);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy session should remain");
    let record = &inner.sessions[index];
    assert!(matches!(
        &record.session.messages[0],
        Message::Text { text, .. } if text == "Hello world"
    ));
    assert_eq!(record.session.preview, "Hello world");
    assert_eq!(record.session.session_mutation_stamp, Some(11));
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&3));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_tail_repair_failure_falls_back_to_full_state_resync() {
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
    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Before recovery".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("remote-message-1", "Hello")];
    initial_state.sessions[0].messages_loaded = true;
    initial_state.sessions[0].message_count = 1;
    let initial_session = initial_state.sessions[0].clone();
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: initial_session.id.clone(),
                session: test_state_session_summary_from_session(&initial_session),
            },
        )
        .expect("initial remote session should apply");
    materialize_remote_proxy_session_transcript_for_test(&state, &remote, &initial_session);

    let mut repaired_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        3,
        OrchestratorInstanceStatus::Running,
    );
    repaired_state.sessions[0].preview = "Recovered from full state".to_owned();
    repaired_state.sessions[0].messages =
        vec![remote_text_message("remote-message-1", "Hello world")];
    repaired_state.sessions[0].messages_loaded = true;
    repaired_state.sessions[0].message_count = 1;
    repaired_state.sessions[0].session_mutation_stamp = Some(11);
    let repaired_body =
        serde_json::to_string(&repaired_state).expect("repaired state should encode");

    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let mut stream = accept_test_connection(&listener, "remote repair fallback listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push(request.request_line.clone());
            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
                );
            } else if request
                .request_line
                .starts_with("GET /api/sessions/remote-session-1?tail=64 ")
            {
                write_test_http_response(
                    &mut stream,
                    StatusCode::BAD_GATEWAY,
                    "application/json",
                    r#"{"error":"tail unavailable"}"#,
                );
            } else if request.request_line.starts_with("GET /api/state ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &repaired_body,
                );
            } else {
                panic!("unexpected request: {}", request.request_line);
            }
        }
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let discontinuous_delta = DeltaEvent::TextDelta {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        text_start_byte: 6,
        delta: " world".to_owned(),
        preview: Some("Hello world".to_owned()),
        session_mutation_stamp: Some(11),
    };
    dispatch_remote_event(
        &state,
        &remote.id,
        "delta",
        &[serde_json::to_string(&discontinuous_delta).expect("delta should encode")],
    )
    .expect("failed bounded repair should recover through full state");

    assert_eq!(
        requests.lock().expect("requests mutex poisoned").as_slice(),
        [
            "GET /api/health HTTP/1.1",
            "GET /api/sessions/remote-session-1?tail=64 HTTP/1.1",
            "GET /api/health HTTP/1.1",
            "GET /api/state HTTP/1.1",
        ]
    );
    join_test_server(server);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, "remote-session-1")
        .expect("remote proxy should remain");
    assert_eq!(
        inner.sessions[index].session.preview,
        "Recovered from full state"
    );
    drop(inner);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that a marked empty-state SSE "state" event triggers one full
// /api/state resync per revision, deduplicates repeated sends of the
// same revision, and still resyncs when the revision bumps.
// Guards against a chatty remote forcing the local backend into a
// tight resync loop over identical fallback payloads.
#[test]
fn remote_state_event_dedupes_marked_sse_fallback_resyncs_by_revision() {
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
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.remotes.push(remote.clone());
        let publication = state
            .remote_registry
            .publish_configs(&inner.preferences.remotes);
        drop(inner);
        state.remote_registry.finish_config_publication(publication);
    }
    let (remote_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let remote_record = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local_record = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let remote_index = inner
            .find_session_index(&remote_record.session.id)
            .expect("remote session should exist");
        inner.sessions[remote_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[remote_index].remote_session_id = Some("remote-session-keep".to_owned());

        (remote_record.session.id, local_record.session.id)
    };

    let mut first_full_state_response = state.snapshot();
    let mut first_remote_session = first_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    first_remote_session.id = "remote-session-keep".to_owned();
    first_remote_session.preview = "Hydrated from /api/state v1".to_owned();
    first_full_state_response.sessions = vec![first_remote_session];
    let first_full_state_response =
        serde_json::to_string(&first_full_state_response).expect("state response should encode");

    let mut second_full_state_response = state.snapshot();
    let mut second_remote_session = second_full_state_response
        .sessions
        .iter()
        .find(|session| session.id == remote_local_session_id)
        .cloned()
        .expect("remote session should be present in the snapshot");
    second_remote_session.id = "remote-session-keep".to_owned();
    second_remote_session.preview = "Hydrated from /api/state v2".to_owned();
    second_full_state_response.revision = second_full_state_response.revision.saturating_add(1);
    second_full_state_response.sessions = vec![second_remote_session];
    let second_full_state_response =
        serde_json::to_string(&second_full_state_response).expect("state response should encode");

    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_for_server = captured.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        let mut state_responses =
            vec![first_full_state_response, second_full_state_response].into_iter();
        for _ in 0..4 {
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

            if request_line.starts_with("GET /api/state ") {
                let response = state_responses
                    .next()
                    .expect("state response should still be queued");
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            response.len(),
                            response
                        )
                        .as_bytes(),
                    )
                    .expect("state response should write");
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
                event_bridge_started: AtomicBool::new(false),
                event_bridge_shutdown: AtomicBool::new(false),
            }),
        );

    let mut first_fallback_payload: Value = serde_json::from_str(
        &fallback_state_events_payload(0, "remote-instance".to_owned())
            .expect("fallback payload should encode"),
    )
    .expect("fallback payload should parse");
    first_fallback_payload["revision"] = json!(4);
    let first_data_lines = serde_json::to_string_pretty(&first_fallback_payload)
        .expect("first fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut second_fallback_payload = first_fallback_payload.clone();
    second_fallback_payload["revision"] = json!(5);
    let second_data_lines = serde_json::to_string_pretty(&second_fallback_payload)
        .expect("second fallback payload should encode")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("first fallback state payload should trigger a resync");
    dispatch_remote_event(&state, "ssh-lab", "state", &first_data_lines)
        .expect("duplicate fallback revision should be deduped");
    dispatch_remote_event(&state, "ssh-lab", "state", &second_data_lines)
        .expect("newer fallback revision should trigger another resync");

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.id == remote_local_session_id)
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
            .find(|session| session.id == remote_local_session_id)
            .expect("remote mirrored session should remain")
            .preview,
        "Hydrated from /api/state v2"
    );
    assert_eq!(
        captured.lock().expect("capture mutex poisoned").clone(),
        vec![
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
            "GET /api/health HTTP/1.1".to_owned(),
            "GET /api/state HTTP/1.1".to_owned(),
        ]
    );
    join_test_server(server);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that the fallback-resync watermark is keyed per remote id, is
// monotonic (older revisions for the same remote stay skipped), treats
// a different remote id as independent, and is cleared by
// clear_remote_sse_fallback_resync.
// Guards against cross-remote contamination or a sticky watermark
// that blocks resync after a reconnect.
#[test]
fn remote_sse_fallback_resync_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    state.note_remote_sse_fallback_resync("ssh-lab", 0);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 0));

    state.note_remote_sse_fallback_resync("ssh-lab", 1);
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    state.clear_remote_sse_fallback_resync("ssh-lab");
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 1));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab", 0));
    assert!(!state.should_skip_remote_sse_fallback_resync("ssh-lab-2", 1));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins StateInner::{note,should_skip}_remote_applied_revision and the
// delta variant: the snapshot watermark skips <= revisions once noted,
// the delta watermark skips only strictly-older revisions, and a lower
// note() call is a no-op.
// Guards against the dedupe predicates letting already-applied state
// replay or blocking fresh state.
#[test]
fn state_inner_remote_applied_revision_methods_cover_monotonic_cases() {
    let mut inner = StateInner::new();

    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));

    inner.note_remote_applied_revision("ssh-lab", 1);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));

    inner.note_remote_applied_revision("ssh-lab", 4);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(inner.should_skip_remote_applied_delta_revision("ssh-lab", 3));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));

    inner.note_remote_applied_revision("ssh-lab", 2);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 4));
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 4));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));

    inner.note_remote_applied_snapshot_revision("ssh-lab", 6);
    assert!(inner.should_skip_remote_applied_revision("ssh-lab", 6));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 6));
    assert!(inner.should_skip_remote_applied_delta_revision("ssh-lab", 5));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 7));

    inner.note_remote_applied_snapshot_revision("ssh-lab", 8);
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 8));
    assert!(inner.should_skip_remote_applied_delta_revision("ssh-lab", 7));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 9));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 1));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab-2", 1));
}

// Pins the snapshot-specific predicate because it intentionally differs
// from both generic state snapshots and transcript deltas.
#[test]
fn state_inner_remote_applied_snapshot_revision_predicate_covers_boundary_cases() {
    let mut inner = StateInner::new();

    inner.note_remote_applied_revision("applied-only", 10);
    assert!(!inner.should_skip_remote_applied_snapshot_revision("applied-only", 10));
    assert!(inner.should_skip_remote_applied_snapshot_revision("applied-only", 9));
    assert!(!inner.should_skip_remote_applied_snapshot_revision("applied-only", 11));

    inner.note_remote_applied_snapshot_revision("snapshot-only", 10);
    assert!(inner.should_skip_remote_applied_snapshot_revision("snapshot-only", 10));
    assert!(inner.should_skip_remote_applied_snapshot_revision("snapshot-only", 9));
    assert!(!inner.should_skip_remote_applied_snapshot_revision("snapshot-only", 11));
    assert!(!inner.should_skip_remote_applied_snapshot_revision("other-remote", 10));
}

// Pins that decode_remote_json strips control characters, replaces
// CRLF with spaces, and truncates raw non-JSON error bodies past the
// MAX_REMOTE_ERROR_BODY_CHARS limit before surfacing them.
// Guards against a hostile/broken remote injecting log-splitting or
// control sequences, or flooding logs with a huge error body.
#[test]
fn decode_remote_json_sanitizes_and_caps_raw_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_body = format!(
        "<html>\0Service\tUnavailable\r\nDetails {}{}\u{7}</html>",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body_len = noisy_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, noisy_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("raw non-JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Service Unavailable Details"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Pins that decode_remote_json applies the same sanitization + cap to
// the "error" field of a structured JSON error body, stripping control
// chars and appending "..." when truncated.
// Guards against attackers smuggling log-splitting sequences via the
// JSON "error" field instead of the raw body.
#[test]
fn decode_remote_json_sanitizes_structured_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let noisy_message = format!(
        "Remote\tfailure\r\n{}{}\u{7}",
        "A".repeat(600),
        "B".repeat(32)
    );
    let response_body = serde_json::to_string(&json!({
        "error": noisy_message,
    }))
    .expect("structured error response should encode");
    let response_body_len = response_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, response_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("structured JSON 503 should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(err.message.contains("Remote failure"));
    assert!(err.message.chars().count() <= 512);
    assert!(err.message.ends_with("..."));
    assert!(!err.message.contains('\r'));
    assert!(!err.message.contains('\n'));
    assert!(!err.message.contains('\t'));
    assert!(!err.message.chars().any(|ch| ch.is_control()));

    join_test_server(server);
}

// Pins that decode_remote_json rejects response bodies exceeding
// MAX_REMOTE_ERROR_BODY_BYTES with the fixed message "remote error
// response too large" rather than reading the full body.
// Guards against memory exhaustion from a remote replying with a
// multi-megabyte error body.
#[test]
fn decode_remote_json_rejects_oversized_error_bodies() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let oversized_body = "X".repeat(70_000);
    let response_body_len = oversized_body.as_bytes().len();
    let server = std::thread::spawn(move || {
        let mut stream = accept_test_connection(&listener, "test listener");
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        stream
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    response_body_len, oversized_body
                )
                .as_bytes(),
            )
            .expect("error response should write");
    });

    let client = BlockingHttpClient::builder()
        .build()
        .expect("test HTTP client should build");
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .expect("error response should still be returned");
    let err = match decode_remote_json::<HealthResponse>(response) {
        Ok(_) => panic!("oversized error response should surface as an ApiError"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.message, "remote error response too large");

    join_test_server(server);
}

// Pins that applied-revision tracking on StateInner is keyed per
// remote id, stays monotonic (a lower note() call does not lower the
// watermark), and that clear_remote_applied_revision resets it without
// touching other remotes.
// Guards against cross-remote dedupe contamination and against a
// stuck watermark after event-stream teardown.
#[test]
fn remote_applied_revision_tracking_is_per_remote_and_monotonic() {
    let state = test_app_state();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
        inner.note_remote_applied_revision("ssh-lab", 0);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 0));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 0));

        inner.note_remote_applied_snapshot_revision("ssh-lab", 2);
        inner.note_remote_applied_revision("ssh-lab", 1);
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 2));
        assert!(inner.should_skip_remote_applied_revision("ssh-lab", 1));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 3));
        assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));
        assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));
        assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    }

    state.clear_remote_applied_revision("ssh-lab");
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_delta_revision("ssh-lab", 2));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab", 0));
    assert!(!inner.should_skip_remote_applied_revision("ssh-lab-2", 2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that an SSE "state" event carrying an ordinary (non-fallback)
// empty snapshot is applied directly: remote proxy sessions go away,
// local-only sessions stay.
// Guards against mistaking a genuine empty-state resync for a
// fallback marker and skipping its session retention work.
#[test]
fn remote_state_event_applies_non_fallback_empty_snapshot_payload() {
    let state = test_app_state();
    let (removed_local_session_id, local_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let removed = inner.create_session(Agent::Codex, None, "/tmp".to_owned(), None, None);
        let local = inner.create_session(Agent::Claude, None, "/tmp".to_owned(), None, None);

        let removed_index = inner
            .find_session_index(&removed.session.id)
            .expect("remote session should exist");
        inner.sessions[removed_index].remote_id = Some("ssh-lab".to_owned());
        inner.sessions[removed_index].remote_session_id = Some("remote-session-gone".to_owned());

        (removed.session.id, local.session.id)
    };

    let mut remote_state = empty_state_events_response("remote-instance".to_owned());
    remote_state.revision = 1;
    let data_lines =
        vec![serde_json::to_string(&remote_state).expect("state payload should encode")];
    dispatch_remote_event(&state, "ssh-lab", "state", &data_lines)
        .expect("ordinary empty state payload should apply");

    let snapshot = state.full_snapshot();
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
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        inner.remote_snapshot_applied_revisions.get("ssh-lab"),
        Some(&1)
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn retired_bridge_cannot_apply_buffered_frames_or_clear_current_continuity() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-retired-bridge".to_owned(),
        name: "Retired Bridge".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/retired-bridge",
        "Retired Bridge Project",
        "remote-project-retired-bridge",
    );
    let old_connection = state
        .remote_registry
        .connection(&remote)
        .expect("current bridge route should resolve")
        .connection;

    let mut current_state = sample_remote_orchestrator_state(
        "remote-project-retired-bridge",
        "/remote/retired-bridge",
        2,
        OrchestratorInstanceStatus::Running,
    );
    current_state.sessions[0].preview = "Current endpoint preview".to_owned();
    let mut current_payload =
        serde_json::to_value(&current_state).expect("current state should encode");
    current_payload["_sseFallback"] = Value::Bool(false);
    let current_frame = format!(
        "event: state\ndata: {}\n\n",
        serde_json::to_string(&current_payload).expect("current payload should encode")
    );
    process_remote_event_stream_reader_with_authority(
        &state,
        &remote.id,
        Some((&remote, &old_connection)),
        std::io::Cursor::new(current_frame),
        &mut String::new(),
        &mut Vec::new(),
        &mut RemoteEventStreamRecovery::default(),
    )
    .expect("the current bridge should apply its frame");

    let mut replacement = remote.clone();
    replacement.host = Some("new.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    let publication = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let current = inner
            .preferences
            .remotes
            .iter_mut()
            .find(|candidate| candidate.id == remote.id)
            .expect("remote settings should contain the bridge route");
        *current = replacement.clone();
        let publication = state
            .remote_registry
            .publish_configs(&inner.preferences.remotes);
        for remote_id in &publication.changed_ids {
            let _ = state.clear_remote_applied_revision_locked(&mut inner, remote_id);
            let _ = state.clear_remote_sse_fallback_resync(remote_id);
        }
        publication
    };
    state.remote_registry.finish_config_publication(publication);
    assert!(old_connection.retired.load(Ordering::SeqCst));

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.note_remote_applied_revision(&remote.id, 9);
    }
    state.note_remote_sse_fallback_resync(&remote.id, 9);

    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-retired-bridge",
        "/remote/retired-bridge",
        10,
        OrchestratorInstanceStatus::Running,
    );
    stale_state.sessions[0].preview = "Stale old endpoint preview".to_owned();
    let mut stale_payload = serde_json::to_value(&stale_state).expect("stale state should encode");
    let mut stale_fallback_payload = stale_payload.clone();
    stale_fallback_payload["_sseFallback"] = Value::Bool(true);
    let stale_fallback_frame = format!(
        "event: state\ndata: {}\n\n",
        serde_json::to_string(&stale_fallback_payload)
            .expect("stale fallback payload should encode")
    );
    let fallback_error = process_remote_event_stream_reader_with_authority(
        &state,
        &remote.id,
        Some((&remote, &old_connection)),
        std::io::Cursor::new(stale_fallback_frame),
        &mut String::new(),
        &mut Vec::new(),
        &mut RemoteEventStreamRecovery::default(),
    )
    .expect_err("a retired bridge must reject its buffered fallback state frame");
    assert!(
        fallback_error
            .to_string()
            .contains(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST)
    );
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .get(&remote.id),
        Some(&9)
    );
    assert!(state.should_skip_remote_sse_fallback_resync(&remote.id, 9));

    stale_payload["_sseFallback"] = Value::Bool(false);
    let stale_frame = format!(
        "event: state\ndata: {}\n\n",
        serde_json::to_string(&stale_payload).expect("stale payload should encode")
    );
    let error = process_remote_event_stream_reader_with_authority(
        &state,
        &remote.id,
        Some((&remote, &old_connection)),
        std::io::Cursor::new(stale_frame),
        &mut String::new(),
        &mut Vec::new(),
        &mut RemoteEventStreamRecovery::default(),
    )
    .expect_err("a retired bridge must reject its buffered frame");
    assert!(
        error
            .to_string()
            .contains(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST)
    );
    let mut stale_delta_session = stale_state.sessions[0].clone();
    stale_delta_session.id = "remote-session-from-retired-delta".to_owned();
    let stale_delta = DeltaEvent::SessionCreated {
        revision: 11,
        session_id: stale_delta_session.id.clone(),
        session: test_state_session_summary_from_session(&stale_delta_session),
    };
    let stale_delta_frame = format!(
        "event: delta\ndata: {}\n\n",
        serde_json::to_string(&stale_delta).expect("stale delta should encode")
    );
    let delta_error = process_remote_event_stream_reader_with_authority(
        &state,
        &remote.id,
        Some((&remote, &old_connection)),
        std::io::Cursor::new(stale_delta_frame),
        &mut String::new(),
        &mut Vec::new(),
        &mut RemoteEventStreamRecovery::default(),
    )
    .expect_err("a retired bridge must reject its buffered delta");
    assert!(
        delta_error
            .to_string()
            .contains(REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST)
    );
    assert!(
        !state.clear_remote_bridge_continuity_if_current(&remote, &old_connection),
        "retired bridge cleanup must not erase its replacement's watermarks"
    );

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Current endpoint preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Stale old endpoint preview")
    );
    assert_eq!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .get(&remote.id),
        Some(&9)
    );
    assert!(state.should_skip_remote_sse_fallback_resync(&remote.id, 9));
    assert!(
        state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .find_remote_session_index(&remote.id, "remote-session-from-retired-delta")
            .is_none()
    );

    let current_connection = state
        .remote_registry
        .connection(&replacement)
        .expect("replacement bridge route should resolve")
        .connection;
    assert!(state.clear_remote_bridge_continuity_if_current(&replacement, &current_connection));
    assert!(
        !state
            .inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .contains_key(&remote.id)
    );
    assert!(!state.should_skip_remote_sse_fallback_resync(&remote.id, 9));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the same-connection race between request localization and bridge
// disconnect cleanup. Both leases are issued before cleanup. The newer request
// applies first, cleanup then clears its revision watermark, and the older
// request must still fail instead of using the cleared gate to regress state.
#[test]
fn bridge_continuity_cleanup_invalidates_pre_cleanup_request_leases() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-bridge-continuity".to_owned(),
        name: "Bridge Continuity".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    create_test_remote_project(
        &state,
        &remote,
        "/remote/bridge-continuity",
        "Bridge Continuity Project",
        "remote-project-bridge-continuity",
    );

    let newer_lease = state
        .remote_registry
        .connection(&remote)
        .expect("newer request lease should resolve");
    let stale_lease = state
        .remote_registry
        .connection(&remote)
        .expect("stale request lease should resolve");
    assert!(Arc::ptr_eq(
        &newer_lease.connection,
        &stale_lease.connection
    ));
    assert_eq!(
        newer_lease.state_continuity_generation,
        stale_lease.state_continuity_generation
    );

    let mut newer_state = sample_remote_orchestrator_state(
        "remote-project-bridge-continuity",
        "/remote/bridge-continuity",
        8,
        OrchestratorInstanceStatus::Running,
    );
    newer_state.sessions[0].preview = "Newer request preview".to_owned();
    state
        .apply_remote_state_snapshot_for_request(
            &newer_lease,
            newer_state.into_state_response(),
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        )
        .expect("newer same-connection response should localize before cleanup");

    assert!(state.clear_remote_bridge_continuity_if_current(&remote, &newer_lease.connection));
    assert_ne!(
        newer_lease.state_continuity_generation,
        newer_lease
            .connection
            .state_continuity_generation
            .load(Ordering::SeqCst)
    );

    let mut stale_state = sample_remote_orchestrator_state(
        "remote-project-bridge-continuity",
        "/remote/bridge-continuity",
        7,
        OrchestratorInstanceStatus::Running,
    );
    stale_state.sessions[0].preview = "Stale request preview".to_owned();
    let error = state
        .apply_remote_state_snapshot_for_request(
            &stale_lease,
            stale_state.into_state_response(),
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        )
        .expect_err("pre-cleanup request lease must not cross continuity cleanup");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Newer request preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Stale request preview")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins remote SSE `lagged` handling. A remote backend emits `lagged` followed
// immediately by a recovery `state` snapshot after its broadcast receiver drops
// frames. That recovery can have the same revision as a state snapshot the
// bridge already applied; the marker must therefore bypass the same-revision
// snapshot gate exactly once.
#[test]
fn remote_lagged_marker_force_applies_next_same_revision_state_snapshot() {
    let state = test_app_state();
    let mut recovery = RemoteEventStreamRecovery::default();
    let remote = local_replay_test_remote();
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
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Initial remote preview".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("message-1", "Initial body")];
    initial_state.sessions[0].message_count = 1;
    let initial_session = initial_state
        .sessions
        .into_iter()
        .find(|session| session.id == "remote-session-1")
        .expect("initial remote session should exist");
    state
        .apply_remote_delta_event(
            "ssh-lab",
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: initial_session.id.clone(),
                session: test_state_session_summary_from_session(&initial_session),
            },
        )
        .expect("initial remote session delta should apply");
    materialize_remote_proxy_session_transcript_for_test(&state, &remote, &initial_session);

    let mut repaired_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    repaired_state.sessions[0].preview = "Lagged repaired preview".to_owned();
    repaired_state.sessions[0].messages =
        vec![remote_text_message("message-1", "Lagged repaired body")];
    let repaired_data_lines =
        vec![serde_json::to_string(&repaired_state).expect("state payload should encode")];

    dispatch_remote_event_with_recovery(&state, "ssh-lab", "lagged", &[], &mut recovery)
        .expect("bare lagged marker should arm recovery");
    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &repaired_data_lines,
        &mut recovery,
    )
    .expect("same-revision lagged recovery state should apply");

    let mut repeated_same_revision_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    repeated_same_revision_state.sessions[0].preview =
        "Unexpected second same-revision preview".to_owned();
    repeated_same_revision_state.sessions[0].messages =
        vec![remote_text_message("message-1", "Unexpected repeated body")];
    repeated_same_revision_state.sessions[0].message_count = 1;
    let repeated_same_revision_data_lines = vec![
        serde_json::to_string(&repeated_same_revision_state).expect("state payload should encode"),
    ];
    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &repeated_same_revision_data_lines,
        &mut recovery,
    )
    .expect("repeated same-revision state should be ignored after marker consumption");

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Lagged repaired preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Initial remote preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Unexpected second same-revision preview")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_event_stream_parser_dispatches_bare_lagged_at_eof() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
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
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Initial EOF remote preview".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("message-1", "Initial EOF body")];
    initial_state.sessions[0].message_count = 1;
    let initial_data_lines =
        vec![serde_json::to_string(&initial_state).expect("state payload should encode")];
    let mut recovery = RemoteEventStreamRecovery::default();

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &initial_data_lines,
        &mut recovery,
    )
    .expect("initial remote state should apply");

    let mut event_name = String::new();
    let mut data_lines = Vec::new();
    process_remote_event_stream_reader(
        &state,
        "ssh-lab",
        std::io::Cursor::new("event: lagged\n"),
        &mut event_name,
        &mut data_lines,
        &mut recovery,
    )
    .expect("EOF-terminated bare lagged frame should dispatch");
    assert!(
        event_name.is_empty() && data_lines.is_empty(),
        "EOF dispatch should clear parser scratch buffers for reuse"
    );

    let mut repaired_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    repaired_state.sessions[0].preview = "EOF lagged repaired preview".to_owned();
    repaired_state.sessions[0].messages =
        vec![remote_text_message("message-1", "EOF lagged repaired body")];
    repaired_state.sessions[0].message_count = 1;
    let repaired_data_lines =
        vec![serde_json::to_string(&repaired_state).expect("state payload should encode")];

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &repaired_data_lines,
        &mut recovery,
    )
    .expect("same-revision EOF lagged recovery state should apply");

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "EOF lagged repaired preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Initial EOF remote preview")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_event_stream_parser_clears_buffers_after_eof_state_dispatch() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );

    let mut remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    remote_state.sessions[0].preview = "EOF state preview".to_owned();
    remote_state.sessions[0].messages = vec![remote_text_message("message-1", "EOF state body")];
    remote_state.sessions[0].message_count = 1;
    let mut payload_value =
        serde_json::to_value(&remote_state).expect("state payload should encode");
    payload_value["_sseFallback"] = serde_json::Value::Bool(false);
    let payload = serde_json::to_string(&payload_value).expect("state payload should encode");
    let mut event_name = String::new();
    let mut data_lines = Vec::new();
    let mut recovery = RemoteEventStreamRecovery::default();

    process_remote_event_stream_reader(
        &state,
        "ssh-lab",
        std::io::Cursor::new(format!("event: state\ndata: {payload}\n")),
        &mut event_name,
        &mut data_lines,
        &mut recovery,
    )
    .expect("EOF-terminated state frame should dispatch");

    assert!(
        event_name.is_empty() && data_lines.is_empty(),
        "EOF state dispatch should clear parser scratch buffers for reuse"
    );
    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "EOF state preview")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_lagged_marker_clears_on_empty_data_bearing_frame() {
    for empty_event_name in ["state", "delta"] {
        let state = test_app_state();
        let remote = local_replay_test_remote();
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
            2,
            OrchestratorInstanceStatus::Running,
        );
        initial_state.sessions[0].preview = format!("Initial empty {empty_event_name} preview");
        initial_state.sessions[0].messages = vec![remote_text_message(
            "message-1",
            &format!("Initial empty {empty_event_name} body"),
        )];
        initial_state.sessions[0].message_count = 1;
        let initial_data_lines =
            vec![serde_json::to_string(&initial_state).expect("state payload should encode")];
        let mut recovery = RemoteEventStreamRecovery::default();

        dispatch_remote_event_with_recovery(
            &state,
            "ssh-lab",
            "state",
            &initial_data_lines,
            &mut recovery,
        )
        .expect("initial remote state should apply");
        dispatch_remote_event_with_recovery(&state, "ssh-lab", "lagged", &[], &mut recovery)
            .expect("bare lagged marker should arm recovery");
        dispatch_remote_event_with_recovery(
            &state,
            "ssh-lab",
            empty_event_name,
            &[],
            &mut recovery,
        )
        .expect("empty data-bearing frame should clear stale lagged recovery");

        let mut repaired_state = sample_remote_orchestrator_state(
            "remote-project-1",
            "/remote/repo",
            2,
            OrchestratorInstanceStatus::Running,
        );
        repaired_state.sessions[0].preview =
            format!("Unexpected empty {empty_event_name} repaired preview");
        repaired_state.sessions[0].messages = vec![remote_text_message(
            "message-1",
            &format!("Unexpected empty {empty_event_name} repaired body"),
        )];
        repaired_state.sessions[0].message_count = 1;
        let repaired_data_lines =
            vec![serde_json::to_string(&repaired_state).expect("state payload should encode")];

        dispatch_remote_event_with_recovery(
            &state,
            "ssh-lab",
            "state",
            &repaired_data_lines,
            &mut recovery,
        )
        .expect("same-revision state after intervening empty frame should be handled");

        let snapshot = state.full_snapshot();
        assert!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.preview
                    == format!("Initial empty {empty_event_name} preview")),
            "initial state should remain after empty {empty_event_name} frame"
        );
        assert!(
            !snapshot.sessions.iter().any(|session| {
                session.preview == format!("Unexpected empty {empty_event_name} repaired preview")
            }),
            "same-revision state after empty {empty_event_name} frame must not force-apply"
        );
        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[test]
fn remote_lagged_marker_does_not_force_apply_after_intervening_delta_progress() {
    let state = test_app_state();
    let mut recovery = RemoteEventStreamRecovery::default();
    let remote = local_replay_test_remote();
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
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Initial remote preview".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("message-1", "Initial body")];
    initial_state.sessions[0].messages_loaded = true;
    initial_state.sessions[0].message_count = 1;
    let initial_session = initial_state
        .sessions
        .into_iter()
        .find(|session| session.id == "remote-session-1")
        .expect("initial remote session should exist");
    state
        .apply_remote_delta_event(
            "ssh-lab",
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: initial_session.id.clone(),
                session: test_state_session_summary_from_session(&initial_session),
            },
        )
        .expect("initial remote session delta should apply");
    materialize_remote_proxy_session_transcript_for_test(&state, &remote, &initial_session);

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "lagged",
        &["1".to_owned()],
        &mut recovery,
    )
    .expect("lagged marker should arm recovery");

    let newer_delta = DeltaEvent::MessageUpdated {
        revision: 3,
        session_id: "remote-session-1".to_owned(),
        message_id: "message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("message-1", "Newer delta body"),
        preview: "Newer delta preview".to_owned(),
        status: SessionStatus::Active,
        session_mutation_stamp: Some(12),
    };
    let newer_delta_data_lines =
        vec![serde_json::to_string(&newer_delta).expect("delta payload should encode")];
    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "delta",
        &newer_delta_data_lines,
        &mut recovery,
    )
    .expect("newer remote delta should apply");

    let mut stale_recovery = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    stale_recovery.sessions[0].preview = "Stale lagged recovery preview".to_owned();
    stale_recovery.sessions[0].messages = vec![remote_text_message(
        "message-1",
        "Stale lagged recovery body",
    )];
    stale_recovery.sessions[0].messages_loaded = true;
    stale_recovery.sessions[0].message_count = 1;
    let stale_recovery_data_lines =
        vec![serde_json::to_string(&stale_recovery).expect("state payload should encode")];

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &stale_recovery_data_lines,
        &mut recovery,
    )
    .expect("stale lagged recovery state should be ignored");

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Newer delta preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Stale lagged recovery preview")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_lagged_marker_force_applies_same_revision_fallback_resync_snapshot() {
    let state = test_app_state();
    let mut recovery = RemoteEventStreamRecovery::default();
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

    let mut initial_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    initial_state.sessions[0].preview = "Initial remote preview".to_owned();
    initial_state.sessions[0].messages = vec![remote_text_message("message-1", "Initial body")];
    initial_state.sessions[0].messages_loaded = true;
    initial_state.sessions[0].message_count = 1;
    let initial_data_lines =
        vec![serde_json::to_string(&initial_state).expect("state payload should encode")];

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &initial_data_lines,
        &mut recovery,
    )
    .expect("initial remote state should apply");

    let mut repaired_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        2,
        OrchestratorInstanceStatus::Running,
    );
    repaired_state.sessions[0].preview = "Fallback repaired preview".to_owned();
    repaired_state.sessions[0].messages =
        vec![remote_text_message("message-1", "Fallback repaired body")];
    repaired_state.sessions[0].messages_loaded = true;
    repaired_state.sessions[0].message_count = 1;
    let (port, requests, server) =
        spawn_remote_state_response_server(repaired_state.into_state_response());
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let mut fallback_marker = empty_state_events_response("remote-instance".to_owned());
    fallback_marker.revision = 2;
    let mut fallback_value =
        serde_json::to_value(&fallback_marker).expect("fallback marker should encode");
    fallback_value["_sseFallback"] = serde_json::Value::Bool(true);
    let fallback_data_lines = vec![fallback_value.to_string()];

    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "lagged",
        &["1".to_owned()],
        &mut recovery,
    )
    .expect("lagged marker should arm recovery");
    dispatch_remote_event_with_recovery(
        &state,
        "ssh-lab",
        "state",
        &fallback_data_lines,
        &mut recovery,
    )
    .expect("fallback lagged recovery should resync");

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("GET /api/state ")),
        "expected remote state fallback fetch, saw {request_lines:?}"
    );
    join_test_server(server);

    let snapshot = state.full_snapshot();
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Fallback repaired preview")
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .any(|session| session.preview == "Initial remote preview")
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the per-session in-flight guard for remote delta transcript repair.
// When several deltas arrive for the same unloaded remote proxy, only the
// first one should perform the blocking `/api/sessions/{id}` hydration.
#[test]
fn remote_delta_hydration_skips_duplicate_in_flight_same_session_fetch() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    let local_project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut remote_session, 2);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        upsert_remote_proxy_session_record(
            &mut inner,
            &remote.id,
            &remote_session,
            Some(local_project_id),
        );
        state
            .commit_locked(&mut inner)
            .expect("remote summary should persist");
    }

    let authority_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    let hydration_key = RemoteDeltaHydrationKey {
        remote_id: remote.id.clone(),
        remote_session_id: remote_session.id.clone(),
        authority_generation,
    };
    state
        .remote_delta_hydrations_in_flight
        .lock()
        .expect("remote delta hydration mutex poisoned")
        .insert(hydration_key.clone());

    let outcome = state
        .hydrate_unloaded_remote_session_for_delta(
            &remote.id,
            &remote_session.id,
            authority_generation,
            2,
            2,
            None,
            None,
            None,
            None,
        )
        .expect("duplicate in-flight hydration should not fail");

    assert_eq!(outcome, RemoteDeltaHydrationOutcome::SkipInFlight);
    assert!(
        state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .contains(&hydration_key)
    );
}

#[test]
fn remote_delta_hydration_in_flight_skips_narrow_unloaded_delta_apply() {
    let state = test_app_state();
    let remote = local_replay_test_remote();
    create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Project",
        "remote-project-1",
    );
    let mut remote_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut remote_session, 1);
    remote_session.preview = "Remote summary before delta".to_owned();
    remote_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: remote_session.id.clone(),
                session: test_state_session_summary_from_session(&remote_session),
            },
        )
        .expect("remote summary session create delta should apply");

    let authority_generation = state
        .remote_registry
        .config_generation
        .load(Ordering::Acquire);
    state
        .remote_delta_hydrations_in_flight
        .lock()
        .expect("remote delta hydration mutex poisoned")
        .insert(RemoteDeltaHydrationKey {
            remote_id: remote.id.clone(),
            remote_session_id: remote_session.id.clone(),
            authority_generation,
        });

    let mut delta_rx = state.subscribe_delta_events();
    let event = DeltaEvent::MessageCreated {
        revision: 3,
        session_id: remote_session.id.clone(),
        message_id: "remote-message-1".to_owned(),
        message_index: 0,
        message_count: 1,
        message: remote_text_message("remote-message-1", "Delta should wait for hydration."),
        preview: "Delta should wait for hydration.".to_owned(),
        status: SessionStatus::Idle,
        session_mutation_stamp: Some(10),
    };
    let replay_key =
        AppState::remote_delta_replay_key_for_generation(&remote.id, authority_generation, &event);

    state
        .apply_remote_delta_event(&remote.id, event)
        .expect("in-flight hydration should skip the narrow delta without failing");

    assert!(
        delta_rx.try_recv().is_err(),
        "in-flight hydration skip should not publish a narrow message delta",
    );
    assert!(
        !state.should_skip_remote_applied_delta_replay(&replay_key),
        "an in-flight hydration skip must not mark the skipped narrow delta as replayed",
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, &remote_session.id)
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(!record.session.messages_loaded);
    assert!(record.session.messages.is_empty());
    assert_eq!(record.session.preview, "Remote summary before delta");
    assert_eq!(inner.remote_applied_revisions.get(&remote.id), Some(&2));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn remote_delta_hydration_burst_uses_one_fetch_and_skips_duplicate_delta() {
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
    let mut summary_session = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/repo",
        1,
        OrchestratorInstanceStatus::Running,
    )
    .sessions
    .into_iter()
    .find(|session| session.id == "remote-session-1")
    .expect("sample remote session should exist");
    make_remote_session_summary_only(&mut summary_session, 1);
    summary_session.preview = "Remote summary before burst".to_owned();
    summary_session.session_mutation_stamp = Some(9);
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: summary_session.id.clone(),
                session: test_state_session_summary_from_session(&summary_session),
            },
        )
        .expect("remote summary session create delta should apply");

    let mut hydrated_session = summary_session.clone();
    hydrated_session.messages = vec![remote_text_message("remote-message-1", "Hydrated body.")];
    hydrated_session.messages_loaded = true;
    hydrated_session.message_count = 1;
    hydrated_session.preview = "Hydrated body.".to_owned();
    hydrated_session.session_mutation_stamp = Some(10);
    let response_body = serde_json::to_string(&SessionResponse {
        revision: 3,
        session: hydrated_session,
        server_instance_id: "remote-instance".to_owned(),
    })
    .expect("session response should encode");
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let (session_request_tx, session_request_rx) = mpsc::channel::<()>();
    let (release_response_tx, release_response_rx) = mpsc::channel::<()>();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut stream = accept_test_connection(&listener, "remote hydration burst listener");
            let request = read_test_http_request(&mut stream);
            requests_for_server
                .lock()
                .expect("requests mutex poisoned")
                .push(request.request_line.clone());

            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
                );
                continue;
            }

            if request
                .request_line
                .starts_with("GET /api/sessions/remote-session-1?tail=64 ")
            {
                let _ = session_request_tx.send(());
                release_response_rx
                    .recv()
                    .expect("test should release the session response");
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &response_body,
                );
                continue;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );

    let first_state = state.clone();
    let first_remote_id = remote.id.clone();
    let first_session_id = summary_session.id.clone();
    let first = std::thread::spawn(move || {
        first_state.apply_remote_delta_event(
            &first_remote_id,
            DeltaEvent::MessageCreated {
                revision: 3,
                session_id: first_session_id,
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                message: remote_text_message("remote-message-1", "Hydrated body."),
                preview: "Hydrated body.".to_owned(),
                status: SessionStatus::Idle,
                session_mutation_stamp: Some(10),
            },
        )
    });

    recv_within_guard(
        &session_request_rx,
        "first delta should start targeted hydration",
    )
    .expect("first delta should start targeted hydration");

    let second_state = state.clone();
    let second_remote_id = remote.id.clone();
    let second_session_id = summary_session.id.clone();
    let second = std::thread::spawn(move || {
        second_state.apply_remote_delta_event(
            &second_remote_id,
            DeltaEvent::TextDelta {
                revision: 4,
                session_id: second_session_id,
                message_id: "remote-message-1".to_owned(),
                message_index: 0,
                message_count: 1,
                text_start_byte: "Hydrated body".len(),
                delta: " duplicate".to_owned(),
                preview: Some("Hydrated body duplicate".to_owned()),
                session_mutation_stamp: Some(11),
            },
        )
    });
    second
        .join()
        .expect("second delta thread should not panic")
        .expect("duplicate in-flight delta should skip without falling through");

    release_response_tx
        .send(())
        .expect("test should release session response");
    first
        .join()
        .expect("first delta thread should not panic")
        .expect("first delta should hydrate the transcript");
    join_test_server(server);

    let request_lines = requests.lock().expect("requests mutex poisoned").clone();
    let session_fetch_count = request_lines
        .iter()
        .filter(|line| line.starts_with("GET /api/sessions/remote-session-1?tail=64 "))
        .count();
    assert_eq!(
        session_fetch_count, 1,
        "same-session burst should issue only one targeted hydration fetch: {request_lines:?}",
    );
    assert!(
        !state
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .iter()
            .any(|key| {
                key.remote_id == remote.id && key.remote_session_id == summary_session.id
            }),
        "successful hydration should clear the in-flight guard for the remote session",
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_remote_session_index(&remote.id, &summary_session.id)
        .expect("remote proxy session should exist");
    let record = &inner.sessions[index];
    assert!(record.session.messages_loaded);
    assert_eq!(record.session.preview, "Hydrated body.");
    assert!(matches!(
        record.session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hydrated body."
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}
