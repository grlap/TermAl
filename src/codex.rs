// Codex runtime — spawning + shared-app-server lifecycle + event handling.
//
// Covers: per-session `spawn_codex_runtime` (which actually attaches each
// session to the shared app server), the shared-app-server lifecycle
// (`spawn_shared_codex_runtime`, stdin watchdog, completed-turn cleanup
// worker), the main event dispatch (`handle_shared_codex_app_server_message`,
// `handle_shared_codex_app_server_notification`, item started/completed,
// task complete, agent message delta + final, global notices, thread
// compacted, model rerouted), prompt + turn-start command handling,
// JSON-RPC request/response plumbing (send/start/wait + fail_pending +
// reject_undeliverable), delta-suffix deduplication with UTF-8 safety,
// subagent-result buffering with flush-after-final-assistant ordering,
// and stdout parsing with capped line reads + bad-JSON streak tracking.
//
// The Codex protocol types (`CodexRuntimeCommand`, `CodexPromptCommand`,
// `CodexJsonRpcResponseCommand`, `CodexPendingApproval`,
// `CodexPendingUserInput`, `CodexPendingMcpElicitation`,
// `CodexPendingAppRequest`, `CodexTurnState`, `CodexResponseError`,
// `PendingCodexJsonRpcRequest`, `SharedCodexSessionState`,
// `SharedCodexSessions`, `SharedCodexCompletedTurnCleanup`,
// `PendingSubagentResult`) stay in runtime.rs next to the ACP + Claude
// type definitions — this file owns only the implementation.
//
// Extracted from runtime.rs into its own `include!()` fragment — the
// single largest extraction of the refactor. The crate still compiles as
// one flat module, so no visibility changes are required.

/// Spawns Codex runtime.
fn spawn_codex_runtime(
    state: AppState,
    session_id: String,
    _workdir: String,
) -> Result<CodexRuntimeHandle> {
    let shared_runtime = state.shared_codex_runtime()?;
    let shared_session = SharedCodexSessionHandle {
        runtime: shared_runtime.clone(),
        session_id,
    };
    // Shared-session state is inserted lazily once Codex starts or resumes a real thread.
    // Avoiding eager registration here keeps the first-turn dispatch path from taking the
    // shared-session mutex while `state.inner` is still locked.

    Ok(CodexRuntimeHandle {
        runtime_id: shared_runtime.runtime_id.clone(),
        input_tx: shared_runtime.input_tx.clone(),
        process: shared_runtime.process.clone(),
        shared_session: Some(shared_session),
    })
}

/// Spawns shared Codex runtime.
const SHARED_CODEX_STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const SHARED_CODEX_STDIN_WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
struct SharedCodexStdinActivity {
    operation: &'static str,
    started_at: std::time::Instant,
    timed_out: bool,
}

type SharedCodexStdinActivityState = Arc<Mutex<Option<SharedCodexStdinActivity>>>;

struct SharedCodexStdinActivityGuard<'a> {
    activity: &'a SharedCodexStdinActivityState,
}

impl<'a> SharedCodexStdinActivityGuard<'a> {
    fn new(
        activity: &'a SharedCodexStdinActivityState,
        operation: &'static str,
    ) -> SharedCodexStdinActivityGuard<'a> {
        *activity
            .lock()
            .expect("shared Codex stdin activity mutex poisoned") = Some(SharedCodexStdinActivity {
            operation,
            started_at: std::time::Instant::now(),
            timed_out: false,
        });
        SharedCodexStdinActivityGuard { activity }
    }
}

impl Drop for SharedCodexStdinActivityGuard<'_> {
    fn drop(&mut self) {
        *self
            .activity
            .lock()
            .expect("shared Codex stdin activity mutex poisoned") = None;
    }
}

struct SharedCodexWatchedWriter<W> {
    inner: W,
    activity: SharedCodexStdinActivityState,
}

impl<W> SharedCodexWatchedWriter<W> {
    fn new(inner: W, activity: SharedCodexStdinActivityState) -> Self {
        SharedCodexWatchedWriter { inner, activity }
    }
}

impl<W: Write> Write for SharedCodexWatchedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _guard = SharedCodexStdinActivityGuard::new(&self.activity, "write");
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _guard = SharedCodexStdinActivityGuard::new(&self.activity, "flush");
        self.inner.flush()
    }
}

