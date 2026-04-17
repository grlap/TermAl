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
    let (method, params) = match command.resume_thread_id.as_deref() {
        Some(thread_id) => (
            "thread/resume",
            json!({
                "threadId": thread_id,
                "cwd": command.cwd,
                "model": command.model,
                "sandbox": command.sandbox_mode.as_cli_value(),
                "approvalPolicy": command.approval_policy.as_cli_value(),
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

/// Handles shared Codex app server message.
fn handle_shared_codex_app_server_message(
    message: &Value,
    state: &AppState,
    runtime_id: &str,
    pending_requests: &CodexPendingRequestMap,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    input_tx: &Sender<CodexRuntimeCommand>,
) -> Result<()> {
    if let Some(response_id) = message.get("id") {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = codex_request_id_key(response_id);
            let sender = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&key);
            if let Some(sender) = sender {
                let response = if let Some(result) = message.get("result") {
                    Ok(result.clone())
                } else {
                    Err(CodexResponseError::JsonRpc(summarize_codex_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    )))
                };
                let _ = sender.send(response);
            }
            return Ok(());
        }
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        log_unhandled_codex_event("Codex app-server message missing method", message);
        return Ok(());
    };

    if method == "account/rateLimits/updated" {
        let Some(rate_limits) = message.pointer("/params/rateLimits") else {
            log_unhandled_codex_event(
                "Codex rate limit notification missing params.rateLimits",
                message,
            );
            return Ok(());
        };

        match serde_json::from_value::<CodexRateLimits>(rate_limits.clone()) {
            Ok(rate_limits) => state.note_codex_rate_limits(rate_limits)?,
            Err(err) => {
                log_unhandled_codex_event(
                    &format!("failed to parse Codex rate limits notification: {err}"),
                    message,
                );
            }
        }
        return Ok(());
    }

    if handle_shared_codex_global_notice(method, message, state)? {
        return Ok(());
    }

    let Some(thread_id) = shared_codex_session_thread_id(method, message) else {
        match method {
            "thread/archived"
            | "thread/closed"
            | "thread/compacted"
            | "thread/name/updated"
            | "thread/realtime/closed"
            | "thread/realtime/error"
            | "thread/realtime/itemAdded"
            | "thread/realtime/outputAudio/delta"
            | "thread/realtime/started"
            | "thread/status/changed"
            | "thread/tokenUsage/updated" => return Ok(()),
            _ => {
                if let Some(notice) = build_shared_codex_runtime_notice(method, message) {
                    state.note_codex_notice(notice)?;
                    return Ok(());
                }
                log_unhandled_codex_event(
                    &format!("shared Codex event missing thread id for `{method}`"),
                    message,
                );
                return Ok(());
            }
        }
    };

    let Some(session_id) = find_shared_codex_session_id(state, thread_sessions, thread_id) else {
        // Auto-reject server requests for unknown sessions so Codex does not
        // hang waiting for a response that will never come.
        reject_undeliverable_codex_server_request(message, input_tx);
        return Ok(());
    };
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    if !state.session_matches_runtime_token(&session_id, &runtime_token) {
        reject_undeliverable_codex_server_request(message, input_tx);
        return Ok(());
    }

    let mut shared_sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    // Lazy registration: the session may exist in state.inner (via
    // find_shared_codex_session_id) but not yet in the shared session map
    // because remember_shared_codex_thread hasn't run. Insert a default
    // entry so early events from Codex are not dropped.
    let session_state = shared_sessions.entry(session_id.clone()).or_default();
    let SharedCodexSessionState {
        pending_turn_start_request_id,
        recorder: recorder_state,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        turn_state,
    } = session_state;
    let turn_started_turn_id = (method == "turn/started")
        .then(|| message.pointer("/params/turn/id").and_then(Value::as_str))
        .flatten();
    if method == "turn/started" && turn_started_turn_id != turn_id.as_deref() {
        clear_shared_codex_turn_recorder_state(recorder_state);
        clear_codex_turn_state(turn_state);
        *pending_turn_start_request_id = None;
        *completed_turn_id = None;
        *turn_started = false;
    }
    let mut recorder = BorrowedSessionRecorder::new(state, &session_id, recorder_state);

    if message.get("id").is_some() {
        let event_turn_id = shared_codex_event_turn_id(message);
        if !shared_codex_app_server_event_matches_active_turn(
            turn_id.as_deref(),
            *turn_started,
            event_turn_id,
        ) {
            return Ok(());
        }
        return handle_codex_app_server_request(method, message, &mut recorder);
    }

    handle_shared_codex_app_server_notification(
        method,
        message,
        state,
        &session_id,
        &runtime_token,
        sessions,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        pending_turn_start_request_id,
        turn_state,
        thread_sessions,
        &mut recorder,
    )
}

/// Handles shared Codex global notice.
fn handle_shared_codex_global_notice(
    method: &str,
    message: &Value,
    state: &AppState,
) -> Result<bool> {
    let notice = match method {
        "configWarning" => build_shared_codex_global_notice(
            CodexNoticeKind::ConfigWarning,
            CodexNoticeLevel::Warning,
            "Config warning",
            message,
        ),
        "deprecationNotice" => build_shared_codex_global_notice(
            CodexNoticeKind::DeprecationNotice,
            CodexNoticeLevel::Info,
            "Deprecation notice",
            message,
        ),
        _ => return Ok(false),
    };

    if let Some(notice) = notice {
        state.note_codex_notice(notice)?;
    } else {
        log_unhandled_codex_event(
            &format!("failed to parse shared Codex global notice `{method}`"),
            message,
        );
    }

    Ok(true)
}

/// Builds shared Codex runtime notice.
fn build_shared_codex_runtime_notice(method: &str, message: &Value) -> Option<CodexNotice> {
    build_shared_codex_global_notice(
        CodexNoticeKind::RuntimeNotice,
        infer_shared_codex_notice_level(method, message),
        &format!("Codex notice: {method}"),
        message,
    )
}

/// Infers shared Codex notice level.
fn infer_shared_codex_notice_level(method: &str, message: &Value) -> CodexNoticeLevel {
    let payload = message.get("params").unwrap_or(message);
    let severity = payload
        .get("level")
        .or_else(|| payload.get("severity"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase());

    match severity.as_deref() {
        Some("warning") | Some("warn") | Some("error") => CodexNoticeLevel::Warning,
        Some("info") | Some("notice") => CodexNoticeLevel::Info,
        _ => {
            let normalized = method.to_ascii_lowercase();
            if normalized.contains("warning")
                || normalized.contains("error")
                || normalized.contains("auth")
                || normalized.contains("maintenance")
            {
                CodexNoticeLevel::Warning
            } else {
                CodexNoticeLevel::Info
            }
        }
    }
}

/// Builds shared Codex global notice.
fn build_shared_codex_global_notice(
    kind: CodexNoticeKind,
    level: CodexNoticeLevel,
    default_title: &str,
    message: &Value,
) -> Option<CodexNotice> {
    let payload = message.get("params").unwrap_or(message);
    let code = extract_shared_codex_notice_text(
        payload,
        &[
            "/code",
            "/id",
            "/warningCode",
            "/warning/code",
            "/deprecationId",
            "/deprecation/id",
        ],
    );
    let title = extract_shared_codex_notice_text(
        payload,
        &["/title", "/name", "/warning/title", "/deprecation/title"],
    );
    let detail = extract_shared_codex_notice_text(
        payload,
        &[
            "/detail",
            "/message",
            "/description",
            "/text",
            "/warning/message",
            "/warning/detail",
            "/deprecation/message",
            "/deprecation/detail",
        ],
    );

    let (title, detail) = match (title, detail, code.clone()) {
        (Some(title), Some(detail), _) => (title, detail),
        (Some(title), None, _) if title != default_title => (default_title.to_owned(), title),
        (None, Some(detail), _) => (default_title.to_owned(), detail),
        (None, None, Some(code)) => (default_title.to_owned(), format!("Code: `{code}`")),
        _ => return None,
    };

    Some(CodexNotice {
        kind,
        level,
        title,
        detail,
        timestamp: stamp_now(),
        code,
    })
}

/// Extracts shared Codex notice text.
fn extract_shared_codex_notice_text(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

/// Handles shared Codex app server notification.
fn handle_shared_codex_app_server_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    sessions: &SharedCodexSessionMap,
    session_thread_id: &mut Option<String>,
    turn_id: &mut Option<String>,
    completed_turn_id: &mut Option<String>,
    turn_started: &mut bool,
    pending_turn_start_request_id: &mut Option<String>,
    turn_state: &mut CodexTurnState,
    thread_sessions: &SharedCodexThreadMap,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = message.pointer("/params/thread/id").and_then(Value::as_str) {
                let previous_thread_id = session_thread_id.replace(thread_id.to_owned());
                *turn_id = None;
                *completed_turn_id = None;
                *turn_started = false;
                *pending_turn_start_request_id = None;
                let mut thread_sessions = thread_sessions
                    .lock()
                    .expect("shared Codex thread mutex poisoned");
                if let Some(previous_thread_id) = previous_thread_id {
                    if previous_thread_id != thread_id {
                        thread_sessions.remove(&previous_thread_id);
                    }
                }
                thread_sessions.insert(thread_id.to_owned(), session_id.to_owned());
                state.set_external_session_id(session_id, thread_id.to_owned())?;
                recorder.note_external_session(thread_id)?;
            }
        }
        "thread/archived" => {
            state.set_codex_thread_state_if_runtime_matches(
                session_id,
                runtime_token,
                CodexThreadState::Archived,
            )?;
        }
        "thread/unarchived" => {
            state.set_codex_thread_state_if_runtime_matches(
                session_id,
                runtime_token,
                CodexThreadState::Active,
            )?;
        }
        "turn/started" => {
            let next_turn_id = message.pointer("/params/turn/id").and_then(Value::as_str);
            let turn_changed = turn_id.as_deref() != next_turn_id;
            *turn_id = next_turn_id.map(str::to_owned);
            *completed_turn_id = None;
            *turn_started = true;
            *pending_turn_start_request_id = None;
            if turn_changed {
                recorder.finish_streaming_text()?;
            }
        }
        "turn/completed" => {
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    *turn_id = None;
                    *completed_turn_id = None;
                    *turn_started = false;
                    *pending_turn_start_request_id = None;
                    clear_codex_turn_state(turn_state);
                    recorder.reset_turn_state()?;
                    state.fail_turn_if_runtime_matches(
                        session_id,
                        runtime_token,
                        &summarize_error(error),
                    )?;
                    return Ok(());
                }
            }

            *completed_turn_id = turn_id.clone().or_else(|| {
                message
                    .pointer("/params/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            *turn_id = None;
            *turn_started = false;
            *pending_turn_start_request_id = None;
            flush_pending_codex_subagent_results(turn_state, recorder)?;
            recorder.finish_streaming_text()?;
            state.finish_turn_ok_if_runtime_matches(session_id, runtime_token)?;
            if let Some(completed_turn_id) = completed_turn_id.as_deref() {
                schedule_shared_codex_completed_turn_cleanup(
                    sessions,
                    session_id,
                    completed_turn_id,
                );
            }
        }
        "item/started" => {
            let event_turn_id = shared_codex_event_turn_id(message);
            if !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_started(item, recorder)?;
            }
        }
        "item/completed" => {
            let Some(item) = message.get("params").and_then(|params| params.get("item")) else {
                return Ok(());
            };
            let event_turn_id = shared_codex_event_turn_id(message);
            let matches_completed_agent_message = turn_id.is_none()
                && completed_turn_id.is_some()
                && matches!(item.get("type").and_then(Value::as_str), Some("agentMessage"))
                && match event_turn_id {
                    Some(event) => completed_turn_id.as_deref() == Some(event),
                    None => true,
                };
            if !matches_completed_agent_message
                && !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            handle_codex_app_server_item_completed(item, state, session_id, turn_state, recorder)?;
        }
        "item/agentMessage/delta" => {
            let event_turn_id = shared_codex_event_turn_id(message);
            let matches_completed_turn = turn_id.is_none()
                && completed_turn_id.is_some()
                && match event_turn_id {
                    Some(event) => completed_turn_id.as_deref() == Some(event),
                    None => true,
                };
            if !matches_completed_turn
                && !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(item_id) = message.pointer("/params/itemId").and_then(Value::as_str) else {
                return Ok(());
            };
            record_codex_agent_message_delta(
                turn_state, recorder, state, session_id, item_id, delta,
            )?;
        }
        "model/rerouted" => {
            handle_shared_codex_model_rerouted(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "thread/compacted" => {
            handle_shared_codex_thread_compacted(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "thread/status/changed"
        | "turn/diff/updated"
        | "turn/plan/updated"
        | "item/commandExecution/outputDelta"
        | "item/commandExecution/terminalInteraction"
        | "item/fileChange/outputDelta"
        | "item/plan/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta"
        | "thread/tokenUsage/updated"
        | "thread/name/updated"
        | "thread/closed"
        | "thread/realtime/started"
        | "thread/realtime/itemAdded"
        | "thread/realtime/outputAudio/delta"
        | "thread/realtime/error"
        | "thread/realtime/closed" => {}
        "error" => {
            *turn_id = None;
            *completed_turn_id = None;
            *turn_started = false;
            *pending_turn_start_request_id = None;
            clear_codex_turn_state(turn_state);
            recorder.reset_turn_state()?;
            let payload = message.get("params").unwrap_or(message);
            let detail = summarize_error(payload);

            if is_retryable_connectivity_error(payload) {
                state.note_turn_retry_if_runtime_matches(session_id, runtime_token, &detail)?;
            } else {
                state.fail_turn_if_runtime_matches(session_id, runtime_token, &detail)?;
            }
        }
        "codex/event/item_completed" => {
            handle_shared_codex_event_item_completed(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message_content_delta" => {
            handle_shared_codex_event_agent_message_content_delta(
                message,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
                state,
                session_id,
            )?;
        }
        "codex/event/agent_message" => {
            handle_shared_codex_event_agent_message(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "codex/event/task_complete" => {
            handle_shared_codex_task_complete(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
            )?;
        }
        _ if method.starts_with("codex/event/") => {}
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled shared Codex app-server notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

/// Handles shared Codex task complete.
fn handle_shared_codex_task_complete(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
) -> Result<()> {
    let Some(summary) = message
        .pointer("/params/msg/last_agent_message")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let conversation_id = message
        .pointer("/params/conversationId")
        .and_then(Value::as_str);
    let turn_id = shared_codex_event_turn_id(message);
    if current_turn_id.is_none() {
        return Ok(());
    }
    if !shared_codex_event_matches_active_turn(current_turn_id, turn_id) {
        return Ok(());
    }

    if let Some(anchor_message_id) = turn_state.first_visible_assistant_message_id.as_deref() {
        state.insert_message_before(
            session_id,
            anchor_message_id,
            Message::SubagentResult {
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Subagent completed".to_owned(),
                summary: trimmed.to_owned(),
                conversation_id: conversation_id.map(str::to_owned),
                turn_id: turn_id.map(str::to_owned),
            },
        )?;
        return Ok(());
    }

    buffer_codex_subagent_result(
        turn_state,
        "Subagent completed",
        trimmed,
        conversation_id,
        turn_id,
    );
    Ok(())
}

/// Handles shared Codex event turn ID.
fn shared_codex_event_turn_id<'a>(message: &'a Value) -> Option<&'a str> {
    message
        .pointer("/params/msg/turn_id")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/params/turnId").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn_id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn/id").and_then(Value::as_str))
}

/// Handles shared Codex event matches active turn.
fn shared_codex_event_matches_active_turn(
    current_turn_id: Option<&str>,
    event_turn_id: Option<&str>,
) -> bool {
    match current_turn_id {
        Some(current) => {
            event_turn_id.is_none() || matches!(event_turn_id, Some(event) if current == event)
        }
        None => false,
    }
}

/// Matches shared Codex final-output events against either the active turn or
/// the most recently completed turn.
fn shared_codex_event_matches_visible_turn(
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    event_turn_id: Option<&str>,
) -> bool {
    if shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return true;
    }

    matches!(
        (current_turn_id, completed_turn_id, event_turn_id),
        (None, Some(completed), Some(event)) if completed == event
    )
}

/// Matches shared Codex app-server events against the active turn.
fn shared_codex_app_server_event_matches_active_turn(
    current_turn_id: Option<&str>,
    turn_started: bool,
    event_turn_id: Option<&str>,
) -> bool {
    match current_turn_id {
        Some(current) => match event_turn_id {
            Some(event) => current == event,
            None => turn_started,
        },
        None => false,
    }
}

/// Pushes shared Codex turn notice.
fn push_shared_codex_turn_notice(
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let message = Message::Text {
        attachments: Vec::new(),
        id: state.allocate_message_id(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        text: trimmed.to_owned(),
        expanded_text: None,
    };

    if let Some(anchor_message_id) = turn_state.first_visible_assistant_message_id.as_deref() {
        state.insert_message_before(session_id, anchor_message_id, message)?;
        return Ok(());
    }

    state.push_message(session_id, message)
}

/// Handles shared Codex model rerouted.
fn handle_shared_codex_model_rerouted(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    _recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return Ok(());
    }

    let Some(from_model) = message.pointer("/params/fromModel").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(to_model) = message.pointer("/params/toModel").and_then(Value::as_str) else {
        return Ok(());
    };
    if from_model == to_model {
        return Ok(());
    }

    let reason = match message.pointer("/params/reason").and_then(Value::as_str) {
        Some("highRiskCyberActivity") => " because it detected high-risk cyber activity",
        Some(_) | None => "",
    };
    let notice = format!("Codex rerouted this turn from `{from_model}` to `{to_model}`{reason}.");
    push_shared_codex_turn_notice(state, session_id, turn_state, &notice)
}

/// Handles shared Codex thread compacted.
fn handle_shared_codex_thread_compacted(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    _recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return Ok(());
    }

    push_shared_codex_turn_notice(
        state,
        session_id,
        turn_state,
        "Codex compacted the thread context for this turn.",
    )
}

/// Records completed Codex agent message.
fn record_completed_codex_agent_message(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
    item_id: &str,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if turn_state.current_agent_message_id.as_deref() != Some(item_id) {
        recorder.finish_streaming_text()?;
        turn_state.current_agent_message_id = Some(item_id.to_owned());
    }

    if !turn_state.streamed_agent_message_item_ids.contains(item_id) {
        begin_codex_assistant_output(turn_state, recorder)?;
        recorder.push_text(trimmed)?;
        return remember_codex_first_assistant_message_id(state, session_id, turn_state);
    }

    let entry = turn_state
        .streamed_agent_message_text_by_item_id
        .entry(item_id.to_owned())
        .or_default();
    let update = next_completed_codex_text_update(entry, trimmed);
    if matches!(update, CompletedTextUpdate::NoChange) {
        return Ok(());
    }

    begin_codex_assistant_output(turn_state, recorder)?;
    match update {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Handles shared Codex event item completed.
fn handle_shared_codex_event_item_completed(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        event_turn_id,
    ) {
        return Ok(());
    }

    let Some(item) = message.pointer("/params/msg/item") else {
        return Ok(());
    };

    match item.get("type").and_then(Value::as_str) {
        Some("AgentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let text = item
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| concatenate_codex_text_parts(content));

            if let Some(text) = text.as_deref() {
                record_completed_codex_agent_message(
                    turn_state, recorder, state, session_id, item_id, text,
                )?;
            }
        }
        Some("CommandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Concatenates Codex text parts.
fn concatenate_codex_text_parts(content: &[Value]) -> Option<String> {
    let mut combined = String::new();

    for part in content {
        if part.get("type").and_then(Value::as_str) != Some("Text") {
            continue;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        combined.push_str(text);
    }

    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

/// Clears Codex turn state.
fn clear_codex_turn_state(turn_state: &mut CodexTurnState) {
    turn_state.current_agent_message_id = None;
    turn_state.streamed_agent_message_text_by_item_id.clear();
    turn_state.streamed_agent_message_item_ids.clear();
    turn_state.pending_subagent_results.clear();
    turn_state.assistant_output_started = false;
    turn_state.first_visible_assistant_message_id = None;
}

/// Clears shared Codex recorder state that should not leak across turns.
fn clear_shared_codex_turn_recorder_state(recorder_state: &mut SessionRecorderState) {
    reset_recorder_state_fields(recorder_state);
}

fn spawn_shared_codex_completed_turn_cleanup_worker(
    sessions: &SharedCodexSessionMap,
    cleanup_rx: mpsc::Receiver<SharedCodexCompletedTurnCleanup>,
) {
    let weak_sessions = Arc::downgrade(sessions);
    std::thread::Builder::new()
        .name("termal-codex-cleanup".to_owned())
        .spawn(move || {
            let mut pending = Vec::<SharedCodexCompletedTurnCleanup>::new();
            loop {
                if !run_due_shared_codex_completed_turn_cleanups(&weak_sessions, &mut pending) {
                    break;
                }

                let next_timeout = pending
                    .iter()
                    .map(|cleanup| cleanup.due_at)
                    .min()
                    .map(|due_at| due_at.saturating_duration_since(std::time::Instant::now()));

                let next_cleanup = match next_timeout {
                    Some(timeout) => match cleanup_rx.recv_timeout(timeout) {
                        Ok(cleanup) => Some(cleanup),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => match cleanup_rx.recv() {
                        Ok(cleanup) => Some(cleanup),
                        Err(_) => break,
                    },
                };

                if let Some(cleanup) = next_cleanup {
                    pending.push(cleanup);
                }
            }
        })
        .expect("failed to spawn shared Codex cleanup worker");
}

fn run_due_shared_codex_completed_turn_cleanups(
    weak_sessions: &std::sync::Weak<SharedCodexSessions>,
    pending: &mut Vec<SharedCodexCompletedTurnCleanup>,
) -> bool {
    let now = std::time::Instant::now();
    if !pending.iter().any(|cleanup| cleanup.due_at <= now) {
        return true;
    }

    let Some(sessions) = weak_sessions.upgrade() else {
        return false;
    };
    let mut sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let mut index = 0usize;
    while index < pending.len() {
        if pending[index].due_at > now {
            index += 1;
            continue;
        }

        let cleanup = pending.swap_remove(index);
        let Some(session_state) = sessions.get_mut(&cleanup.session_id) else {
            continue;
        };
        if session_state.turn_id.is_some()
            || session_state.completed_turn_id.as_deref() != Some(&cleanup.completed_turn_id)
        {
            continue;
        }
        clear_shared_codex_completed_turn_state_fields(
            &mut session_state.completed_turn_id,
            &mut session_state.turn_state,
            &mut session_state.recorder,
        );
    }
    true
}

fn read_capped_child_stdout_line(
    reader: &mut impl BufRead,
    line_buf: &mut Vec<u8>,
    max_bytes: usize,
    stream_label: &str,
) -> io::Result<usize> {
    line_buf.clear();
    let mut total_read = 0usize;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total_read);
        }

        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let consume_len = newline_index.map_or(available.len(), |index| index + 1);
        if total_read + consume_len > max_bytes {
            // The line exceeds the safety cap.  Drain the remainder so the
            // reader stays aligned with the next newline-delimited message,
            // but discard the content instead of tearing down the runtime.
            // Legitimate large messages (e.g. aggregatedOutput from long
            // command executions) can exceed the cap.
            reader.consume(consume_len);
            total_read += consume_len;
            if newline_index.is_none() {
                loop {
                    let buf = reader.fill_buf()?;
                    if buf.is_empty() {
                        break;
                    }
                    let nl = buf.iter().position(|b| *b == b'\n');
                    let n = nl.map_or(buf.len(), |i| i + 1);
                    reader.consume(n);
                    total_read += n;
                    if nl.is_some() {
                        break;
                    }
                }
            }
            eprintln!(
                "[termal] skipping oversized {stream_label} line \
                 ({total_read} bytes, cap {max_bytes} bytes)"
            );
            line_buf.clear();
            return Ok(total_read);
        }

        line_buf.extend_from_slice(&available[..consume_len]);
        reader.consume(consume_len);
        total_read += consume_len;

        if newline_index.is_some() {
            return Ok(total_read);
        }
    }
}

fn truncate_child_stdout_log_line(line: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    for (index, ch) in line.chars().enumerate() {
        if index == max_chars {
            truncated.push_str("...");
            return truncated;
        }
        truncated.push(ch);
    }
    truncated
}

fn shared_codex_bad_json_streak_failure_detail(
    consecutive_bad_json_lines: usize,
    _line: &str,
) -> Option<String> {
    if consecutive_bad_json_lines < SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES {
        return None;
    }

    // Use a generic message for the user-facing failure detail.  Raw child
    // stdout content is already logged to stderr per-line as it arrives and
    // should not leak into persisted session state or SSE updates.
    Some(format!(
        "shared Codex app-server produced {consecutive_bad_json_lines} consecutive non-JSON stdout lines"
    ))
}

fn clear_shared_codex_completed_turn_state_fields(
    completed_turn_id: &mut Option<String>,
    turn_state: &mut CodexTurnState,
    recorder_state: &mut SessionRecorderState,
) {
    *completed_turn_id = None;
    clear_codex_turn_state(turn_state);
    clear_shared_codex_turn_recorder_state(recorder_state);
}

/// Clears shared Codex per-turn session state before a new turn starts.
fn clear_shared_codex_turn_session_state(session_state: &mut SharedCodexSessionState) {
    session_state.pending_turn_start_request_id = None;
    session_state.turn_id = None;
    session_state.turn_started = false;
    clear_shared_codex_completed_turn_state_fields(
        &mut session_state.completed_turn_id,
        &mut session_state.turn_state,
        &mut session_state.recorder,
    );
}

fn schedule_shared_codex_completed_turn_cleanup(
    sessions: &SharedCodexSessionMap,
    session_id: &str,
    completed_turn_id: &str,
) {
    sessions.schedule_completed_turn_cleanup(session_id, completed_turn_id);
}

fn shared_codex_app_server_error_is_stale_session(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let message = cause.to_string();
        let Some(session_id) = message
            .strip_prefix("session `")
            .and_then(|message| message.strip_suffix("` not found"))
        else {
            return false;
        };

        !session_id.is_empty() && !session_id.contains('`')
    })
}

/// Buffers Codex subagent result.
fn buffer_codex_subagent_result(
    turn_state: &mut CodexTurnState,
    title: &str,
    summary: &str,
    conversation_id: Option<&str>,
    turn_id: Option<&str>,
) {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return;
    }

    turn_state
        .pending_subagent_results
        .push(PendingSubagentResult {
            title: title.to_owned(),
            summary: trimmed.to_owned(),
            conversation_id: conversation_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
        });
}

/// Flushes pending Codex subagent results.
fn flush_pending_codex_subagent_results(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    for pending in std::mem::take(&mut turn_state.pending_subagent_results) {
        recorder.push_subagent_result(
            &pending.title,
            &pending.summary,
            pending.conversation_id.as_deref(),
            pending.turn_id.as_deref(),
        )?;
    }

    Ok(())
}

/// Begins Codex assistant output.
fn begin_codex_assistant_output(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    if !turn_state.assistant_output_started {
        flush_pending_codex_subagent_results(turn_state, recorder)?;
        turn_state.assistant_output_started = true;
    }

    Ok(())
}

/// Remembers Codex first assistant message ID.
fn remember_codex_first_assistant_message_id(
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
) -> Result<()> {
    if turn_state.first_visible_assistant_message_id.is_none() {
        turn_state.first_visible_assistant_message_id = state.last_message_id(session_id)?;
    }
    Ok(())
}

/// Handles shared Codex event agent message content delta.
fn handle_shared_codex_event_agent_message_content_delta(
    message: &Value,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        event_turn_id,
    ) {
        return Ok(());
    }

    let Some(delta) = message.pointer("/params/msg/delta").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(item_id) = message
        .pointer("/params/msg/item_id")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };

    record_codex_agent_message_delta(turn_state, recorder, state, session_id, item_id, delta)
}

/// Handles shared Codex event agent message.
fn handle_shared_codex_event_agent_message(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        event_turn_id,
    ) {
        return Ok(());
    }

    let Some(text) = message
        .pointer("/params/msg/message")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if let Some(item_id) = turn_state.current_agent_message_id.clone() {
        return record_completed_codex_agent_message(
            turn_state, recorder, state, session_id, &item_id, trimmed,
        );
    }

    begin_codex_assistant_output(turn_state, recorder)?;
    recorder.push_text(trimmed)?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Records Codex agent message delta.
fn record_codex_agent_message_delta(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
    item_id: &str,
    delta: &str,
) -> Result<()> {
    if turn_state.current_agent_message_id.as_deref() != Some(item_id) {
        recorder.finish_streaming_text()?;
        turn_state.current_agent_message_id = Some(item_id.to_owned());
    }
    let entry = turn_state
        .streamed_agent_message_text_by_item_id
        .entry(item_id.to_owned())
        .or_default();
    let Some(unseen_suffix) = next_codex_delta_suffix(entry, delta) else {
        return Ok(());
    };

    begin_codex_assistant_output(turn_state, recorder)?;
    turn_state
        .streamed_agent_message_item_ids
        .insert(item_id.to_owned());
    recorder.text_delta(&unseen_suffix)?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Defines the completed text update variants.
enum CompletedTextUpdate {
    NoChange,
    Append(String),
    Replace(String),
}

/// Returns the next completed Codex text update.
fn next_completed_codex_text_update(existing: &mut String, incoming: &str) -> CompletedTextUpdate {
    if incoming.is_empty() {
        return CompletedTextUpdate::NoChange;
    }

    if existing.is_empty() {
        existing.push_str(incoming);
        return CompletedTextUpdate::Replace(incoming.to_owned());
    }

    if incoming == existing {
        return CompletedTextUpdate::NoChange;
    }

    if incoming.starts_with(existing.as_str()) {
        let split = existing.len();
        debug_assert!(incoming.is_char_boundary(split));
        let suffix = incoming[split..].to_owned();
        existing.clear();
        existing.push_str(incoming);
        return if suffix.is_empty() {
            CompletedTextUpdate::NoChange
        } else {
            CompletedTextUpdate::Append(suffix)
        };
    }

    if existing.ends_with(incoming) {
        return CompletedTextUpdate::NoChange;
    }

    existing.clear();
    existing.push_str(incoming);
    CompletedTextUpdate::Replace(incoming.to_owned())
}

/// Returns the next Codex delta suffix.
fn next_codex_delta_suffix(existing: &mut String, incoming: &str) -> Option<String> {
    if incoming.is_empty() {
        return None;
    }

    if existing.is_empty() {
        existing.push_str(incoming);
        return Some(incoming.to_owned());
    }

    if incoming == existing {
        return None;
    }

    if incoming.starts_with(existing.as_str()) {
        let split = existing.len();
        debug_assert!(incoming.is_char_boundary(split));
        let suffix = incoming[split..].to_owned();
        existing.clear();
        existing.push_str(incoming);
        return if suffix.is_empty() {
            None
        } else {
            Some(suffix)
        };
    }

    if existing.ends_with(incoming) {
        return None;
    }

    let overlap = longest_codex_delta_overlap(existing, incoming);
    let suffix = incoming[overlap..].to_owned();
    existing.push_str(&suffix);
    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

/// Handles longest Codex delta overlap.
fn longest_codex_delta_overlap(existing: &str, incoming: &str) -> usize {
    let max_overlap = existing.len().min(incoming.len());
    for overlap in (1..=max_overlap).rev() {
        if incoming.is_char_boundary(overlap) && existing.ends_with(&incoming[..overlap]) {
            return overlap;
        }
    }

    0
}

/// Handles Codex app server request.
fn handle_codex_app_server_request(
    method: &str,
    message: &Value,
    recorder: &mut impl CodexTurnRecorder,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("Codex app-server request missing id"))?;
    let params = message
        .get("params")
        .ok_or_else(|| anyhow!("Codex app-server request missing params"))?;

    match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command execution");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if cwd.is_empty() && reason.is_empty() {
                "Codex requested approval to execute a command.".to_owned()
            } else if reason.is_empty() {
                format!("Codex requested approval to execute this command in {cwd}.")
            } else if cwd.is_empty() {
                format!("Codex requested approval to execute this command. Reason: {reason}")
            } else {
                format!(
                    "Codex requested approval to execute this command in {cwd}. Reason: {reason}"
                )
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                command,
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::CommandExecution,
                    request_id,
                },
            )?;
        }
        "item/fileChange/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if reason.is_empty() {
                "Codex requested approval to apply file changes.".to_owned()
            } else {
                format!("Codex requested approval to apply file changes. Reason: {reason}")
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Apply file changes",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::FileChange,
                    request_id,
                },
            )?;
        }
        "item/permissions/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let permissions_summary = describe_codex_permission_request(
                params.get("permissions").unwrap_or(&Value::Null),
            );
            let detail = match (
                reason.trim().is_empty(),
                permissions_summary
                    .as_deref()
                    .filter(|value| !value.is_empty()),
            ) {
                (true, Some(summary)) => {
                    format!("Codex requested approval to grant additional permissions: {summary}.")
                }
                (false, Some(summary)) => format!(
                    "Codex requested approval to grant additional permissions: {summary}. Reason: {reason}"
                ),
                (true, None) => {
                    "Codex requested approval to grant additional permissions.".to_owned()
                }
                (false, None) => format!(
                    "Codex requested approval to grant additional permissions. Reason: {reason}"
                ),
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Grant additional permissions",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::Permissions {
                        requested_permissions: params
                            .get("permissions")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    },
                    request_id,
                },
            )?;
        }
        "item/tool/requestUserInput" => {
            let questions: Vec<UserInputQuestion> = serde_json::from_value(
                params
                    .get("questions")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .context("failed to parse Codex request_user_input questions")?;
            let detail = describe_codex_user_input_request(&questions);

            recorder.push_codex_user_input_request(
                "Codex needs input",
                &detail,
                questions.clone(),
                CodexPendingUserInput {
                    questions,
                    request_id,
                },
            )?;
        }
        "mcpServer/elicitation/request" => {
            let request: McpElicitationRequestPayload = serde_json::from_value(params.clone())
                .context("failed to parse Codex MCP elicitation request")?;
            let detail = describe_codex_mcp_elicitation_request(&request);

            recorder.push_codex_mcp_elicitation_request(
                "Codex needs MCP input",
                &detail,
                request.clone(),
                CodexPendingMcpElicitation {
                    request,
                    request_id,
                },
            )?;
        }
        _ => {
            let (title, detail) = describe_codex_app_server_request(method, params);
            recorder.push_codex_app_request(
                &title,
                &detail,
                method,
                params.clone(),
                CodexPendingAppRequest { request_id },
            )?;
        }
    }

    Ok(())
}

