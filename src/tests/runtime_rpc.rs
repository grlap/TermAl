// acp + codex agents speak json-rpc over stdio; TermAl's runtime thread writes
// each outbound request with a monotonic id and parks a reply channel in the
// per-agent `PendingRequestMap` keyed by that id. when a response frame
// arrives on stdout, the reader looks up the id and signals the waiter via
// the stashed `Sender`, which wakes the caller blocked on `recv`.
// two wait flavors live side by side: `wait_*_without_timeout` blocks
// indefinitely and is used for operations that can legitimately take seconds
// (e.g. claude-code session init), while the timed variant arms a deadline
// and removes the pending entry on expiry so a late response cannot leak a
// dangling sender. json-rpc-level failures (method rejected, bad params) are
// distinct from transport failures: the wait helpers preserve the original
// `JsonRpc(code, message)` shape so callers can inspect it. when the agent
// process exits mid-request, `fail_pending_*_requests` drains the map and
// releases every waiter with a `Transport` error so nobody parks forever.
// the writer loop must also stay responsive to approvals + notifications
// while a prompt request is still pending. production surfaces exercised:
// `AcpPendingRequestMap`, `CodexPendingRequestMap`, `wait_acp_*_response`,
// `wait_codex_*_response`, `fail_pending_acp_requests`,
// `fail_pending_codex_requests`.

use super::*;

// pins the untimed ACP waiter: after the request id is written and parked in
// the pending map, the caller blocks until a late response lands on the
// stashed sender, then returns the raw result value and drains the entry.
// guards against regressions that would short-circuit the wait or leak the
// pending entry after the late reply is delivered.
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

// pins the codex mirror of the untimed waiter: a queued `turn/start` request
// parks in the codex pending map and only unblocks once a late ok-response
// arrives, after which the entry is removed.
// guards against codex-specific drift from the ACP contract — both agents
// must share the same "wait forever for slow replies" semantics.
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

// pins error-shape preservation: when the response channel delivers a
// `CodexResponseError::JsonRpc(message)`, the untimed waiter surfaces that
// same variant rather than flattening it into a transport or string error.
// guards against callers losing the ability to distinguish a method-level
// rejection from a dropped connection when inspecting the result.
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

// pins the timed waiter's cleanup invariant: when the 10ms deadline elapses
// with no response, the helper returns `Timeout` AND removes the pending
// entry so a subsequent late reply cannot hit a dangling sender.
// guards against a leak where an expired waiter leaves its id + channel in
// the map, growing the map unbounded across retried calls.
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

// pins writer-loop responsiveness: while a `session/prompt` request is
// parked in the pending map, the runtime writer still drains and writes
// subsequent queued messages (approval responses, notifications) rather
// than serializing everything behind the in-flight prompt.
// guards against a regression where the writer would block on the prompt's
// response and stall approvals the user needs to send to unblock it.
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

// pins the ACP exit-path cleanup: `fail_pending_acp_requests` drains every
// entry in the map and delivers `Transport(detail)` to each sender so no
// waiter parks forever after the runtime exits.
// guards against leaking waiters that would hang the UI indefinitely if
// the cursor/claude agent process dies mid-request.
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

// pins the codex mirror: `fail_pending_codex_requests` drains the shared
// app-server's pending map and releases each waiter with the transport
// detail when the codex app-server exits mid-request.
// guards against divergence from the ACP path — codex requests must also
// be rescued on process exit so the shared app-server never orphans them.
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
