//! Remote terminal proxy and terminal-stream framing tests.
//!
//! Split out of remote.rs so terminal forwarding limits and stream protocol
//! behavior stay isolated from state-sync coverage.

use super::*;

fn replace_remote_settings_for_terminal_authority_test(state: &AppState, remote: RemoteConfig) {
    state
        .update_app_settings(UpdateAppSettingsRequest {
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
            remotes: Some(vec![RemoteConfig::local(), remote]),
        })
        .expect("remote settings replacement should succeed");
}

// Pins that when a remote responds 200 OK to /api/terminal/run/stream
// with a non-SSE content-type (e.g. text/html), the local proxy emits
// a BAD_GATEWAY "unexpected content type" error event and does not
// fall back to the non-stream /api/terminal/run JSON route.
// Guards against silently accepting malformed stream responses or
// double-executing the command by falling back after success.
#[tokio::test]
async fn remote_terminal_stream_rejects_successful_non_sse_without_json_fallback() {
    let saw_stream_request = Arc::new(AtomicBool::new(false));
    let saw_stream_request_for_server = saw_stream_request.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote terminal stream listener");
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
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
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

            if request_line.starts_with("GET /api/health ") {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: 53\r\n\r\n{\"ok\":true,\"serverInstanceId\":\"remote-test-instance\"}",
                    )
                    .expect("health response should write");
                continue;
            }

            if request_line.starts_with("POST /api/terminal/run/stream ") {
                saw_stream_request_for_server.store(true, Ordering::SeqCst);
                let body = "already accepted";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .expect("html stream response should write");
                break;
            }

            if request_line.starts_with("POST /api/terminal/run ") {
                panic!("successful non-SSE stream responses must not fall back to JSON execution");
            }

            panic!("unexpected request: {request_line}");
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-stream-html".to_owned(),
        name: "SSH Stream HTML".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream HTML",
        "remote-stream-html-project",
    );
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
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    let event = next_sse_event(&mut body).await;
    let (event_name, event_data) = parse_sse_event(&event);
    assert_eq!(event_name, "error");
    let payload: Value = serde_json::from_str(&event_data).expect("error event should decode");
    assert_eq!(
        payload["status"],
        Value::from(StatusCode::BAD_GATEWAY.as_u16())
    );
    assert!(
        payload["error"]
            .as_str()
            .unwrap()
            .contains("unexpected content type"),
        "unexpected SSE error payload: {payload}"
    );
    assert!(saw_stream_request.load(Ordering::SeqCst));
    join_test_server(server);
}

// Pins that 404 and 405 from the current remote stream route are surfaced
// directly and never retried through the JSON endpoint. Guards against
// restoring the removed legacy-route fallback and double-executing commands.
#[tokio::test]
async fn remote_terminal_stream_never_retries_404_or_405_through_json() {
    assert_remote_terminal_stream_status_is_not_retried(StatusCode::NOT_FOUND).await;
    assert_remote_terminal_stream_status_is_not_retried(StatusCode::METHOD_NOT_ALLOWED).await;
}