fn shared_codex_stdin_timeout_detail(
    operation: &'static str,
    timeout: Duration,
) -> String {
    // Log the internal detail to stderr; return a generic user-facing message.
    eprintln!(
        "[termal] shared Codex writer thread blocked on stdin {operation} for over {}s",
        timeout.as_secs()
    );
    "Agent communication timed out.".to_owned()
}

fn spawn_shared_codex_stdin_watchdog(
    state: &AppState,
    runtime_id: &str,
    process: Arc<SharedChild>,
    activity: &SharedCodexStdinActivityState,
    stop_rx: mpsc::Receiver<()>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<()> {
    let watchdog_state = state.clone();
    let watchdog_runtime_id = runtime_id.to_owned();
    let watchdog_process = process;
    let watchdog_activity = activity.clone();
    std::thread::Builder::new()
        .name("termal-codex-stdin-watchdog".to_owned())
        .spawn(move || loop {
            match stop_rx.recv_timeout(poll_interval) {
                Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            let timed_out_operation = {
                let mut locked = watchdog_activity
                    .lock()
                    .expect("shared Codex stdin activity mutex poisoned");
                match locked.as_mut() {
                    Some(entry)
                        if !entry.timed_out && entry.started_at.elapsed() >= timeout =>
                    {
                        entry.timed_out = true;
                        Some(entry.operation)
                    }
                    _ => None,
                }
            };

            if let Some(operation) = timed_out_operation {
                let detail = shared_codex_stdin_timeout_detail(operation, timeout);
                if let Err(err) = watchdog_state
                    .handle_shared_codex_runtime_exit(&watchdog_runtime_id, Some(&detail))
                {
                    eprintln!(
                        "[termal] shared Codex watchdog cleanup failed; killing app-server: {err:#}"
                    );
                    if let Err(kill_err) =
                        kill_child_process(&watchdog_process, "shared Codex runtime")
                    {
                        eprintln!(
                            "[termal] shared Codex watchdog fallback kill failed: {kill_err:#}"
                        );
                    }
                }
                break;
            }
        })
        .context("failed to spawn shared Codex stdin watchdog")?;
    Ok(())
}

fn spawn_shared_codex_runtime(state: AppState) -> Result<SharedCodexRuntime> {
    // Codex threads carry their own cwd, so one shared app-server can serve all sessions.
    let codex_home = prepare_termal_codex_home(&state.default_workdir, "shared-app-server")?;
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = codex_command()?;
    command
        .arg("app-server")
        .args(["--listen", "stdio://"])
        .env("CODEX_HOME", &codex_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to start shared Codex app-server")?;
    let stdin = child
        .stdin
        .take()
        .context("failed to capture shared Codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture shared Codex app-server stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture shared Codex app-server stderr")?;
    let process =
        Arc::new(SharedChild::new(child).context("failed to share shared Codex app-server child")?);
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));

    {
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_sessions = sessions.clone();
        let writer_thread_sessions = thread_sessions.clone();
        let writer_runtime_id = runtime_id.clone();
        let writer_runtime_token = RuntimeToken::Codex(runtime_id.clone());
        let writer_input_tx = input_tx.clone();
        let writer_activity: SharedCodexStdinActivityState = Arc::new(Mutex::new(None));
        let (watchdog_stop_tx, watchdog_stop_rx) = mpsc::channel();
        if let Err(err) = spawn_shared_codex_stdin_watchdog(
            &writer_state,
            &writer_runtime_id,
            process.clone(),
            &writer_activity,
            watchdog_stop_rx,
            SHARED_CODEX_STDIN_WRITE_TIMEOUT,
            SHARED_CODEX_STDIN_WATCHDOG_POLL_INTERVAL,
        ) {
            if let Err(kill_err) =
                kill_child_process(&process, "shared Codex runtime after watchdog startup failure")
            {
                eprintln!(
                    "[termal] failed to clean up shared Codex app-server after watchdog startup failure: {kill_err:#}"
                );
            }
            return Err(err);
        }
        std::thread::spawn(move || {
            let mut stdin = SharedCodexWatchedWriter::new(stdin, writer_activity);
            let initialize_result = send_codex_json_rpc_request(
                &mut stdin,
                &writer_pending_requests,
                "initialize",
                json!({
                    "clientInfo": {
                        "name": "termal",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
                Duration::from_secs(180),
            )
            .map_err(anyhow::Error::new)
            .and_then(|_| {
                write_codex_json_rpc_message(
                    &mut stdin,
                    &json_rpc_notification_message("initialized"),
                )
            });

            if let Err(err) = initialize_result {
                drop(stdin);
                let _ = watchdog_stop_tx.send(());
                let _ = writer_state.handle_shared_codex_runtime_exit(
                    &writer_runtime_id,
                    Some(&format!(
                        "failed to initialize shared Codex app-server: {err:#}"
                    )),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let command_result = match command {
                    CodexRuntimeCommand::Prompt {
                        session_id,
                        command,
                    } => handle_shared_codex_prompt_command_result(
                        &writer_state,
                        &session_id,
                        &writer_runtime_token,
                        handle_shared_codex_prompt_command(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_state,
                            &writer_runtime_id,
                            &writer_sessions,
                            &writer_thread_sessions,
                            &writer_input_tx,
                            &session_id,
                            command,
                        ),
                    ),
                    CodexRuntimeCommand::StartTurnAfterSetup {
                        session_id,
                        thread_id,
                        command,
                    } => handle_shared_codex_prompt_command_result(
                        &writer_state,
                        &session_id,
                        &writer_runtime_token,
                        handle_shared_codex_start_turn(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_state,
                            &writer_runtime_id,
                            &writer_sessions,
                            &session_id,
                            &thread_id,
                            command,
                        ),
                    ),
                    CodexRuntimeCommand::JsonRpcRequest {
                        method,
                        params,
                        timeout,
                        response_tx,
                    } => {
                        // Fire-and-forget: write the request, then spawn a
                        // waiter thread for the response. The writer thread
                        // returns immediately so other commands are not blocked.
                        match start_codex_json_rpc_request(
                            &mut stdin,
                            &writer_pending_requests,
                            &method,
                            params,
                        ) {
                            Ok(pending) => {
                                let waiter_pending = writer_pending_requests.clone();
                                let method_owned = method.clone();
                                std::thread::spawn(move || match wait_for_codex_json_rpc_response(
                                    &waiter_pending,
                                    pending,
                                    &method_owned,
                                    Some(timeout),
                                ) {
                                    Ok(result) => {
                                        let _ = response_tx.send(Ok(result));
                                    }
                                    Err(CodexResponseError::JsonRpc(detail)
                                        | CodexResponseError::Timeout(detail)
                                        | CodexResponseError::Transport(detail)) => {
                                        let _ = response_tx.send(Err(detail));
                                    }
                                });
                                Ok(())
                            }
                            Err(CodexResponseError::Transport(detail)) => Err(anyhow!(detail)),
                            Err(CodexResponseError::JsonRpc(detail)
                                | CodexResponseError::Timeout(detail)) => {
                                let _ = response_tx.send(Err(detail));
                                Ok(())
                            }
                        }
                    }
                    CodexRuntimeCommand::JsonRpcResponse { response } => {
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &codex_json_rpc_response_message(&response),
                        )
                    }
                    CodexRuntimeCommand::JsonRpcNotification { method } => {
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &json_rpc_notification_message(&method),
                        )
                    }
                    CodexRuntimeCommand::InterruptTurn {
                        response_tx,
                        thread_id,
                        turn_id,
                    } => {
                        // Fire-and-forget: write the interrupt request, then
                        // spawn a waiter thread for the ack. The writer thread
                        // returns immediately so new commands (e.g. a follow-up
                        // prompt) are not blocked behind a slow interrupt ack.
                        match start_codex_json_rpc_request(
                            &mut stdin,
                            &writer_pending_requests,
                            "turn/interrupt",
                            json!({
                                "threadId": thread_id,
                                "turnId": turn_id,
                            }),
                        ) {
                            Ok(pending) => {
                                let waiter_pending = writer_pending_requests.clone();
                                std::thread::spawn(move || match wait_for_codex_json_rpc_response(
                                    &waiter_pending,
                                    pending,
                                    "turn/interrupt",
                                    Some(Duration::from_secs(30)),
                                ) {
                                    Ok(_) => {
                                        let _ = response_tx.send(Ok(()));
                                    }
                                    Err(CodexResponseError::JsonRpc(detail)
                                        | CodexResponseError::Timeout(detail)
                                        | CodexResponseError::Transport(detail)) => {
                                        let _ = response_tx.send(Err(detail));
                                    }
                                });
                                Ok(())
                            }
                            Err(CodexResponseError::Transport(detail)) => Err(anyhow!(detail)),
                            Err(CodexResponseError::JsonRpc(detail)
                                | CodexResponseError::Timeout(detail)) => {
                                let _ = response_tx.send(Err(detail));
                                Ok(())
                            }
                        }
                    }
                    CodexRuntimeCommand::RefreshModelList { response_tx } => {
                        fire_codex_model_list_page(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_input_tx,
                            None,
                            Vec::new(),
                            1,
                            response_tx,
                        )
                    }
                    CodexRuntimeCommand::RefreshModelListPage {
                        cursor,
                        accumulated,
                        page_count,
                        response_tx,
                    } => fire_codex_model_list_page(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_input_tx,
                        Some(cursor),
                        accumulated,
                        page_count,
                        response_tx,
                    ),
                };

                if let Err(err) = command_result {
                    let _ = writer_state.handle_shared_codex_runtime_exit(
                        &writer_runtime_id,
                        Some(&shared_codex_runtime_command_error_detail(&err)),
                    );
                    break;
                }
            }

            // Close the pipe before thread teardown so the child can observe EOF
            // promptly even if later cleanup work is refactored.
            drop(stdin);
            let _ = watchdog_stop_tx.send(());
        });
    }

    {
        let reader_state = state.clone();
        let reader_pending_requests = pending_requests.clone();
        let reader_sessions = sessions.clone();
        let reader_thread_sessions = thread_sessions.clone();
        let reader_runtime_id = runtime_id.clone();
        let reader_input_tx = input_tx.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line_buf = Vec::new();
            let mut consecutive_bad_json_lines = 0usize;
            let mut runtime_failure: Option<String> = None;

            loop {
                let bytes_read = match read_capped_child_stdout_line(
                    &mut reader,
                    &mut line_buf,
                    SHARED_CODEX_STDOUT_LINE_MAX_BYTES,
                    "shared Codex app-server stdout",
                ) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        runtime_failure = Some(format!(
                            "failed to read stdout from shared Codex app-server: {err}"
                        ));
                        break;
                    }
                };

                if bytes_read == 0 {
                    break;
                }

                let raw_line = String::from_utf8_lossy(&line_buf);
                let trimmed = raw_line.trim_end();
                if trimmed.is_empty() {
                    consecutive_bad_json_lines = 0;
                    continue;
                }
                let message: Value = match serde_json::from_str(trimmed) {
                    Ok(message) => {
                        consecutive_bad_json_lines = 0;
                        message
                    }
                    Err(err) => {
                        consecutive_bad_json_lines += 1;
                        let preview = truncate_child_stdout_log_line(
                            trimmed,
                            SHARED_CODEX_STDOUT_LOG_PREVIEW_MAX_CHARS,
                        );
                        eprintln!(
                            "[termal] skipping non-JSON line from shared Codex app-server \
                             ({err}, {consecutive_bad_json_lines}/{}): {preview}",
                            SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES,
                        );
                        if let Some(detail) = shared_codex_bad_json_streak_failure_detail(
                            consecutive_bad_json_lines,
                            trimmed,
                        ) {
                            runtime_failure = Some(detail);
                            break;
                        }
                        continue;
                    }
                };

                if let Err(err) = handle_shared_codex_app_server_message(
                    &message,
                    &reader_state,
                    &reader_runtime_id,
                    &reader_pending_requests,
                    &reader_sessions,
                    &reader_thread_sessions,
                    &reader_input_tx,
                ) {
                    if shared_codex_app_server_error_is_stale_session(&err) {
                        eprintln!(
                            "[termal] non-fatal error handling shared Codex app-server event: {err:#}"
                        );
                        continue;
                    }
                    runtime_failure = Some(format!(
                        "failed to handle shared Codex app-server event: {err:#}"
                    ));
                    break;
                }
            }

            // Always fail pending requests and notify sessions, even on
            // clean EOF. Without this, sessions remain "active" in the UI
            // with no events ever arriving.
            let detail = runtime_failure
                .as_deref()
                .unwrap_or("shared Codex app-server exited");
            fail_pending_codex_requests(&reader_pending_requests, detail);
            let _ = reader_state
                .handle_shared_codex_runtime_exit(&reader_runtime_id, runtime_failure.as_deref());

            let mut sessions = reader_sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            for session_state in sessions.values_mut() {
                session_state.pending_turn_start_request_id = None;
                session_state.recorder.streaming_text_message_id = None;
                session_state.turn_id = None;
                session_state.completed_turn_id = None;
                session_state.turn_started = false;
            }
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let timestamp = runtime_stderr_timestamp();
                let prefix = format_runtime_stderr_prefix("codex", &timestamp);
                eprintln!("{prefix} {line}");
            }
        });
    }

    {
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_pending_requests = pending_requests.clone();
        let wait_runtime_id = runtime_id.clone();
        std::thread::spawn(move || match wait_process.wait() {
            Ok(status) if status.success() => {
                fail_pending_codex_requests(
                    &wait_pending_requests,
                    "shared Codex app-server exited while waiting for a pending response",
                );
                let _ = wait_state.handle_shared_codex_runtime_exit(&wait_runtime_id, None);
            }
            Ok(status) => {
                let detail = format!("shared Codex app-server exited with status {status}");
                fail_pending_codex_requests(&wait_pending_requests, &detail);
                let _ =
                    wait_state.handle_shared_codex_runtime_exit(&wait_runtime_id, Some(&detail));
            }
            Err(err) => {
                let detail = format!("failed waiting for shared Codex app-server: {err}");
                fail_pending_codex_requests(&wait_pending_requests, &detail);
                let _ =
                    wait_state.handle_shared_codex_runtime_exit(&wait_runtime_id, Some(&detail));
            }
        });
    }

    Ok(SharedCodexRuntime {
        runtime_id,
        input_tx,
        process,
        sessions,
        thread_sessions,
    })
}