/// Describes Codex app server request.
fn describe_codex_app_server_request(method: &str, params: &Value) -> (String, String) {
    if method == "item/tool/call" {
        let tool = params
            .get("tool")
            .or_else(|| params.get("toolName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("tool");
        let server = params
            .get("server")
            .or_else(|| params.get("serverName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let scope = server
            .map(|server_name| format!(" from `{server_name}`"))
            .unwrap_or_default();
        return (
            "Codex needs a tool result".to_owned(),
            format!(
                "Codex requested a result for `{tool}`{scope}. Review the request payload and submit the JSON result to continue."
            ),
        );
    }

    (
        "Codex needs a response".to_owned(),
        format!(
            "Codex sent an app-server request `{method}` that needs a JSON result before it can continue."
        ),
    )
}

/// Handles Codex app server item started.
fn handle_codex_app_server_item_started(
    item: &Value,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            recorder.finish_streaming_text()?;
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                recorder.command_started(key, command)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            recorder.command_started(key, &command)?;
        }
        _ => {}
    }

    Ok(())
}

/// Describes Codex permission request.
fn describe_codex_permission_request(permissions: &Value) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(read_paths) = permissions
        .pointer("/fileSystem/read")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = read_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("read access to `{joined}`"));
        }
    }

    if let Some(write_paths) = permissions
        .pointer("/fileSystem/write")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = write_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("write access to `{joined}`"));
        }
    }

    if permissions
        .pointer("/network/enabled")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("network access".to_owned());
    }

    if permissions
        .pointer("/macos/accessibility")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS accessibility access".to_owned());
    }

    if permissions
        .pointer("/macos/calendar")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS calendar access".to_owned());
    }

    if let Some(preferences) = permissions
        .pointer("/macos/preferences")
        .and_then(Value::as_str)
        .filter(|value| *value != "none")
    {
        parts.push(format!("macOS preferences access ({preferences})"));
    }

    if let Some(automations) = permissions.pointer("/macos/automations") {
        if let Some(scope) = automations.as_str() {
            if scope == "all" {
                parts.push("macOS automation access".to_owned());
            }
        } else if let Some(bundle_ids) = automations.get("bundle_ids").and_then(Value::as_array) {
            let joined = bundle_ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if !joined.is_empty() {
                parts.push(format!("macOS automation access for `{joined}`"));
            }
        }
    }

    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Describes Codex user input request.