async fn assert_remote_terminal_stream_status_is_not_retried(stream_status: StatusCode) {
    let request_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let request_lines_for_server = request_lines.clone();
    let client_observed_result = Arc::new(AtomicBool::new(false));
    let client_observed_result_for_server = client_observed_result.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let fallback_response = serde_json::to_string(&TerminalCommandResponse {
        command: "echo no retry".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "unexpected fallback\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let server = std::thread::spawn(move || {
        let mut stream_response_sent = false;
        loop {
            let mut stream = if stream_response_sent {
                loop {
                    if client_observed_result_for_server.load(Ordering::SeqCst) {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream
                                .set_nonblocking(false)
                                .expect("accepted test socket should support blocking mode");
                            break stream;
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(err) => panic!("remote terminal no-retry listener failed: {err}"),
                    }
                }
            } else {
                accept_test_connection_with_timeout(
                    &listener,
                    "remote terminal no-retry listener",
                    std::time::Duration::from_secs(10),
                )
            };
            let request = read_test_http_request(&mut stream);
            request_lines_for_server
                .lock()
                .expect("request lines mutex poisoned")
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
                .starts_with("POST /api/terminal/run/stream ")
            {
                write_test_http_response(
                    &mut stream,
                    stream_status,
                    "application/json",
                    r#"{"error":"stream route unavailable"}"#,
                );
                stream_response_sent = true;
                continue;
            }

            if request.request_line.starts_with("POST /api/terminal/run ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    &fallback_response,
                );
                return;
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: format!("ssh-stream-no-retry-{}", stream_status.as_u16()),
        name: format!("SSH Stream No Retry {}", stream_status.as_u16()),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        &format!("Remote Stream No Retry {}", stream_status.as_u16()),
        &format!("remote-stream-no-retry-project-{}", stream_status.as_u16()),
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo no retry",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = Box::pin(response.into_body().into_data_stream());
    let event = next_sse_event(&mut body).await;
    client_observed_result.store(true, Ordering::SeqCst);
    let (event_name, event_data) = parse_sse_event(&event);
    assert_eq!(event_name, "error");
    let payload: Value = serde_json::from_str(&event_data).expect("error event should decode");
    assert_eq!(payload["status"], Value::from(stream_status.as_u16()));
    assert_eq!(payload["error"], "stream route unavailable");

    join_test_server(server);
    let request_lines = request_lines.lock().expect("request lines mutex poisoned");
    assert_eq!(
        request_lines
            .iter()
            .filter(|line| line.starts_with("POST /api/terminal/run/stream "))
            .count(),
        1,
        "remote stream route should be requested once: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.starts_with("POST /api/terminal/run ")),
        "remote JSON route must not be retried: {request_lines:?}"
    );
}

// Pins that a remote SSE response to /api/terminal/run/stream is
// forwarded frame-for-frame with output events ahead of a single
// complete event and no error event, and that the non-stream JSON
// fallback is not invoked.
// Guards against reordering, duplicated complete frames, or the
// fallback firing alongside a successful stream.
#[tokio::test]
async fn remote_terminal_stream_proxies_successful_sse_output() {
    let request_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let request_lines_for_server = request_lines.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let remote_response = TerminalCommandResponse {
        command: "echo remote".to_owned(),
        duration_ms: 11,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "remote chunk\nremote done\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    };
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection_with_timeout(
                &listener,
                "remote terminal SSE listener",
                std::time::Duration::from_secs(10),
            );
            let request = read_test_http_request(&mut stream);
            request_lines_for_server
                .lock()
                .expect("request lines mutex poisoned")
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
                .starts_with("POST /api/terminal/run/stream ")
            {
                let output = json!({ "stream": "stdout", "text": "remote chunk\n" });
                let complete = serde_json::to_string(&remote_response)
                    .expect("remote terminal response should encode");
                let body = format!(
                    "event: output\ndata: {output}\n\nevent: complete\ndata: {complete}\n\n"
                );
                write_test_http_response(&mut stream, StatusCode::OK, "text/event-stream", &body);
                break;
            }

            if request.request_line.starts_with("POST /api/terminal/run ") {
                panic!("successful remote stream should not fall back to JSON execution");
            }

            panic!("unexpected request: {}", request.request_line);
        }
    });

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "ssh-stream-sse".to_owned(),
        name: "SSH Stream SSE".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Stream SSE",
        "remote-stream-sse-project",
    );
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    let app = app_router(state);

    let response = request_response(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run/stream")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": "echo remote",
                    "projectId": project_id,
                    "workdir": "/remote/repo",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let events = collect_sse_events(response).await;
    let output_index = events
        .iter()
        .position(|(event_name, _)| event_name == "output")
        .expect("remote output should be forwarded");
    let complete_events = events
        .iter()
        .enumerate()
        .filter(|(_, (event_name, _))| event_name == "complete")
        .collect::<Vec<_>>();
    assert_eq!(complete_events.len(), 1, "events: {events:?}");
    assert!(
        output_index < complete_events[0].0,
        "remote output should precede completion: {events:?}"
    );
    assert!(
        events.iter().all(|(event_name, _)| event_name != "error"),
        "successful remote stream should not emit an error: {events:?}"
    );
    let output: Value =
        serde_json::from_str(&events[output_index].1).expect("output event should decode");
    assert_eq!(output["stream"], Value::String("stdout".to_owned()));
    assert_eq!(output["text"], Value::String("remote chunk\n".to_owned()));
    let complete = serde_json::from_str::<TerminalCommandResponse>(&complete_events[0].1.1)
        .expect("complete event should decode");
    assert_eq!(complete.stdout, "remote chunk\nremote done\n");
    assert!(complete.success);

    join_test_server(server);
    let request_lines = request_lines.lock().expect("request lines mutex poisoned");
    assert!(
        request_lines
            .iter()
            .any(|line| line.starts_with("POST /api/terminal/run/stream ")),
        "remote stream route was not requested: {request_lines:?}"
    );
    assert!(
        request_lines
            .iter()
            .all(|line| !line.starts_with("POST /api/terminal/run ")),
        "remote JSON fallback should not run on successful SSE: {request_lines:?}"
    );
}

// Pins parse_terminal_sse_frame's handling of default `message` event
// names, `:` comment lines, CRLF line endings, multi-line `data:`
// concatenation via `\n`, and the mixed-delimiter detection in
// find_sse_frame_delimiter.
// Guards against subtle SSE-parser regressions that would drop comment
// lines or mis-join multi-line data fields.
#[test]
fn terminal_sse_parser_handles_default_events_comments_and_multiline_data() {
    assert_eq!(
        parse_terminal_sse_frame("data: hello"),
        Some(("message".to_owned(), "hello".to_owned()))
    );
    assert_eq!(
        parse_terminal_sse_frame(
            ": keepalive\r\nevent: output\r\ndata: first\r\ndata: second\r\nid: ignored"
        ),
        Some(("output".to_owned(), "first\nsecond".to_owned()))
    );
    assert_eq!(
        find_sse_frame_delimiter(b"event: output\r\rrest"),
        Some(("event: output".len(), 2))
    );
}

// Pins that a remote `event: error` SSE frame carrying its own status
// (503 here) is normalized to a local BAD_GATEWAY with a message
// including "remote terminal stream error (503)".
// Guards against the remote dictating client-visible status codes
// (e.g. pretending to be 5xx/4xx from our server).
#[test]
fn remote_terminal_stream_error_frames_are_normalized_to_bad_gateway() {
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let err = match handle_remote_terminal_sse_frame(
        r#"event: error
data: {"error":"backend unavailable","status":503}"#,
        &tx,
        &mut forward_state,
    ) {
        Ok(_) => panic!("remote embedded errors should not propagate remote-selected statuses"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert!(
        err.message.contains("remote terminal stream error (503)"),
        "unexpected normalized error message: {}",
        err.message
    );
}

// Pins that a remote error frame with status 429 is specifically kept
// as TOO_MANY_REQUESTS (not downgraded to BAD_GATEWAY) and that
// annotate_remote_terminal_429 adds a "remote <name>:" prefix so the
// UI can attribute the throttling to the remote.
// Guards against losing the 429 signal and against unattributed
// throttling messages.
#[test]
fn remote_terminal_stream_error_frames_preserve_429_for_remote_annotation() {
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let err = match handle_remote_terminal_sse_frame(
        r#"event: error
data: {"error":"too many local terminal commands are already running; limit is 4","status":429}"#,
        &tx,
        &mut forward_state,
    ) {
        Ok(_) => panic!("remote embedded 429 should surface as a throttling error"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        err.message.contains("remote terminal stream error (429)"),
        "unexpected throttling error message: {}",
        err.message
    );

    let annotated = annotate_remote_terminal_429(err, "SSH Terminal Limit");
    assert_eq!(annotated.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        annotated
            .message
            .starts_with("remote SSH Terminal Limit: remote terminal stream error (429)"),
        "remote 429 should include the remote display-name prefix: {}",
        annotated.message
    );
}

// Pins that handle_remote_terminal_sse_frame truncates an oversized
// `output` stdout chunk to TERMINAL_OUTPUT_MAX_BYTES, marks
// RemoteTerminalForwardState::output_truncated, and then propagates
// that flag onto the terminal response when the `complete` frame
// arrives without it.
// Guards against streaming a remote's oversized chunk straight to the
// client and against missing the truncation flag at completion.
#[test]
fn remote_terminal_stream_output_is_capped_before_forwarding() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();
    let oversized_output = "x".repeat(TERMINAL_OUTPUT_MAX_BYTES + 128);
    let frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stdout", "text": oversized_output })
    );

    assert!(
        handle_remote_terminal_sse_frame(&frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );

    let event = rx.try_recv().expect("capped output should be forwarded");
    match event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stdout);
            assert_eq!(text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        }
        _ => panic!("expected output event"),
    }
    assert!(forward_state.output_truncated);

    let complete = format!(
        "event: complete\ndata: {}\n",
        serde_json::to_string(&TerminalCommandResponse {
            command: "remote".to_owned(),
            duration_ms: 1,
            exit_code: Some(0),
            output_truncated: false,
            shell: "sh".to_owned(),
            stderr: String::new(),
            stdout: "ok".to_owned(),
            success: true,
            timed_out: false,
            workdir: "/repo".to_owned(),
        })
        .unwrap()
    );
    let response = handle_remote_terminal_sse_frame(&complete, &tx, &mut forward_state)
        .unwrap()
        .expect("complete event should finish remote stream");
    assert!(response.output_truncated);
}

// Pins that cap_terminal_response_output maintains independent
// per-stream budgets: when stdout is 500 KiB and stderr 100 KiB (each
// within its own TERMINAL_OUTPUT_MAX_BYTES budget), both survive
// unchanged and the function returns false (no truncation).
// Guards against a shared counter that would truncate stderr just
// because stdout was large.
#[test]
fn cap_terminal_response_output_uses_independent_stdout_and_stderr_budgets() {
    let stdout = "o".repeat(500 * 1024);
    let stderr = "e".repeat(100 * 1024);
    let mut response = TerminalCommandResponse {
        command: "remote".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: stderr.clone(),
        stdout: stdout.clone(),
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };

    assert!(!cap_terminal_response_output(&mut response));
    assert_eq!(response.stdout, stdout);
    assert_eq!(response.stderr, stderr);
}

// Pins that when both stdout and stderr exceed
// TERMINAL_OUTPUT_MAX_BYTES, cap_terminal_response_output truncates
// each independently to the budget (preserving the original bytes up
// to the cap) and returns true.
// Guards against only one stream being truncated or the function
// failing to report truncation when both are capped.
#[test]
fn cap_terminal_response_output_truncates_both_streams_above_their_budgets() {
    // Regression guard for the "both streams exceed budget" truncation case.
    // Each stream should be truncated independently to
    // `TERMINAL_OUTPUT_MAX_BYTES`, and the function must report truncation.
    let stdout_fill = "a".repeat(TERMINAL_OUTPUT_MAX_BYTES * 2);
    let stderr_fill = "b".repeat(TERMINAL_OUTPUT_MAX_BYTES * 2);
    let mut response = TerminalCommandResponse {
        command: "remote".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: stderr_fill,
        stdout: stdout_fill,
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };

    assert!(cap_terminal_response_output(&mut response));
    assert_eq!(response.stdout.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert_eq!(response.stderr.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert!(response.stdout.bytes().all(|byte| byte == b'a'));
    assert!(response.stderr.bytes().all(|byte| byte == b'b'));
}

// Pins that a full-budget stdout frame followed by a small stderr
// frame still forwards both: stdout is allowed through at exactly its
// per-stream budget without tripping output_truncated, and the later
// stderr event is delivered rather than silently dropped by a shared
// byte counter.
// Guards against a regression to a single shared forward counter
// across stdout and stderr.
#[test]
fn remote_terminal_stream_forwards_stderr_even_when_stdout_filled_its_budget() {
    // Regression for the shared-counter bug: before per-stream tracking,
    // `RemoteTerminalForwardState.forwarded_output_bytes` was a single
    // counter shared across stdout + stderr, so a remote that sent a full
    // `TERMINAL_OUTPUT_MAX_BYTES` stdout chunk followed by a small stderr
    // chunk saw the live stderr event silently dropped and the final
    // completion response marked `output_truncated = true` even though the
    // per-stream `cap_terminal_response_output` would not have truncated
    // either stream.
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let mut forward_state = RemoteTerminalForwardState::new();

    let stdout_fill = "a".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let stdout_frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stdout", "text": stdout_fill.clone() })
    );
    assert!(
        handle_remote_terminal_sse_frame(&stdout_frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );
    let stdout_event = rx.try_recv().expect("stdout event should be forwarded");
    match stdout_event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stdout);
            assert_eq!(text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        }
        _ => panic!("expected stdout output event"),
    }
    assert!(
        !forward_state.output_truncated,
        "stdout at exactly the per-stream budget must not trip truncation"
    );

    let stderr_frame = format!(
        "event: output\ndata: {}\n",
        json!({ "stream": "stderr", "text": "tiny stderr payload" })
    );
    assert!(
        handle_remote_terminal_sse_frame(&stderr_frame, &tx, &mut forward_state)
            .unwrap()
            .is_none()
    );
    let stderr_event = rx
        .try_recv()
        .expect("stderr event must be forwarded even after stdout has consumed its own budget");
    match stderr_event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            assert_eq!(stream, TerminalOutputStream::Stderr);
            assert_eq!(text, "tiny stderr payload");
        }
        _ => panic!("expected stderr output event"),
    }
    assert!(
        !forward_state.output_truncated,
        "stderr within its per-stream budget must not trip truncation after a full stdout"
    );

    let complete_frame = format!(
        "event: complete\ndata: {}\n",
        serde_json::to_string(&TerminalCommandResponse {
            command: "remote".to_owned(),
            duration_ms: 1,
            exit_code: Some(0),
            output_truncated: false,
            shell: "sh".to_owned(),
            stderr: "tiny stderr payload".to_owned(),
            stdout: stdout_fill,
            success: true,
            timed_out: false,
            workdir: "/repo".to_owned(),
        })
        .unwrap()
    );
    let response = handle_remote_terminal_sse_frame(&complete_frame, &tx, &mut forward_state)
        .unwrap()
        .expect("complete event should finish remote stream");
    assert!(
        !response.output_truncated,
        "completion response must not be marked truncated when both streams fit \
         their per-stream budgets"
    );
}