/// Remembers shared Codex thread.
fn remember_shared_codex_thread(
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    session_id: &str,
    thread_id: String,
) {
    let previous_thread_id = {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions.entry(session_id.to_owned()).or_default();
        clear_shared_codex_turn_session_state(session_state);
        session_state.thread_id.replace(thread_id.clone())
    };

    let mut thread_sessions = thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    if let Some(previous_thread_id) = previous_thread_id {
        if previous_thread_id != thread_id {
            thread_sessions.remove(&previous_thread_id);
        }
    }
    thread_sessions.insert(thread_id, session_id.to_owned());
}

/// Forgets a shared Codex thread mapping that was registered provisionally.
fn forget_shared_codex_thread(
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    session_id: &str,
    thread_id: &str,
) {
    {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        if let Some(session_state) = sessions.get_mut(session_id) {
            if session_state.thread_id.as_deref() == Some(thread_id) {
                clear_shared_codex_turn_session_state(session_state);
                session_state.thread_id = None;
            }
        }
    }

    let mut thread_sessions = thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    if matches!(
        thread_sessions.get(thread_id),
        Some(mapped_session_id) if mapped_session_id == session_id
    ) {
        thread_sessions.remove(thread_id);
    }
}

/// Finds shared Codex session ID.
fn find_shared_codex_session_id(
    state: &AppState,
    thread_sessions: &SharedCodexThreadMap,
    thread_id: &str,
) -> Option<String> {
    if let Some(session_id) = thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .get(thread_id)
        .cloned()
    {
        return Some(session_id);
    }

    let session_id = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.sessions.iter().find_map(|record| {
            (record.external_session_id.as_deref() == Some(thread_id))
                .then(|| record.session.id.clone())
        })
    }?;

    thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(thread_id.to_owned(), session_id.clone());
    Some(session_id)
}