fn describe_codex_user_input_request(questions: &[UserInputQuestion]) -> String {
    match questions.len() {
        0 => "Codex requested additional input.".to_owned(),
        1 => {
            let question = &questions[0];
            format!(
                "Codex requested additional input for \"{}\".",
                question.header.trim()
            )
        }
        count => format!("Codex requested additional input for {count} questions."),
    }
}

/// Describes Codex MCP elicitation request.
fn describe_codex_mcp_elicitation_request(request: &McpElicitationRequestPayload) -> String {
    match &request.mode {
        McpElicitationRequestMode::Form { message, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested additional structured input.",
                    request.server_name
                )
            } else {
                format!(
                    "MCP server {} requested additional structured input. {}",
                    request.server_name, trimmed
                )
            }
        }
        McpElicitationRequestMode::Url { message, url, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested that you continue in a browser: {}",
                    request.server_name, url
                )
            } else {
                format!(
                    "MCP server {} requested that you continue in a browser. {} {}",
                    request.server_name, trimmed, url
                )
            }
        }
    }
}

/// Handles Codex app server item completed.
fn handle_codex_app_server_item_completed(
    item: &Value,
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                record_completed_codex_agent_message(
                    turn_state, recorder, state, session_id, item_id, text,
                )?;
            }
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        Some("fileChange") => {
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return Ok(());
            }
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return Ok(());
            };
            for change in changes {
                let Some(file_path) = change.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
                if diff.trim().is_empty() {
                    continue;
                }
                let change_type = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => ChangeType::Create,
                    _ => ChangeType::Edit,
                };
                let summary = match change_type {
                    ChangeType::Create => format!("Created {}", short_file_name(file_path)),
                    ChangeType::Edit => format!("Updated {}", short_file_name(file_path)),
                };
                recorder.push_diff(file_path, &summary, diff, change_type)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            let output = summarize_codex_app_server_web_search_output(item);
            recorder.command_completed(key, &command, &output, CommandStatus::Success)?;
        }
        _ => {}
    }

    Ok(())
}

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