// Pins that forward_remote_terminal_stream_reader accepts a completion
// frame whose JSON encoding balloons well past
// 2 * TERMINAL_OUTPUT_MAX_BYTES (newline-heavy stdout produces \n
// escapes that roughly double the byte count) but stays within
// TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES.
// Guards against the pending-frame cap becoming so tight that real,
// legal completion payloads are rejected.
#[test]
fn forward_remote_terminal_stream_reader_accepts_max_sized_completion_frame() {
    // Regression for the rejected-valid-completion-frame bug. A remote that
    // legitimately returns `TERMINAL_OUTPUT_MAX_BYTES` of newline-heavy
    // stdout serializes the completion frame to ~1 MiB + envelope. JSON
    // expands each `\n` byte to the two-char escape `\\n`, so the payload
    // alone weighs roughly `2 * TERMINAL_OUTPUT_MAX_BYTES` — already above
    // the pre-fix proxy cap of `TERMINAL_OUTPUT_MAX_BYTES * 2 = 1 MiB`.
    // Before the cap was raised the forwarder returned
    // `remote terminal stream frame exceeded the allowed size` instead of
    // delivering the command result. The new cap
    // (`TERMINAL_OUTPUT_MAX_BYTES * 16`) must let this frame through while
    // still enforcing a bounded upper limit.
    let (tx, mut rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));

    let stdout_fill = "\n".repeat(TERMINAL_OUTPUT_MAX_BYTES);
    let response = TerminalCommandResponse {
        command: "cat big.log".to_owned(),
        duration_ms: 42,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: stdout_fill.clone(),
        success: true,
        timed_out: false,
        workdir: "/repo".to_owned(),
    };
    let payload = serde_json::to_string(&response).expect("response serializes");
    let frame = format!("event: complete\ndata: {payload}\n\n");

    // Regression: the frame must overflow the pre-fix cap (double the raw
    // output limit) but still fit within the new cap (16× the raw limit).
    let old_cap = TERMINAL_OUTPUT_MAX_BYTES * 2;
    assert!(
        frame.len() > old_cap,
        "expected newline-heavy completion frame to exceed the pre-fix cap \
         (frame = {} bytes, old cap = {old_cap} bytes)",
        frame.len()
    );
    assert!(
        frame.len() <= TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        "expected newline-heavy completion frame to fit within the post-fix cap \
         (frame = {} bytes, new cap = {} bytes)",
        frame.len(),
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES
    );

    let mut reader = std::io::Cursor::new(frame.into_bytes());
    let forwarded = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .expect("max-sized completion frame should be accepted");

    assert_eq!(forwarded.command, "cat big.log");
    // The proxy's post-decode `cap_terminal_response_output` leaves stdout
    // at its incoming size when it is already at the budget, so the full
    // 512 KiB of newlines round-trip through the forwarder unchanged.
    assert_eq!(forwarded.stdout.len(), TERMINAL_OUTPUT_MAX_BYTES);
    assert_eq!(forwarded.stdout, stdout_fill);
    assert!(forwarded.stderr.is_empty());
    // No intermediate output events were emitted — the frame is a single
    // completion event, so the channel stays empty.
    assert!(rx.try_recv().is_err());
}

