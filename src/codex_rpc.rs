// Codex JSON-RPC 2.0 plumbing — the low-level request/response machinery
// that sits under the Codex app-server event handlers.
//
// Covers the request dispatch (monotonic id assignment, Sender-backed
// reply channels, timeout + untimed variants), the response waiter that
// inspects the pending-request map, model-list pagination that fires one
// page at a time, undeliverable-server-request rejection (for when the
// runtime writer queue drops a message), mass fail-pending-requests on
// agent exit, and the shared-Codex runtime command error-detail
// summarizer used when a runtime command channel send fails.
//
// Extracted from codex.rs so the event-handler block can stay separate
// from the transport layer.

/// Handles send Codex JSON RPC request.
fn send_codex_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Duration,
) -> std::result::Result<Value, CodexResponseError> {
    send_codex_json_rpc_request_inner(writer, pending_requests, method, params, Some(timeout))
}

/// Sends a Codex JSON-RPC request without a local timeout.
#[cfg(test)]
fn send_codex_json_rpc_request_without_timeout(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
) -> std::result::Result<Value, CodexResponseError> {
    send_codex_json_rpc_request_inner(writer, pending_requests, method, params, None)
}

/// Starts a Codex JSON-RPC request without waiting for the response.
fn start_codex_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
) -> std::result::Result<PendingCodexJsonRpcRequest, CodexResponseError> {
    start_codex_json_rpc_request_with_id(
        writer,
        pending_requests,
        Uuid::new_v4().to_string(),
        method,
        params,
    )
}

/// Starts a Codex JSON-RPC request with a preallocated request id.
fn start_codex_json_rpc_request_with_id(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    request_id: String,
    method: &str,
    params: Value,
) -> std::result::Result<PendingCodexJsonRpcRequest, CodexResponseError> {
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert(request_id.clone(), tx);

    if let Err(err) = write_codex_json_rpc_message(
        writer,
        &json_rpc_request_message(request_id.clone(), method, params),
    ) {
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .remove(&request_id);
        return Err(CodexResponseError::Transport(format!("{err:#}")));
    }

    Ok(PendingCodexJsonRpcRequest {
        request_id,
        response_rx: rx,
    })
}