/// Handles Codex message thread ID.
fn codex_message_thread_id<'a>(message: &'a Value) -> Option<&'a str> {
    message
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/params/thread/id").and_then(Value::as_str))
}

/// Handles shared Codex session thread ID.
fn shared_codex_session_thread_id<'a>(method: &str, message: &'a Value) -> Option<&'a str> {
    codex_message_thread_id(message).or_else(|| match method {
        _ if method.starts_with("codex/event/") => message
            .pointer("/params/msg/thread_id")
            .and_then(Value::as_str)
            .or_else(|| {
                message
                    .pointer("/params/conversationId")
                    .and_then(Value::as_str)
            }),
        _ => None,
    })
}

/// Handles shared Codex prompt command.
///
/// When the session already has a thread id, this sends `turn/start`
/// immediately (fire-and-forget). When the session needs a new thread, this
/// sends `thread/start` or `thread/resume` as a fire-and-forget write and
/// spawns a waiter thread that extracts the thread id from the response and
/// feeds a `StartTurnAfterSetup` command back through `input_tx`. The writer
/// thread returns immediately in both cases so other commands are never
/// blocked behind thread setup.
fn handle_shared_codex_prompt_command(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    state: &AppState,
    runtime_id: &str,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    input_tx: &Sender<CodexRuntimeCommand>,
    session_id: &str,
    command: CodexPromptCommand,
) -> Result<()> {
    const SHARED_CODEX_THREAD_SETUP_TIMEOUT: Duration = Duration::from_secs(180);

    let existing_thread_id = {
        let sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions
            .get(session_id)
            .and_then(|session| session.thread_id.clone())
    };

    // Fast path: thread already exists, go straight to turn/start.
    if let Some(thread_id) = existing_thread_id {
        return handle_shared_codex_start_turn(
            writer,
            pending_requests,
            state,
            runtime_id,
            sessions,
            session_id,
            &thread_id,
            command,
        );
    }

    // Slow path: need to create or resume a thread first. Fire-and-forget the
    // setup request and spawn a waiter so the writer thread is not blocked.
    let mcp_config = state
        .termal_delegation_mcp_codex_config(session_id)
        .context("failed to build Codex delegation MCP config")?;
    let (method, params) = match command.resume_thread_id.as_deref() {
        Some(thread_id) => (
            "thread/resume",
            json!({
                "threadId": thread_id,
                "cwd": command.cwd,
                "model": command.model,
                "sandbox": command.sandbox_mode.as_cli_value(),
                "approvalPolicy": command.approval_policy.as_cli_value(),
                "config": mcp_config,
            }),
        ),
        None => (
            "thread/start",
            json!({
                "cwd": command.cwd,
                "model": command.model,
                "sandbox": command.sandbox_mode.as_cli_value(),
                "approvalPolicy": command.approval_policy.as_cli_value(),
                "personality": "pragmatic",
                "config": mcp_config,
            }),
        ),
    };

    let pending = start_codex_json_rpc_request(writer, pending_requests, method, params)?;

    let waiter_pending = pending_requests.clone();
    let waiter_state = state.clone();
    let waiter_sessions = sessions.clone();
    let waiter_thread_sessions = thread_sessions.clone();
    let waiter_runtime_id = runtime_id.to_owned();
    let waiter_session_id = session_id.to_owned();
    let waiter_input_tx = input_tx.clone();
    let waiter_method = method.to_owned();
    std::thread::spawn(move || {
        let result = wait_for_codex_json_rpc_response(
            &waiter_pending,
            pending,
            &waiter_method,
            Some(SHARED_CODEX_THREAD_SETUP_TIMEOUT),
        );

        match result {
            Ok(setup_result) => {
                let thread_id = match setup_result.pointer("/thread/id").and_then(Value::as_str) {
                    Some(id) => id.to_owned(),
                    None => {
                        let _ = waiter_state.fail_turn_if_runtime_matches(
                            &waiter_session_id,
                            &RuntimeToken::Codex(waiter_runtime_id),
                            "Codex app-server did not return a thread id",
                        );
                        return;
                    }
                };

                // Atomically verify the session still belongs to this runtime
                // and set the external session id. This closes the TOCTOU gap
                // between the old separate runtime-token check and the
                // subsequent set_external_session_id call.
                let runtime_token = RuntimeToken::Codex(waiter_runtime_id.clone());
                match waiter_state.set_external_session_id_if_runtime_matches(
                    &waiter_session_id,
                    &runtime_token,
                    thread_id.clone(),
                ) {
                    Ok(RuntimeMatchOutcome::SessionMissing | RuntimeMatchOutcome::RuntimeMismatch) => {
                        // Session was stopped or rebound while we waited for
                        // thread setup — silently discard the stale result.
                        return;
                    }
                    Err(err) => {
                        eprintln!(
                            "runtime state warning> failed to persist shared Codex thread \
                             registration for session `{}`: {err:#}",
                            waiter_session_id
                        );
                        fail_shared_codex_turn_without_runtime_exit(
                            &waiter_state,
                            &waiter_session_id,
                            &waiter_runtime_id,
                            "Failed to save session state. Check disk space and permissions.",
                            "persisting shared Codex thread registration",
                        );
                        return;
                    }
                    Ok(RuntimeMatchOutcome::Applied) => {}
                }
                remember_shared_codex_thread(
                    &waiter_sessions,
                    &waiter_thread_sessions,
                    &waiter_session_id,
                    thread_id.clone(),
                );

                // Feed the turn/start back through the writer thread so it
                // can write to stdin (which only the writer thread owns).
                let start_turn_command = CodexRuntimeCommand::StartTurnAfterSetup {
                    session_id: waiter_session_id.clone(),
                    thread_id: thread_id.clone(),
                    command,
                };
                if let Err(err) = waiter_input_tx.send(start_turn_command) {
                    let runtime_token = RuntimeToken::Codex(waiter_runtime_id.clone());
                    forget_shared_codex_thread(
                        &waiter_sessions,
                        &waiter_thread_sessions,
                        &waiter_session_id,
                        &thread_id,
                    );
                    // Suppress rediscovery only for newly created threads
                    // (thread/start). Resumed pre-existing threads should
                    // remain discoverable — they are not orphans.
                    let suppress_rediscovery = waiter_method == "thread/start";
                    if let Err(clear_err) = waiter_state
                        .clear_external_session_id_if_runtime_matches(
                            &waiter_session_id,
                            &runtime_token,
                            &thread_id,
                            suppress_rediscovery,
                        )
                    {
                        eprintln!(
                            "runtime state warning> failed to roll back shared Codex thread \
                             registration for `{}` after writer shutdown: {clear_err:#}",
                            waiter_session_id
                        );
                    }
                    let detail = format!(
                        "failed to queue shared Codex turn/start after thread setup: {err}"
                    );
                    let _ = waiter_state
                        .handle_shared_codex_runtime_exit(&waiter_runtime_id, Some(&detail));
                }
            }
            Err(CodexResponseError::JsonRpc(detail)
                | CodexResponseError::Timeout(detail)) => {
                let _ = waiter_state.fail_turn_if_runtime_matches(
                    &waiter_session_id,
                    &RuntimeToken::Codex(waiter_runtime_id),
                    &detail,
                );
            }
            Err(CodexResponseError::Transport(detail)) => {
                let _ = waiter_state.handle_shared_codex_runtime_exit(
                    &waiter_runtime_id,
                    Some(&shared_codex_runtime_command_error_detail(&anyhow!(detail))),
                );
            }
        }
    });
    Ok(())
}