// Pins that InterruptibleRemoteStreamReader::read correctly drains a
// single buffered chunk across multiple small destination buffers
// without losing or duplicating bytes (offset tracking + buffered
// clear).
// Guards against future callers with smaller scratch buffers silently
// dropping/repeating bytes through the partial-drain path.
#[test]
fn interruptible_remote_stream_reader_drains_partial_chunks_across_small_reads() {
    // Regression guard for `read_buffered`'s partial-chunk drain path.
    // Production callers in the SSE forwarder read with an 8 KiB scratch
    // that always equals or exceeds the `scratch` size used by
    // `read_remote_stream_response`, so a buffered chunk is always consumed
    // in one `Read::read` call. A future caller (or a test-time change to
    // the worker's scratch size) that uses a smaller destination buffer
    // would exercise the partial-drain path for the first time, and a
    // regression in `self.offset` tracking or `self.buffered.clear()`
    // would drop or duplicate bytes silently. Construct the reader
    // directly via `::new`, pre-load a single 10-byte chunk, and assert
    // that repeated 4-byte reads drain exactly the original chunk in the
    // correct order.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    chunk_tx
        .send(Ok(b"abcdefghij".to_vec()))
        .expect("chunk send must succeed");
    drop(chunk_tx);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new(chunk_rx, cancellation);

    let mut buf = [0u8; 4];
    let mut collected = Vec::new();
    loop {
        match std::io::Read::read(&mut reader, &mut buf).expect("partial read must succeed") {
            0 => break,
            n => collected.extend_from_slice(&buf[..n]),
        }
    }
    assert_eq!(collected, b"abcdefghij");
}

