// ACP and Codex JSON-RPC request lifecycle tests — pending-request map
// waiter semantics (timeout-free vs timeout, error preservation), writer
// loop responsiveness under pending responses, and `fail_pending_*`
// behavior when the agent process exits mid-request.
//
// Extracted from tests.rs — cohesive cluster (previously lines 1154-1516)
// exercising AcpPendingRequestMap, CodexPendingRequestMap, and their
// timeout + exit-signal waiters.

use super::*;

// Tests that ACP JSON RPC request without timeout waits for late response.
#[test]
fn acp_json_rpc_request_without_timeout_waits_for_late_response() {
    let pending_requests: AcpPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_acp_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "session/prompt",
            json!({
                "sessionId": "cursor-session-1",
                "prompt": [],
            }),
            AcpAgent::Cursor,
        )
        .expect("prompt request should resolve once a response arrives");
        result_tx
            .send((
                String::from_utf8(writer).expect("request payload should be UTF-8"),
                result,
            ))
            .unwrap();
    });

    let (request_id, sender) = take_pending_acp_request(&pending_requests, Duration::from_secs(1));

    sender.send(Ok(json!({ "ok": true }))).unwrap();

    let (written, result) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late ACP response should unblock the prompt request");
    assert!(written.contains("\"method\":\"session/prompt\""));
    assert!(written.contains(&format!("\"id\":\"{request_id}\"")));
    assert_eq!(result, json!({ "ok": true }));
    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that Codex JSON RPC request without timeout waits for late response.
#[test]
fn codex_json_rpc_request_without_timeout_waits_for_late_response() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_codex_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "turn/start",
            json!({
                "threadId": "thread-1",
            }),
        )
        .expect("Codex request should resolve once a response arrives");
        result_tx
            .send((
                String::from_utf8(writer).expect("request payload should be UTF-8"),
                result,
            ))
            .unwrap();
    });

    let (request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    sender.send(Ok(json!({ "ok": true }))).unwrap();

    let (written, result) = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("late Codex response should unblock the request");
    assert!(written.contains("\"method\":\"turn/start\""));
    assert!(written.contains(&format!("\"id\":\"{request_id}\"")));
    assert_eq!(result, json!({ "ok": true }));
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that Codex JSON RPC request preserves JSON-RPC errors.
#[test]
fn codex_json_rpc_request_without_timeout_preserves_json_rpc_errors() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (result_tx, result_rx) = mpsc::channel();
    let request_pending_requests = pending_requests.clone();

    std::thread::spawn(move || {
        let mut writer = Vec::new();
        let result = send_codex_json_rpc_request_without_timeout(
            &mut writer,
            &request_pending_requests,
            "turn/start",
            json!({
                "threadId": "thread-1",
            }),
        );
        result_tx.send(result).unwrap();
    });

    let (request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));

    assert!(!request_id.is_empty());
    sender
        .send(Err(CodexResponseError::JsonRpc(
            "thread/start rejected the request".to_owned(),
        )))
        .unwrap();

    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CodexResponseError::JsonRpc(
            "thread/start rejected the request".to_owned(),
        ))
    );
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that waiting for a Codex JSON-RPC response times out and clears the pending request.
#[test]
fn codex_json_rpc_response_wait_timeout_clears_pending_request() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    let pending_request = start_codex_json_rpc_request(
        &mut writer,
        &pending_requests,
        "turn/start",
        json!({
            "threadId": "thread-1",
        }),
    )
    .expect("Codex request should be queued");

    let result = wait_for_codex_json_rpc_response(
        &pending_requests,
        pending_request,
        "turn/start",
        Some(Duration::from_millis(10)),
    );

    assert!(matches!(
        result,
        Err(CodexResponseError::Timeout(detail))
            if detail.contains("timed out waiting for Codex app-server response to `turn/start`")
    ));
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
}

// Tests that ACP prompt command keeps writer loop responsive while waiting for response.
#[test]
fn acp_prompt_command_keeps_writer_loop_responsive_while_waiting_for_response() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Cursor),
            name: Some("Cursor Prompt Loop".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("auto".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: Some(CursorMode::Ask),
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState {
        current_session_id: Some("cursor-session-1".to_owned()),
        is_loading_history: false,
        supports_session_load: Some(true),
    }));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_runtime_state = runtime_state.clone();
    let thread_state = state.clone();
    let thread_session_id = created.session_id.clone();
    let runtime_token = RuntimeToken::Acp("cursor-runtime-1".to_owned());
    let (input_tx, input_rx) = mpsc::channel();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                AcpRuntimeCommand::Prompt(prompt) => handle_acp_prompt_command(
                    &mut stdin,
                    &thread_pending_requests,
                    &thread_state,
                    &thread_session_id,
                    &thread_runtime_state,
                    &runtime_token,
                    AcpAgent::Cursor,
                    prompt,
                )
                .unwrap(),
                AcpRuntimeCommand::JsonRpcMessage(message) => {
                    write_acp_json_rpc_message(&mut stdin, &message, AcpAgent::Cursor).unwrap();
                }
                AcpRuntimeCommand::RefreshSessionConfig { .. } => {
                    panic!("unexpected config refresh in prompt loop test");
                }
            }
        }
    });

    input_tx
        .send(AcpRuntimeCommand::Prompt(AcpPromptCommand {
            cwd: "/tmp".to_owned(),
            cursor_mode: Some(CursorMode::Ask),
            model: "auto".to_owned(),
            prompt: "review-local".to_owned(),
            resume_session_id: Some("cursor-session-1".to_owned()),
        }))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .len()
            == 1
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prompt request should stay pending while waiting for a response"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(AcpRuntimeCommand::JsonRpcMessage(
            json_rpc_result_response_message(
                "approval-1".to_owned(),
                json!({
                    "outcome": {
                        "outcome": "selected",
                        "optionId": "allow-once",
                    }
                }),
            ),
        ))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"method\":\"session/prompt\"")
            && written.contains("\"id\":\"approval-1\"")
            && written.contains("\"jsonrpc\":\"2.0\"")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should remain able to write approval responses while prompt is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let sender = {
        let mut locked = pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned");
        let request_id = locked
            .keys()
            .next()
            .cloned()
            .expect("prompt request id should exist");
        locked
            .remove(&request_id)
            .expect("prompt request sender should still be pending")
    };
    sender.send(Ok(json!({ "ok": true }))).unwrap();

    drop(input_tx);
    writer_thread.join().unwrap();
}

// Tests that fail pending ACP requests releases waiters.
#[test]
fn fail_pending_acp_requests_releases_waiters() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<std::result::Result<Value, AcpResponseError>>();

    pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .insert("req-1".to_owned(), tx);

    fail_pending_acp_requests(&pending_requests, "Cursor ACP runtime exited.");

    assert!(
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .is_empty()
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(AcpResponseError::Transport(
            "Cursor ACP runtime exited.".to_owned()
        ))
    );
}

// Tests that fail pending Codex requests releases waiters.
#[test]
fn fail_pending_codex_requests_releases_waiters() {
    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<std::result::Result<Value, CodexResponseError>>();

    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert("req-1".to_owned(), tx);

    fail_pending_codex_requests(
        &pending_requests,
        "shared Codex app-server exited while waiting for a pending response",
    );

    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CodexResponseError::Transport(
            "shared Codex app-server exited while waiting for a pending response".to_owned()
        ))
    );
}