/// Sends `turn/start` as a fire-and-forget request and spawns a waiter thread
/// for the response. Called directly when the thread id is already known, or
/// via the `StartTurnAfterSetup` command after thread setup completes.
fn handle_shared_codex_start_turn(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    state: &AppState,
    runtime_id: &str,
    sessions: &SharedCodexSessionMap,
    session_id: &str,
    thread_id: &str,
    command: CodexPromptCommand,
) -> Result<()> {
    const SHARED_CODEX_TURN_START_TIMEOUT: Duration = Duration::from_secs(120);
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    let request_id = Uuid::new_v4().to_string();

    // Session may have been killed between thread setup and StartTurnAfterSetup
    // arriving. Fail the turn gracefully instead of crashing the shared runtime.
    match state.record_codex_runtime_config_if_runtime_matches(
        session_id,
        &runtime_token,
        command.sandbox_mode,
        command.approval_policy,
        command.reasoning_effort,
    ) {
        Ok(RuntimeMatchOutcome::Applied) => {}
        Ok(RuntimeMatchOutcome::SessionMissing | RuntimeMatchOutcome::RuntimeMismatch) => {
            return Ok(());
        }
        Err(err) => {
            eprintln!(
                "runtime state warning> failed to persist shared Codex runtime config \
                 for session `{session_id}`: {err:#}"
            );
            fail_shared_codex_turn_without_runtime_exit(
                state,
                session_id,
                runtime_id,
                "Failed to save session state. Check disk space and permissions.",
                "persisting shared Codex runtime config",
            );
            return Ok(());
        }
    }

    {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions.entry(session_id.to_owned()).or_default();
        session_state.thread_id = Some(thread_id.to_owned());
        clear_shared_codex_turn_session_state(session_state);
        session_state.pending_turn_start_request_id = Some(request_id.clone());
        session_state.turn_started = false;
    }

    let pending_turn_request = match start_codex_json_rpc_request_with_id(
        writer,
        pending_requests,
        request_id.clone(),
        "turn/start",
        json!({
            "threadId": thread_id,
            "cwd": command.cwd,
            "approvalPolicy": command.approval_policy.as_cli_value(),
            "effort": command.reasoning_effort.as_api_value(),
            "model": command.model,
            "sandboxPolicy": codex_sandbox_policy_value(command.sandbox_mode),
            "input": codex_user_input_items(&command.prompt, &command.attachments),
        }),
    ) {
        Ok(pending_turn_request) => pending_turn_request,
        Err(err) => {
            let mut sessions = sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            if let Some(session_state) = sessions.get_mut(session_id) {
                if session_state.pending_turn_start_request_id.as_deref() == Some(&request_id) {
                    session_state.pending_turn_start_request_id = None;
                    session_state.turn_started = false;
                }
            }
            return Err(err.into());
        }
    };

    let wait_pending_requests = pending_requests.clone();
    let wait_sessions = sessions.clone();
    let wait_state = state.clone();
    let wait_runtime_id = runtime_id.to_owned();
    let wait_session_id = session_id.to_owned();
    std::thread::spawn(move || {
        let result = wait_for_codex_json_rpc_response(
            &wait_pending_requests,
            pending_turn_request,
            "turn/start",
            Some(SHARED_CODEX_TURN_START_TIMEOUT),
        );

        match result {
            Ok(turn_result) => {
                let mut sessions = wait_sessions
                    .lock()
                    .expect("shared Codex session mutex poisoned");
                let Some(session_state) = sessions.get_mut(&wait_session_id) else {
                    return;
                };
                if session_state.pending_turn_start_request_id.as_deref() != Some(&request_id) {
                    return;
                }
                session_state.pending_turn_start_request_id = None;
                if session_state.turn_id.is_none() {
                    session_state.turn_id = turn_result
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
            Err(CodexResponseError::JsonRpc(detail)
                | CodexResponseError::Timeout(detail)) => {
                {
                    let mut sessions = wait_sessions
                        .lock()
                        .expect("shared Codex session mutex poisoned");
                    let Some(session_state) = sessions.get_mut(&wait_session_id) else {
                        return;
                    };
                    if session_state.pending_turn_start_request_id.as_deref() != Some(&request_id) {
                        return;
                    }
                    session_state.pending_turn_start_request_id = None;
                }
                if let Err(err) = wait_state.fail_turn_if_runtime_matches(
                    &wait_session_id,
                    &RuntimeToken::Codex(wait_runtime_id.clone()),
                    &detail,
                ) {
                    eprintln!(
                        "runtime state warning> failed to mark shared Codex turn error for session `{}`: {err:#}",
                        wait_session_id
                    );
                }
            }
            Err(CodexResponseError::Transport(detail)) => {
                let should_fail = {
                    let mut sessions = wait_sessions
                        .lock()
                        .expect("shared Codex session mutex poisoned");
                    let Some(session_state) = sessions.get_mut(&wait_session_id) else {
                        return;
                    };
                    if session_state.pending_turn_start_request_id.as_deref() != Some(&request_id) {
                        false
                    } else {
                        session_state.pending_turn_start_request_id = None;
                        true
                    }
                };
                if should_fail {
                    if let Err(err) = wait_state.handle_shared_codex_runtime_exit(
                        &wait_runtime_id,
                        Some(&shared_codex_runtime_command_error_detail(&anyhow!(detail))),
                    ) {
                        eprintln!(
                            "runtime state warning> failed to tear down shared Codex runtime for session `{}`: {err:#}",
                            wait_session_id
                        );
                    }
                }
            }
        }
    });
    Ok(())
}

fn fail_shared_codex_turn_without_runtime_exit(
    state: &AppState,
    session_id: &str,
    runtime_id: &str,
    detail: &str,
    context: &str,
) {
    if let Err(err) = state.fail_turn_if_runtime_matches(
        session_id,
        &RuntimeToken::Codex(runtime_id.to_owned()),
        detail,
    ) {
        eprintln!(
            "runtime state warning> failed to mark shared Codex turn error for session `{}` after {}: {err:#}",
            session_id,
            context,
        );
    }
}

/// Handles shared Codex prompt command results.
fn handle_shared_codex_prompt_command_result(
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    result: Result<()>,
) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            if let Some(
                CodexResponseError::JsonRpc(detail) | CodexResponseError::Timeout(detail),
            ) = err.downcast_ref::<CodexResponseError>()
            {
                state.fail_turn_if_runtime_matches(session_id, runtime_token, detail)?;
                return Ok(());
            }
            Err(err)
        }
    }
}