// Pins the consumer-side half of the remote stream authority fence. The HTTP
// producer may validate and enqueue a chunk while endpoint A is current, then
// settings can replace A before the forwarding thread consumes that queue.
// Buffered bytes from A must be discarded instead of crossing the replacement.
#[tokio::test]
async fn interruptible_remote_stream_reader_rejects_queued_bytes_after_endpoint_replacement() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "remote-terminal-queued-authority".to_owned(),
        name: "Queued Authority Remote".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let mut replacement = remote.clone();
    replacement.host = Some("new.example.com".to_owned());
    replacement.port = Some(2222);
    replacement.user = Some("bob".to_owned());
    create_test_remote_project(
        &state,
        &remote,
        "/remote/terminal-queued-authority",
        "Queued Authority Project",
        "remote-project-terminal-queued-authority",
    );
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("the original route should be current");
    let authority = RemoteStreamingAuthority::new(
        lease,
        state.remote_registry.configs.clone(),
        state.remote_registry.config_generation.clone(),
    );
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    chunk_tx
        .send(Ok(b"bytes from retired endpoint".to_vec()))
        .expect("queued chunk should be accepted");
    drop(chunk_tx);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new_with_authority(
        chunk_rx,
        cancellation.clone(),
        authority,
    );

    state
        .update_app_settings(UpdateAppSettingsRequest {
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
            remotes: Some(vec![RemoteConfig::local(), replacement]),
        })
        .expect("endpoint replacement should publish");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let error = match forward_remote_terminal_stream_reader(&mut reader, &event_tx, &cancellation) {
        Ok(_) => panic!("queued bytes from the retired route must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);

    send_terminal_stream_result(&event_tx, Err(error)).await;
    match event_rx
        .recv()
        .await
        .expect("terminal error event should emit")
    {
        TerminalCommandStreamEvent::Error { error, status } => {
            assert_eq!(status, StatusCode::CONFLICT.as_u16());
            assert_eq!(error, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
        }
        _ => panic!("expected terminal stream error event"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn terminal_stream_error_event_preserves_unknown_remote_status_after_removal() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "remote-terminal-queued-removal".to_owned(),
        name: "Queued Removal Remote".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/terminal-queued-removal",
        "Queued Removal Project",
        "remote-project-terminal-queued-removal",
    );
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("the original route should be current");
    let authority = RemoteStreamingAuthority::new(
        lease,
        state.remote_registry.configs.clone(),
        state.remote_registry.config_generation.clone(),
    );
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    chunk_tx
        .send(Ok(b"bytes from removed endpoint".to_vec()))
        .expect("queued chunk should be accepted");
    drop(chunk_tx);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new_with_authority(
        chunk_rx,
        cancellation.clone(),
        authority,
    );

    state
        .delete_project(&project_id)
        .expect("test project deletion should detach the remote");
    state
        .update_app_settings(UpdateAppSettingsRequest {
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
            remotes: Some(vec![RemoteConfig::local()]),
        })
        .expect("remote removal should publish");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let error = match forward_remote_terminal_stream_reader(&mut reader, &event_tx, &cancellation) {
        Ok(_) => panic!("queued bytes from the removed route must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, format!("unknown remote `{}`", remote.id));

    send_terminal_stream_result(&event_tx, Err(error)).await;
    match event_rx
        .recv()
        .await
        .expect("terminal error event should emit")
    {
        TerminalCommandStreamEvent::Error { error, status } => {
            assert_eq!(status, StatusCode::BAD_REQUEST.as_u16());
            assert_eq!(error, format!("unknown remote `{}`", remote.id));
        }
        _ => panic!("expected terminal stream error event"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the free-queue dispatch boundary. Replacement after the optimistic
// authority check but before enqueue must be observed by the locked dispatch
// guard, so no old-endpoint event can enter the browser-facing channel after
// settings publication wins the race.
#[test]
fn remote_terminal_free_queue_enqueue_is_atomic_with_route_publication() {
    let state = test_app_state();
    let remote = RemoteConfig {
        id: "remote-terminal-free-queue-authority".to_owned(),
        name: "Free Queue Authority Remote".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let mut replacement = remote.clone();
    replacement.host = Some("new.example.com".to_owned());
    replacement.port = Some(2222);
    create_test_remote_project(
        &state,
        &remote,
        "/remote/terminal-free-queue-authority",
        "Free Queue Authority Project",
        "remote-project-terminal-free-queue-authority",
    );
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("the original route should be current");
    let authority = RemoteStreamingAuthority::new(
        lease,
        state.remote_registry.configs.clone(),
        state.remote_registry.config_generation.clone(),
    );
    let state_for_hook = state.clone();
    authority.set_test_before_terminal_event_enqueue(move || {
        replace_remote_settings_for_terminal_authority_test(&state_for_hook, replacement);
    });
    let cancellation = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);

    let error = send_remote_terminal_stream_event(
        &event_tx,
        TerminalCommandStreamEvent::Output {
            stream: TerminalOutputStream::Stdout,
            text: "stale".to_owned(),
        },
        Some(&cancellation),
        Some(&authority),
    )
    .expect_err("replacement before enqueue must reject the retired event");

    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
    assert!(event_rx.try_recv().is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins the final terminal forwarding boundary. A complete remote SSE chunk can
// already be decoded into the outer pending buffer while the browser-facing
// channel is full. Replacing the endpoint during that backpressure must abort
// with the route conflict instead of forwarding the retired endpoint's queued
// output or accepting its completion frame.
#[tokio::test]
async fn remote_terminal_frame_dispatch_rechecks_authority_during_backpressure() {
    struct OneChunkReader {
        bytes: Option<Vec<u8>>,
        read_returned: Arc<AtomicBool>,
    }

    impl std::io::Read for OneChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(bytes) = self.bytes.take() else {
                return Ok(0);
            };
            assert!(bytes.len() <= buf.len());
            buf[..bytes.len()].copy_from_slice(&bytes);
            self.read_returned.store(true, Ordering::SeqCst);
            Ok(bytes.len())
        }
    }

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "remote-terminal-frame-dispatch-authority".to_owned(),
        name: "Frame Dispatch Authority Remote".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let mut replacement = remote.clone();
    replacement.host = Some("new.example.com".to_owned());
    replacement.port = Some(2222);
    create_test_remote_project(
        &state,
        &remote,
        "/remote/terminal-frame-dispatch-authority",
        "Frame Dispatch Authority Project",
        "remote-project-terminal-frame-dispatch-authority",
    );
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("the original route should be current");
    let authority = RemoteStreamingAuthority::new(
        lease,
        state.remote_registry.configs.clone(),
        state.remote_registry.config_generation.clone(),
    );
    let response = TerminalCommandResponse {
        command: "printf stale".to_owned(),
        duration_ms: 1,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "stale".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote".to_owned(),
    };
    let bytes = format!(
        "event: output\ndata: {}\n\nevent: complete\ndata: {}\n\n",
        json!({ "stream": "stdout", "text": "stale" }),
        serde_json::to_string(&response).expect("completion response should encode")
    )
    .into_bytes();
    let read_returned = Arc::new(AtomicBool::new(false));
    let mut reader = OneChunkReader {
        bytes: Some(bytes),
        read_returned: read_returned.clone(),
    };
    let cancellation = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
    assert!(
        event_tx
            .try_send(TerminalCommandStreamEvent::Error {
                error: "sentinel".to_owned(),
                status: StatusCode::IM_A_TEAPOT.as_u16(),
            })
            .is_ok()
    );
    let cancellation_for_forwarder = cancellation.clone();
    let authority_for_forwarder = authority.clone();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let forwarder = std::thread::spawn(move || {
        let result = forward_remote_terminal_stream_reader_with_authority(
            &mut reader,
            &event_tx,
            &cancellation_for_forwarder,
            &authority_for_forwarder,
        );
        let _ = result_tx.send(result);
    });

    let read_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !read_returned.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < read_deadline,
            "the forwarder should read the combined remote frame"
        );
        std::thread::yield_now();
    }
    replace_remote_settings_for_terminal_authority_test(&state, replacement);

    let error = match result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("route retirement should interrupt terminal backpressure")
    {
        Ok(_) => panic!("retired buffered frames must not complete"),
        Err(error) => error,
    };
    forwarder.join().expect("forwarder should exit cleanly");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);
    match event_rx
        .recv()
        .await
        .expect("sentinel should remain queued")
    {
        TerminalCommandStreamEvent::Error { error, status } => {
            assert_eq!(error, "sentinel");
            assert_eq!(status, StatusCode::IM_A_TEAPOT.as_u16());
        }
        _ => panic!("only the prefilled sentinel should be present"),
    }
    assert!(event_rx.try_recv().is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that an idle HTTP producer cannot hide endpoint retirement. The worker
// stays blocked in source.read() while the consumer wakes on recv_timeout and
// independently rejects the retired streaming authority.
#[test]
fn interruptible_remote_stream_reader_rejects_retirement_while_producer_is_idle() {
    struct BlockingSource {
        read_called: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl std::io::Read for BlockingSource {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.read_called.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            Ok(0)
        }
    }

    let state = test_app_state();
    let remote = RemoteConfig {
        id: "remote-terminal-idle-authority".to_owned(),
        name: "Idle Authority Remote".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("old.example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let mut replacement = remote.clone();
    replacement.host = Some("new.example.com".to_owned());
    replacement.port = Some(2222);
    create_test_remote_project(
        &state,
        &remote,
        "/remote/terminal-idle-authority",
        "Idle Authority Project",
        "remote-project-terminal-idle-authority",
    );
    let lease = state
        .remote_registry
        .connection(&remote)
        .expect("the original route should be current");
    let authority = RemoteStreamingAuthority::new(
        lease,
        state.remote_registry.configs.clone(),
        state.remote_registry.config_generation.clone(),
    );
    let read_called = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let source = BlockingSource {
        read_called: read_called.clone(),
        release: release.clone(),
    };
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::spawn_with_authority(
        source,
        cancellation.clone(),
        authority,
    );
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation_for_forwarder = cancellation.clone();
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let forwarder = std::thread::spawn(move || {
        let result = forward_remote_terminal_stream_reader(
            &mut reader,
            &event_tx,
            &cancellation_for_forwarder,
        );
        let _ = result_tx.send(result);
    });

    let read_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !read_called.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < read_deadline,
            "the producer should enter its blocking body read"
        );
        std::thread::yield_now();
    }
    replace_remote_settings_for_terminal_authority_test(&state, replacement);

    let result = result_rx.recv_timeout(Duration::from_secs(5));
    release.store(true, Ordering::SeqCst);
    cancellation.store(true, Ordering::SeqCst);
    let _ = forwarder.join();
    let error =
        match result.expect("idle retirement should wake the consumer through authority polling") {
            Ok(_) => panic!("the retired route must be rejected"),
            Err(error) => error,
        };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, REMOTE_CONNECTION_CHANGED_BEFORE_REQUEST);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins that when the chunk channel is idle and cancellation flips,
// forward_remote_terminal_stream_reader returns BAD_GATEWAY with
// "terminal stream client disconnected" from the adapter-level
// recv_timeout poll.
// Guards against a cancellation signal being swallowed because the
// recv_timeout loop is not periodically re-checking it.
#[test]
fn interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts() {
    // Adapter-level contract test: when the internal chunk channel is idle,
    // the reader's `recv_timeout` loop periodically wakes and re-checks the
    // cancellation flag. This is in isolation from the spawned worker —
    // the test wires up `::new` directly with an empty channel so no
    // `read_remote_stream_response` runs at all. It therefore only covers
    // the adapter's recv-timeout poll, NOT the production path where a
    // stalled remote parks a spawned worker inside `source.read()`. The
    // `interruptible_remote_stream_reader_spawn_unblocks_on_cancellation`
    // test below exercises the worker-thread side.
    // Keep the sender alive until forwarding returns. Dropping it from the
    // cancellation thread would race the cancellation poll against channel
    // EOF, allowing the test to observe the unrelated "stream ended" path.
    let (_chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(1);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = InterruptibleRemoteStreamReader::new(chunk_rx, cancellation.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation_for_thread = cancellation.clone();
    let cancel_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        cancellation_for_thread.store(true, Ordering::SeqCst);
    });

    let err = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .err()
        .expect("idle recv_timeout must observe cancellation");

    cancel_thread.join().unwrap();
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(err.message, "terminal stream client disconnected");
}

// Pins the production spawn path: when the worker thread is parked
// inside a blocking source read and cancellation flips,
// forward_remote_terminal_stream_reader still returns the disconnect
// error, and the worker thread is confirmed to have entered the real
// read_remote_stream_response path.
// Guards against a regression where the spawn adapter silently swallows
// cancellation while the worker sits inside reqwest's body read.
#[test]
fn interruptible_remote_stream_reader_spawn_unblocks_on_cancellation() {
    // Regression covering the real production spawn path: a stalled remote
    // that never emits bytes must still let the adapter-level reader return
    // `terminal stream client disconnected` once the cancellation flag
    // flips. The mock `BlockingSource::read` mirrors a hung reqwest body
    // read by parking inside `read()` until a release flag flips, so the
    // worker thread spawned by `InterruptibleRemoteStreamReader::spawn`
    // goes through `read_remote_stream_response` exactly as it would for a
    // real stalled remote. The pre-existing
    // `interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts`
    // sibling exercises only the channel adapter via `::new`, so without
    // this test a future edit could silently break the production spawn
    // path without failing any test.
    struct BlockingSource {
        read_called: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl std::io::Read for BlockingSource {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.read_called.store(true, Ordering::SeqCst);
            loop {
                if self.release.load(Ordering::SeqCst) {
                    return Ok(0);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    let read_called = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let cancellation = Arc::new(AtomicBool::new(false));
    let source = BlockingSource {
        read_called: read_called.clone(),
        release: release.clone(),
    };

    let mut reader = InterruptibleRemoteStreamReader::spawn(source, cancellation.clone());
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);

    // Wait until the worker thread is actually parked inside `read()`.
    let start = std::time::Instant::now();
    while !read_called.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "worker thread never entered the mock BlockingSource::read"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let cancel_thread = std::thread::spawn({
        let cancellation = cancellation.clone();
        move || {
            std::thread::sleep(Duration::from_millis(25));
            cancellation.store(true, Ordering::SeqCst);
        }
    });

    let err = forward_remote_terminal_stream_reader(&mut reader, &tx, &cancellation)
        .err()
        .expect("spawn-path forwarding must surface the disconnect error");

    cancel_thread.join().unwrap();
    // Let the worker thread drain its parked read so this test doesn't
    // leave a detached thread attached to a stale socket mock.
    release.store(true, Ordering::SeqCst);

    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(err.message, "terminal stream client disconnected");
    assert!(
        read_called.load(Ordering::SeqCst),
        "the spawn path must actually drive read_remote_stream_response"
    );
}

// Pins that forward_remote_terminal_stream_reader_capped rejects an
// unterminated frame once it exceeds the configured pending cap,
// returning a BAD_GATEWAY "remote terminal stream frame exceeded the
// allowed size" error rather than buffering forever.
// Guards against unbounded memory growth from a hostile or broken
// remote that never emits a frame delimiter.
#[test]
fn forward_remote_terminal_stream_reader_rejects_frame_past_new_cap() {
    // The cap still bounds memory: if a malicious or buggy remote sends an
    // unterminated SSE frame larger than the new cap, the forwarder must
    // surface the existing "exceeded the allowed size" error rather than
    // buffer indefinitely. Exercise the rejection path with a small
    // test-only cap via `forward_remote_terminal_stream_reader_capped` so
    // we do not have to push megabytes of bytes through the reader in debug
    // mode (the SSE delimiter scan is O(n²) on the growing pending buffer
    // because it rescans from zero after every 8 KiB read).
    const TEST_PENDING_CAP: usize = 64 * 1024;
    let (tx, _rx) = tokio::sync::mpsc::channel(TERMINAL_STREAM_EVENT_QUEUE_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));
    let mut reader = std::io::Cursor::new(vec![b'x'; TEST_PENDING_CAP + 1]);

    let err = forward_remote_terminal_stream_reader_capped(
        &mut reader,
        &tx,
        &cancellation,
        TEST_PENDING_CAP,
    )
    .err()
    .expect("frame exceeding the cap should be rejected");
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        err.message, "remote terminal stream frame exceeded the allowed size",
        "unexpected error for oversized frame: {}",
        err.message
    );
}

// Pins that TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES is at least
// 2 * TERMINAL_OUTPUT_MAX_BYTES + 4 KiB, i.e. large enough to fit a
// worst-case newline-heavy completion frame plus SSE envelope.
// Guards against accidentally shrinking the cap below what the
// acceptance test above needs, which would reintroduce valid-frame
// rejection.
#[test]
fn forward_remote_terminal_stream_reader_uses_production_cap_at_least_as_large_as_max_frame() {
    // Cheap sanity check pinning the constant so a future edit cannot
    // accidentally shrink the cap below the worst-case newline-heavy
    // completion frame that the acceptance test above exercises.
    let minimum_required = 2 * TERMINAL_OUTPUT_MAX_BYTES + 4 * 1024;
    assert!(
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES >= minimum_required,
        "pending cap regressed: {} < {}",
        TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES,
        minimum_required
    );
}

// Pins that annotate_remote_terminal_429 prefixes "remote <name>: " to
// 429 messages (so the UI attributes throttling to the remote) but
// leaves other statuses like INTERNAL_SERVER_ERROR untouched.
// Guards against mis-annotating non-throttling errors or losing the
// annotation on legitimate throttling.
#[test]
fn annotate_remote_terminal_429_prefixes_only_throttled_remote_errors() {
    let throttled = annotate_remote_terminal_429(
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "too many local terminal commands are already running; limit is 4",
        ),
        "SSH Terminal Limit",
    );
    assert_eq!(throttled.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        throttled.message,
        "remote SSH Terminal Limit: too many local terminal commands are already running; limit is 4"
    );

    let server_error = annotate_remote_terminal_429(
        ApiError::from_status(StatusCode::INTERNAL_SERVER_ERROR, "remote server exploded"),
        "SSH Terminal Limit",
    );
    assert_eq!(server_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(server_error.message, "remote server exploded");
}

// Pins that /api/terminal/run forwards a command with maximum-length
// multibyte (combining-character-safe) content to the remote's
// /api/terminal/run and returns the remote's response, i.e. the
// proxy's character-length validation does not count bytes.
// Guards against rejecting legitimate unicode commands at the proxy
// layer due to byte-vs-char confusion.
#[tokio::test]
async fn terminal_run_route_proxies_valid_remote_multibyte_commands() {
    let captured_body = Arc::new(Mutex::new(None::<String>));
    let captured_for_server = captured_body.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
    let port = listener.local_addr().expect("listener addr").port();
    let command = format!("#{}", "\u{00e9}".repeat(TERMINAL_COMMAND_MAX_CHARS - 1));
    let remote_response = serde_json::to_string(&TerminalCommandResponse {
        command: command.clone(),
        duration_ms: 12,
        exit_code: Some(0),
        output_truncated: false,
        shell: "sh".to_owned(),
        stderr: String::new(),
        stdout: "ok\n".to_owned(),
        success: true,
        timed_out: false,
        workdir: "/remote/repo".to_owned(),
    })
    .expect("terminal response should encode");
    let server = std::thread::spawn(move || {
        // Loop until the terminal run request is captured rather than
        // hard-coding the number of proxy round-trips. A future change that
        // adds a capability probe, a binding step, or a retry would
        // otherwise produce a confusing dual failure (server thread
        // panicking on the accept deadline AND the proxy's next request
        // hitting a closed listener). This loop tolerates any number of
        // pre-run requests and terminates as soon as the terminal/run
        // request has been served.
        loop {
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
            let request_line = headers
                .lines()
                .next()
                .expect("request line should exist")
                .to_owned();
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

            if request_line.starts_with("POST /api/terminal/run ") {
                *captured_for_server.lock().expect("capture mutex poisoned") = Some(body);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                            remote_response.len(),
                            remote_response
                        )
                        .as_bytes(),
                    )
                    .expect("terminal response should write");
                break;
            }

            panic!("unexpected request: {request_line}");
        }
    });

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
    let project_id = create_test_remote_project(
        &state,
        &remote,
        "/remote/repo",
        "Remote Terminal",
        "remote-project-1",
    );
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
    let app = app_router(state);

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/terminal/run")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "command": command,
                    "projectId": project_id,
                    "workdir": " /remote/repo ",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        response["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );
    let captured: Value = serde_json::from_str(
        captured_body
            .lock()
            .expect("capture mutex poisoned")
            .as_ref()
            .expect("remote request should be captured"),
    )
    .expect("remote request body should decode");
    assert_eq!(
        captured["workdir"],
        Value::String("/remote/repo".to_owned())
    );
    assert_eq!(
        captured["projectId"],
        Value::String("remote-project-1".to_owned())
    );
    assert_eq!(
        captured["command"].as_str().unwrap().chars().count(),
        TERMINAL_COMMAND_MAX_CHARS
    );

    join_test_server(server);
}