/// Waits for a pending Codex JSON-RPC response.
fn wait_for_codex_json_rpc_response(
    pending_requests: &CodexPendingRequestMap,
    pending_request: PendingCodexJsonRpcRequest,
    method: &str,
    timeout: Option<Duration>,
) -> std::result::Result<Value, CodexResponseError> {
    let PendingCodexJsonRpcRequest {
        request_id,
        response_rx,
    } = pending_request;

    match timeout {
        Some(timeout) => match response_rx.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                pending_requests
                    .lock()
                    .expect("Codex pending requests mutex poisoned")
                    .remove(&request_id);
                Err(CodexResponseError::Timeout(format!(
                    "timed out waiting for Codex app-server response to `{method}`"
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                pending_requests
                    .lock()
                    .expect("Codex pending requests mutex poisoned")
                    .remove(&request_id);
                Err(CodexResponseError::Transport(format!(
                    "Codex app-server response channel closed while waiting for `{method}`"
                )))
            }
        },
        None => match response_rx.recv() {
            Ok(response) => response,
            Err(err) => {
                pending_requests
                    .lock()
                    .expect("Codex pending requests mutex poisoned")
                    .remove(&request_id);
                Err(CodexResponseError::Transport(format!(
                    "failed waiting for Codex app-server response to `{method}`: {err}"
                )))
            }
        },
    }
}

/// Handles send Codex JSON RPC request inner.
fn send_codex_json_rpc_request_inner(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Option<Duration>,
) -> std::result::Result<Value, CodexResponseError> {
    let pending_request = start_codex_json_rpc_request(writer, pending_requests, method, params)?;
    wait_for_codex_json_rpc_response(pending_requests, pending_request, method, timeout)
}

/// Fires one `model/list` page as a fire-and-forget request and spawns a
/// waiter thread. On success, the waiter either sends the accumulated results
/// to `response_tx` (last page) or feeds a `RefreshModelListPage` command back
/// through `input_tx` to fetch the next page.
const SHARED_CODEX_MODEL_LIST_MAX_PAGES: usize = 50;

fn fire_codex_model_list_page(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    input_tx: &Sender<CodexRuntimeCommand>,
    cursor: Option<String>,
    accumulated: Vec<SessionModelOption>,
    page_count: usize,
    response_tx: Sender<std::result::Result<Vec<SessionModelOption>, String>>,
) -> Result<()> {
    let pending = start_codex_json_rpc_request(
        writer,
        pending_requests,
        "model/list",
        json!({
            "cursor": cursor,
            "includeHidden": false,
            "limit": 100,
        }),
    )
    .map_err(|err| anyhow!(err))?;

    let waiter_pending = pending_requests.clone();
    let waiter_input_tx = input_tx.clone();
    std::thread::spawn(move || {
        match wait_for_codex_json_rpc_response(
            &waiter_pending,
            pending,
            "model/list",
            Some(Duration::from_secs(30)),
        ) {
            Ok(result) => {
                let mut model_options = accumulated;
                model_options.extend(codex_model_options(&result));
                let next_cursor = result
                    .get("nextCursor")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(next_cursor) = next_cursor {
                    // More pages — send the next page request through the
                    // writer thread's command channel.
                    if page_count >= SHARED_CODEX_MODEL_LIST_MAX_PAGES {
                        let _ = response_tx.send(Err(format!(
                            "Codex model list pagination exceeded {} pages.",
                            SHARED_CODEX_MODEL_LIST_MAX_PAGES
                        )));
                        return;
                    }
                    if let Err(err) =
                        waiter_input_tx.send(CodexRuntimeCommand::RefreshModelListPage {
                            cursor: next_cursor,
                            accumulated: model_options,
                            page_count: page_count + 1,
                            response_tx,
                        })
                    {
                        let detail = err.to_string();
                        let CodexRuntimeCommand::RefreshModelListPage { response_tx, .. } = err.0
                        else {
                            unreachable!("model list pagination should only queue page commands");
                        };
                        let _ = response_tx.send(Err(format!(
                            "failed to queue next Codex model list page: {detail}"
                        )));
                    }
                } else {
                    let _ = response_tx.send(Ok(model_options));
                }
            }
            Err(CodexResponseError::JsonRpc(detail)
                | CodexResponseError::Timeout(detail)
                | CodexResponseError::Transport(detail)) => {
                let _ = response_tx.send(Err(detail));
            }
        }
    });
    Ok(())
}

/// Auto-rejects a Codex app-server request (one with an `id` field, no
/// `result`/`error`) that cannot be delivered to any session. Sends an error
/// response through the writer so the app-server does not hang waiting for an
/// answer that will never come. Notifications (no `id`) are silently ignored.
fn reject_undeliverable_codex_server_request(
    message: &Value,
    input_tx: &Sender<CodexRuntimeCommand>,
) {
    // Only reject server requests (messages with an `id` and no `result`/`error`).
    let Some(request_id) = message.get("id") else {
        return;
    };
    if message.get("result").is_some() || message.get("error").is_some() {
        return;
    }
    let _ = input_tx.send(CodexRuntimeCommand::JsonRpcResponse {
        response: CodexJsonRpcResponseCommand {
            request_id: request_id.clone(),
            payload: CodexJsonRpcResponsePayload::Error {
                code: -32001,
                message: "Session unavailable; request could not be delivered.".to_owned(),
            },
        },
    });
}

/// Marks pending Codex requests as failed.
fn fail_pending_codex_requests(pending_requests: &CodexPendingRequestMap, detail: &str) {
    let senders = pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .drain()
        .map(|(_, sender)| sender)
        .collect::<Vec<_>>();

    for sender in senders {
        let _ = sender.send(Err(CodexResponseError::Transport(detail.to_owned())));
    }
}

/// Formats a shared Codex runtime command failure.
fn shared_codex_runtime_command_error_detail(err: &anyhow::Error) -> String {
    if let Some(detail) = err
        .downcast_ref::<CodexResponseError>()
        .and_then(CodexResponseError::as_transport)
    {
        if detail.contains("shared Codex app-server") {
            return detail.to_owned();
        }
        return format!("failed to communicate with shared Codex app-server: {detail}");
    }

    format!("failed to communicate with shared Codex app-server: {err:#}")
}
