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

/// How often `wait_for_shared_codex_response_while_server_active` wakes up to
/// re-check the server's stdout liveness while a response is outstanding.
const SHARED_CODEX_RESPONSE_POLL_SLICE: Duration = Duration::from_secs(15);

/// Hard cap on how long a thread-setup / turn-start wait may extend while the
/// app-server keeps showing stdout activity. A demonstrably-alive server gets
/// patience (a `thread/resume` replaying a huge rollout behind another
/// session's streaming turn can legitimately need several minutes); this cap
/// is what keeps a lost request from parking the session on "working"
/// forever. Giving up here fails only the requesting turn — the server is
/// active, so `handle_shared_codex_startup_response_error` takes its scoped
/// branch.
const SHARED_CODEX_RESPONSE_MAX_WAIT_WHILE_ACTIVE: Duration = Duration::from_secs(900);

#[derive(Clone, Debug)]
struct SharedCodexStdinActivity {
    operation: &'static str,
    context: String,
    started_at: std::time::Instant,
    timed_out: bool,
}

type SharedCodexStdinActivityState = Arc<Mutex<Option<SharedCodexStdinActivity>>>;
type SharedCodexStdinContextState = Arc<Mutex<String>>;

struct SharedCodexStdinActivityGuard<'a> {
    activity: &'a SharedCodexStdinActivityState,
}

impl<'a> SharedCodexStdinActivityGuard<'a> {
    fn new(
        activity: &'a SharedCodexStdinActivityState,
        operation: &'static str,
        context: String,
    ) -> SharedCodexStdinActivityGuard<'a> {
        *activity
            .lock()
            .expect("shared Codex stdin activity mutex poisoned") = Some(SharedCodexStdinActivity {
            operation,
            context,
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
    context: SharedCodexStdinContextState,
}

impl<W> SharedCodexWatchedWriter<W> {
    fn new(inner: W, activity: SharedCodexStdinActivityState) -> Self {
        SharedCodexWatchedWriter {
            inner,
            activity,
            context: Arc::new(Mutex::new("idle".to_owned())),
        }
    }

    fn set_activity_context(&mut self, context: impl Into<String>) {
        *self
            .context
            .lock()
            .expect("shared Codex stdin context mutex poisoned") = context.into();
    }

    fn activity_context(&self) -> SharedCodexStdinContextState {
        self.context.clone()
    }
}

impl<W: Write> Write for SharedCodexWatchedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let context = self
            .context
            .lock()
            .expect("shared Codex stdin context mutex poisoned")
            .clone();
        let _guard = SharedCodexStdinActivityGuard::new(&self.activity, "write", context);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let context = self
            .context
            .lock()
            .expect("shared Codex stdin context mutex poisoned")
            .clone();
        let _guard = SharedCodexStdinActivityGuard::new(&self.activity, "flush", context);
        self.inner.flush()
    }
}

fn set_shared_codex_writer_context(
    context: Option<&SharedCodexStdinContextState>,
    value: impl Into<String>,
) {
    if let Some(context) = context {
        *context
            .lock()
            .expect("shared Codex stdin context mutex poisoned") = value.into();
    }
}

fn shared_codex_stdin_timeout_detail(
    activity: &SharedCodexStdinActivity,
    timeout: Duration,
) -> String {
    // Log the internal detail to stderr; return a generic user-facing message.
    eprintln!(
        "[termal] shared Codex writer thread blocked on stdin {} for over {}s; context={}",
        activity.operation,
        timeout.as_secs(),
        activity.context
    );
    if shared_codex_trace_enabled() {
        eprintln!(
            "shared-codex trace> context=stdin_watchdog method=- session=- thread=- event_turn=- active_turn=- completed_turn=- turn_started=- pending_turn_start=- reason=blocked_{} writer_context={}",
            activity.operation,
            activity.context
        );
    }
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

            let timed_out_activity = {
                let mut locked = watchdog_activity
                    .lock()
                    .expect("shared Codex stdin activity mutex poisoned");
                match locked.as_mut() {
                    Some(entry)
                        if !entry.timed_out && entry.started_at.elapsed() >= timeout =>
                    {
                        entry.timed_out = true;
                        Some(entry.clone())
                    }
                    _ => None,
                }
            };

            if let Some(activity) = timed_out_activity {
                let detail = shared_codex_stdin_timeout_detail(&activity, timeout);
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
    if !state.agent_runtime_spawning_enabled {
        bail!("agent runtime spawning is disabled for this AppState");
    }
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
    // Seeded with the spawn instant so a server that dies before its first line
    // reads as "silent since spawn", never as "recently active".
    let stdout_activity: SharedCodexStdoutActivityState =
        Arc::new(Mutex::new(std::time::Instant::now()));

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
            let writer_context = stdin.activity_context();
            stdin.set_activity_context("initialize request");
            let initialize_result = send_codex_json_rpc_request(
                &mut stdin,
                &writer_pending_requests,
                "initialize",
                codex_initialize_params(),
                Duration::from_secs(180),
            )
            .map_err(anyhow::Error::new)
            .and_then(|_| {
                stdin.set_activity_context("initialized notification");
                write_codex_json_rpc_message(
                    &mut stdin,
                    &json_rpc_notification_message("initialized"),
                )
            });
            stdin.set_activity_context("idle");

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
                    } => {
                        stdin.set_activity_context(format!(
                            "command=Prompt session={session_id}"
                        ));
                        handle_shared_codex_prompt_command_result(
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
                                Some(&writer_context),
                                &session_id,
                                command,
                            ),
                        )
                    }
                    CodexRuntimeCommand::StartTurnAfterSetup {
                        session_id,
                        thread_id,
                        command,
                    } => {
                        stdin.set_activity_context(format!(
                            "command=StartTurnAfterSetup session={session_id} thread={thread_id}"
                        ));
                        handle_shared_codex_prompt_command_result(
                            &writer_state,
                            &session_id,
                            &writer_runtime_token,
                            handle_shared_codex_start_turn(
                                &mut stdin,
                                &writer_pending_requests,
                                &writer_state,
                                &writer_runtime_id,
                                &writer_sessions,
                                Some(&writer_context),
                                &session_id,
                                &thread_id,
                                command,
                            ),
                        )
                    }
                    CodexRuntimeCommand::JsonRpcRequest {
                        method,
                        params,
                        timeout,
                        response_tx,
                    } => {
                        stdin.set_activity_context(format!(
                            "command=JsonRpcRequest method={method}"
                        ));
                        let request_id = Uuid::new_v4().to_string();
                        stdin.set_activity_context(format!(
                            "jsonrpc_request method={method} id={request_id}"
                        ));
                        // Fire-and-forget: write the request, then spawn a
                        // waiter thread for the response. The writer thread
                        // returns immediately so other commands are not blocked.
                        match start_codex_json_rpc_request_with_id(
                            &mut stdin,
                            &writer_pending_requests,
                            request_id,
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
                        stdin.set_activity_context(format!(
                            "jsonrpc_response id={}",
                            response.request_id
                        ));
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &codex_json_rpc_response_message(&response),
                        )
                    }
                    CodexRuntimeCommand::JsonRpcNotification { method } => {
                        stdin.set_activity_context(format!(
                            "command=JsonRpcNotification method={method}"
                        ));
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
                        let request_id = Uuid::new_v4().to_string();
                        stdin.set_activity_context(format!(
                            "jsonrpc_request method=turn/interrupt id={request_id} thread={thread_id} turn={turn_id}"
                        ));
                        // Fire-and-forget: write the interrupt request, then
                        // spawn a waiter thread for the ack. The writer thread
                        // returns immediately so new commands (e.g. a follow-up
                        // prompt) are not blocked behind a slow interrupt ack.
                        match start_codex_json_rpc_request_with_id(
                            &mut stdin,
                            &writer_pending_requests,
                            request_id,
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
                        stdin.set_activity_context("command=RefreshModelList");
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
                    } => {
                        stdin.set_activity_context(format!(
                            "command=RefreshModelListPage page={page_count}"
                        ));
                        fire_codex_model_list_page(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_input_tx,
                            Some(cursor),
                            accumulated,
                            page_count,
                            response_tx,
                        )
                    }
                };
                stdin.set_activity_context("idle");

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
        let reader_stdout_activity = stdout_activity.clone();
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

                // Any stdout line — event, response, even a bad-JSON one — proves
                // the app-server process is alive and producing output. Stamp
                // BEFORE parsing/validity checks: this feeds the busy-vs-wedged
                // decision in `handle_shared_codex_startup_response_error`, and
                // that decision is about liveness, not about protocol health.
                *reader_stdout_activity
                    .lock()
                    .expect("shared Codex stdout activity mutex poisoned") =
                    std::time::Instant::now();

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
                if !should_forward_runtime_stderr_line("codex", &line) {
                    continue;
                }
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
        stdout_activity,
    })
}

/// Forgets a shared Codex thread mapping that was registered provisionally.
///
/// CALLERS BEWARE: this calls `clear_shared_codex_turn_session_state`, which drops
/// any prompt parked on an in-flight thread setup. The `thread_id` guard below does
/// NOT protect you from that — `thread/started` can bind the thread while the setup
/// is still pending, so a session can be `{setup in flight}` AND `{thread bound}` at
/// once.
///
/// The one existing caller is safe by POSITION: it runs on the `StartTurnAfterSetup`
/// hand-off failure path, i.e. after `complete_shared_codex_thread_setup` has already
/// taken the setup out of the slot. A second caller on any other path must take the
/// setup out itself first, or it will eat a user's prompt and leave the setup's
/// waiter to disown the session's own live thread.
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

/// Finds shared Codex session ID by active/completed turn ID.
fn find_shared_codex_session_id_by_turn_id(
    sessions: &SharedCodexSessionMap,
    turn_id: &str,
) -> Option<String> {
    sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .iter()
        .find_map(|(session_id, session_state)| {
            (session_state.turn_id.as_deref() == Some(turn_id)
                || session_state.completed_turn_id.as_deref() == Some(turn_id))
            .then(|| session_id.clone())
        })
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

/// Waits for a shared-Codex JSON-RPC response with liveness-scaled patience.
///
/// A flat per-request timeout treats two very different servers identically:
/// one that wedged quietly and one that is grinding through expensive work —
/// a `thread/resume` whose rollout runs tens-to-hundreds of MB, queued behind
/// another session's streaming turn. The first deserves the fast failure; the
/// second just needs more time, and failing it is actively harmful: the
/// abandoned request's work still completes server-side, and the retry queues
/// the same expensive replay on top of the very contention that caused the
/// miss (observed live: a ~39MB resume failing at exactly 180s, twice, while
/// a ~170MB sibling thread streamed — tm-bmd.1).
///
/// So instead of one `recv_timeout(limit)`, this waits in `poll_slice` steps
/// and consults the stdout activity stamp on each miss:
///
///   * Server silent for `silence_limit` and counting → give up now. This is
///     the old flat-timeout behaviour to the second, and the downstream
///     `handle_shared_codex_startup_response_error` will observe the same
///     silence and retire the wedge-shaped runtime.
///   * Server emitted stdout within `silence_limit` → keep waiting, up to
///     `max_wait_while_active`. Hitting the cap fails only this turn
///     downstream (the server is demonstrably alive), so a lost request
///     cannot park the session on "working" forever.
///   * Runtime slot no longer holds `runtime_id` → the probe returns `None`
///     and the wait degrades to the silent budget. Normally moot: every
///     teardown path fails pending requests, which lands here as an error on
///     the channel, not a timeout.
///
/// Only used for requests a busy server can legitimately answer late (thread
/// setup, `turn/start`). The initialize request keeps its flat window —
/// nothing else runs on a server that has not even said hello.
fn wait_for_shared_codex_response_while_server_active(
    pending_requests: &CodexPendingRequestMap,
    pending_request: PendingCodexJsonRpcRequest,
    method: &str,
    state: &AppState,
    runtime_id: &str,
    silence_limit: Duration,
    max_wait_while_active: Duration,
    poll_slice: Duration,
) -> std::result::Result<Value, CodexResponseError> {
    let PendingCodexJsonRpcRequest {
        request_id,
        response_rx,
    } = pending_request;
    let started = std::time::Instant::now();

    loop {
        match response_rx.recv_timeout(poll_slice) {
            Ok(response) => return response,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                pending_requests
                    .lock()
                    .expect("Codex pending requests mutex poisoned")
                    .remove(&request_id);
                return Err(CodexResponseError::Transport(format!(
                    "Codex app-server response channel closed while waiting for `{method}`"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let elapsed = started.elapsed();
        let server_recently_active = state
            .shared_codex_stdout_silence_if_matches(runtime_id)
            .is_some_and(|silence| silence < silence_limit);
        let deadline = if server_recently_active {
            max_wait_while_active
        } else {
            silence_limit
        };
        if elapsed >= deadline {
            pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&request_id);
            return Err(CodexResponseError::Timeout(format!(
                "timed out waiting for Codex app-server response to `{method}` after {}s",
                elapsed.as_secs()
            )));
        }
    }
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
    writer_context: Option<&SharedCodexStdinContextState>,
    session_id: &str,
    command: CodexPromptCommand,
) -> Result<()> {
    const SHARED_CODEX_THREAD_SETUP_TIMEOUT: Duration = Duration::from_secs(180);

    // Decide and commit in ONE critical section. Reading the session state and
    // then acting on it under a second lock is a check-then-act race: the waiter
    // can complete in between, take the parked prompt, and start its turn — and
    // the command parked afterwards is then owned by nobody (the fast path never
    // reads the parked prompt, and the next setup overwrites it), so the user's
    // prompt vanishes with no error.
    //
    // The session must always be observably in exactly one of three states:
    // `{no thread, no setup}`, `{setup in flight}`, `{thread bound}`. Every
    // transition happens under this lock or the waiter's (see
    // `complete_shared_codex_thread_setup`).
    let request_id = Uuid::new_v4().to_string();
    let decision = {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions.entry(session_id.to_owned()).or_default();
        // Test the in-flight setup BEFORE `thread_id`. The `thread/started`
        // notification can land ahead of the `thread/start` response and set
        // `thread_id` while the setup is still pending (`codex_events.rs`), so a
        // session really can be in both states at once. Checking `thread_id`
        // first would let this prompt run immediately and the parked one run
        // again afterwards — two turns from one setup.
        match session_state.pending_thread_setup.as_mut() {
            // A setup is in flight: ALWAYS park. Never start a second one.
            //
            // Parking cannot inherit the wrong thread, because a prompt can never
            // arrive wanting a different thread while a setup is still pending:
            //
            //   * `resume_thread_id` comes from `record.external_session_id`
            //     (`turn_dispatch.rs`). While a setup is in flight and the session
            //     is attached, the only thing that writes that field is this very
            //     setup — via the `thread/started` notification or its own response
            //     — so it can only ever become THIS setup's thread.
            //   * Anything that would give the session a *different* thread identity
            //     goes through stop/detach, and `interrupt_and_detach` calls
            //     `detach()` UNCONDITIONALLY (`session_runtime.rs`) — even when the
            //     interrupt itself fails. `detach()` removes the whole shared-session
            //     entry, and with it this setup. There is nothing left to park on.
            //     (`try_detach` is the one detach that can decline: it returns `false`
            //     on `WouldBlock` instead of blocking. Not a counterexample — it runs
            //     only from natural turn-completion cleanup, where the turn has already
            //     run, so no setup can be in flight for it to strand.)
            //
            // Earlier revisions compared thread identities here and superseded on a
            // mismatch. That machinery was guarding an unreachable state, and it is
            // what produced the redundant `thread/resume` + suppression of the
            // session's own LIVE thread. Not comparing is not a shortcut; it is the
            // invariant.
            //
            // Its waiter runs whatever is parked, so the newest command still wins —
            // exactly what happened before, when superseded waiters bailed out and
            // dropped their turns — while the app-server only ever creates one thread.
            Some(setup) => {
                setup.command = command;
                CodexThreadSetupDecision::Parked
            }
            None => {
                if let Some(thread_id) = session_state.thread_id.clone() {
                    // Fast path: thread already bound and no setup pending.
                    CodexThreadSetupDecision::StartTurn(thread_id, command)
                } else {
                    // Cloned before the command is MOVED into the slot below. Cloning
                    // the whole `CodexPromptCommand` instead would copy its prompt text
                    // and its base64 image attachments and then drop the original;
                    // these are two short `String`s, an `Option<String>`, and two `Copy`
                    // enums, and only this arm pays for them.
                    let request = CodexThreadSetupRequest {
                        approval_policy: command.approval_policy,
                        cwd: command.cwd.clone(),
                        model: command.model.clone(),
                        resume_thread_id: command.resume_thread_id.clone(),
                        sandbox_mode: command.sandbox_mode,
                    };
                    // Claim the slot BEFORE the request goes out, so a command
                    // arriving while we are still writing to stdin parks instead of
                    // racing us into another `thread/start`.
                    session_state.pending_thread_setup = Some(PendingCodexThreadSetup {
                        request_id: request_id.clone(),
                        command,
                    });
                    CodexThreadSetupDecision::StartSetup(request)
                }
            }
        }
    };

    let setup_request = match decision {
        // Fast path: thread already exists, go straight to turn/start.
        CodexThreadSetupDecision::StartTurn(thread_id, command) => {
            return handle_shared_codex_start_turn(
                writer,
                pending_requests,
                state,
                runtime_id,
                sessions,
                writer_context,
                session_id,
                &thread_id,
                command,
            );
        }
        CodexThreadSetupDecision::Parked => {
            // Coalescing: the prompt already parked here never reaches Codex. That
            // matches the old behaviour (superseded waiters dropped their turns too),
            // but it is a deliberate design point now rather than an accident, so say
            // so out loud — a silent drop is what made the original "one prompt, eight
            // threads" incident so hard to see. (`tm-xx3` tracks giving the user a
            // real signal instead of a stderr line.)
            //
            // Logged out here, not in the arm above: nothing writes to stderr while
            // holding the shared-session lock, which every Codex event contends on.
            eprintln!(
                "shared codex> session `{session_id}` already has a thread setup in \
                 flight; the newer prompt replaces the one parked on it"
            );
            return Ok(());
        }
        CodexThreadSetupDecision::StartSetup(request) => request,
    };

    // Slow path: need to create or resume a thread first. Fire-and-forget the
    // setup request and spawn a waiter so the writer thread is not blocked.
    //
    // The slot is claimed. From here until the request is actually on the wire, EVERY
    // early return must release it — one that walks past the release leaves the
    // session wedged in `{setup in flight}`: the setup never completes, and every
    // later prompt parks behind it forever. This used to be a hand-written abort in
    // each failure arm, i.e. a rule you had to remember at each new `?`; one of the
    // two arms was in fact never exercised by a test. The guard makes releasing the
    // default and holding the slot the thing you have to opt into.
    let setup_guard = PendingCodexThreadSetupGuard::new(sessions, session_id, &request_id);

    let mcp_config = state
        .termal_delegation_mcp_codex_config(session_id)
        .context("failed to build Codex delegation MCP config")?;

    // The command itself now lives in the setup slot; the request is built from the
    // parameters the decision carried back out.
    let (method, params) = match setup_request.resume_thread_id.as_deref() {
        Some(thread_id) => (
            "thread/resume",
            json!({
                "threadId": thread_id,
                // TermAl only consumes `/thread/id` from this response. Asking
                // Codex to reconstruct the full turn history can produce one
                // JSON-RPC line larger than our stdout safety cap for long,
                // compaction-heavy rollouts, which discards the response and
                // strands this setup until its timeout.
                "excludeTurns": true,
                "cwd": setup_request.cwd,
                "model": setup_request.model,
                "sandbox": setup_request.sandbox_mode.as_cli_value(),
                "approvalPolicy": setup_request.approval_policy.as_cli_value(),
                "config": mcp_config,
            }),
        ),
        None => (
            "thread/start",
            json!({
                "cwd": setup_request.cwd,
                "model": setup_request.model,
                "sandbox": setup_request.sandbox_mode.as_cli_value(),
                "approvalPolicy": setup_request.approval_policy.as_cli_value(),
                "personality": "pragmatic",
                "config": mcp_config,
            }),
        ),
    };

    set_shared_codex_writer_context(
        writer_context,
        format!("jsonrpc_request method={method} id={request_id} session={session_id}"),
    );
    let pending = start_codex_json_rpc_request_with_id(
        writer,
        pending_requests,
        request_id.clone(),
        method,
        params,
    )?;
    // The request is on the wire, so the waiter spawned below now owns the slot's
    // lifecycle: it releases it on an error response, a timeout, or a response that
    // carries no thread id, and hands it to `complete_shared_codex_thread_setup` on
    // success. Releasing it here as well would abort a setup that is genuinely live.
    setup_guard.disarm();

    let waiter_pending = pending_requests.clone();
    let waiter_state = state.clone();
    let waiter_sessions = sessions.clone();
    let waiter_thread_sessions = thread_sessions.clone();
    let waiter_runtime_id = runtime_id.to_owned();
    let waiter_session_id = session_id.to_owned();
    let waiter_input_tx = input_tx.clone();
    let waiter_method = method.to_owned();
    std::thread::spawn(move || {
        // Liveness-scaled wait: a resume replaying a large rollout behind a
        // busy sibling turn may legitimately outlast the flat window. See the
        // helper's docs for the exact extend/give-up rules.
        let result = wait_for_shared_codex_response_while_server_active(
            &waiter_pending,
            pending,
            &waiter_method,
            &waiter_state,
            &waiter_runtime_id,
            SHARED_CODEX_THREAD_SETUP_TIMEOUT,
            SHARED_CODEX_RESPONSE_MAX_WAIT_WHILE_ACTIVE,
            SHARED_CODEX_RESPONSE_POLL_SLICE,
        );

        // Suppress startup rediscovery of a newly created (`thread/start`)
        // thread whose session is already gone or superseded by the time we
        // learn its id. The Codex app-server writes the thread to disk before
        // we bind it, so an unbindable new thread would otherwise be re-imported
        // as a phantom, unlinked top-level session on the next discovery scan
        // (the delegation-child re-import leak). Resumed threads are pre-existing
        // and must stay discoverable.
        //
        // Every caller below may be suppressing a thread the session record STILL
        // CLAIMS: `set_external_session_id_if_runtime_matches` writes
        // `external_session_id` (and un-ignores the thread) BEFORE the
        // `commit_locked` that is the only thing able to fail, and `thread/started`
        // writes it unconditionally. So "suppressed" and "claimed" genuinely
        // overlap here.
        //
        // That is safe, and the reason lives in `import_discovered_codex_threads`,
        // not here: discovery looks for a record whose `external_session_id` matches
        // the thread FIRST, and when it finds one it calls
        // `allow_discovered_codex_thread` — un-ignoring the thread — before it ever
        // consults the ignore set. A thread someone still owns therefore cannot be
        // stranded on the never-rediscover list; only a genuinely unowned one stays
        // suppressed.
        //
        // This is load-bearing. Round after round, the failure mode that hurt was
        // suppressing a LIVE thread, and the thing that made it hurt was clearing the
        // record's claim on it at the same time. Do not add a rollback of
        // `external_session_id` next to a suppression without re-reading
        // `state_boot.rs`. `import_discovered_codex_threads_reclaims_a_suppressed_thread_a_session_still_owns`
        // pins the invariant.
        let suppress_orphaned_new_thread = |thread_id: &str| {
            if waiter_method == "thread/start" {
                if let Err(err) = waiter_state.suppress_orphaned_codex_thread(thread_id) {
                    eprintln!(
                        "runtime state warning> failed to suppress rediscovery of \
                         orphaned Codex thread for session `{}`: {err:#}",
                        waiter_session_id
                    );
                }
            }
        };

        match result {
            Ok(setup_result) => {
                // Peek WITHOUT clearing. The in-flight marker has to survive until
                // the thread is actually bound below: clearing it here left the
                // session in a `{no thread, no setup}` limbo across the persist
                // call, and a prompt landing in that gap saw neither state, took
                // the slow path, and made the app-server mint a second thread.
                if !shared_codex_thread_setup_is_current(
                    &waiter_sessions,
                    &waiter_session_id,
                    &request_id,
                ) {
                    // Detached, stopped, or superseded by a newer setup request
                    // before this response arrived. The slot belongs to that newer
                    // setup now, so leave it alone and just disown this thread.
                    if let Some(orphan_thread_id) =
                        setup_result.pointer("/thread/id").and_then(Value::as_str)
                    {
                        suppress_orphaned_new_thread(orphan_thread_id);
                    }
                    return;
                }

                let thread_id = match setup_result.pointer("/thread/id").and_then(Value::as_str) {
                    Some(id) => id.to_owned(),
                    None => {
                        // Only fail the turn if the setup was still OURS. If the slot
                        // has moved on, the session's current turn belongs to somebody
                        // else and failing it would kill their work.
                        if matches!(
                            abort_shared_codex_thread_setup(
                                &waiter_sessions,
                                &waiter_session_id,
                                &request_id,
                            ),
                            CodexThreadSetupAbort::Released
                        ) {
                            let _ = waiter_state.fail_turn_if_runtime_matches(
                                &waiter_session_id,
                                &RuntimeToken::Codex(waiter_runtime_id),
                                "Codex app-server did not return a thread id",
                            );
                        }
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
                        // thread setup — discard the stale result and suppress
                        // rediscovery of the orphaned new thread. Release the setup
                        // slot too, so a later prompt is free to start a fresh one
                        // and never inherits this dead command.
                        // The session is gone either way, so the thread is an orphan
                        // regardless of who owns the slot now.
                        let _ = abort_shared_codex_thread_setup(
                            &waiter_sessions,
                            &waiter_session_id,
                            &request_id,
                        );
                        suppress_orphaned_new_thread(&thread_id);
                        return;
                    }
                    Err(err) => {
                        eprintln!(
                            "runtime state warning> failed to persist shared Codex thread \
                             registration for session `{}`: {err:#}",
                            waiter_session_id
                        );
                        // Disown the thread, like every sibling branch.
                        //
                        // Be precise about what failed: only `commit_locked` can Err
                        // here, and it runs AFTER the record was already given this
                        // thread id in memory. So the record does claim it — the claim
                        // just may never reach disk. If it does reach disk (a later
                        // commit succeeds), discovery reclaims the thread for that
                        // record and this suppression is undone; see the closure above.
                        // If it never does (the process dies first), nothing claims the
                        // thread and the suppression is what keeps it from coming back
                        // as a phantom top-level session — though on a genuinely full
                        // disk the suppression will not have persisted either.
                        //
                        // So this is insurance, not a guarantee, and it is consistent
                        // with the other three branches rather than the odd one out.
                        suppress_orphaned_new_thread(&thread_id);
                        // As above: only fail the turn if the setup was still ours.
                        if !matches!(
                            abort_shared_codex_thread_setup(
                                &waiter_sessions,
                                &waiter_session_id,
                                &request_id,
                            ),
                            CodexThreadSetupAbort::Released
                        ) {
                            return;
                        }
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

                // Bind the thread, take the parked prompt, and only THEN clear the
                // in-flight marker — all in one critical section, so the session is
                // never observably `{no thread, no setup}`. Run the newest prompt
                // handed to this setup; commands that arrived while the request was
                // in flight replaced the parked one rather than starting a second
                // thread, so this is where they get their turn.
                let command = match complete_shared_codex_thread_setup(
                    &waiter_sessions,
                    &waiter_thread_sessions,
                    &waiter_session_id,
                    &request_id,
                    &thread_id,
                ) {
                    CodexThreadSetupCompletion::Completed(parked) => parked,
                    CodexThreadSetupCompletion::Superseded => {
                        // The session was detached or reset while we waited. Since a
                        // prompt never supersedes an in-flight setup (it parks), that
                        // is the only way to get here. Reachable on a plain Stop:
                        // `stop_session` detaches from the shared map before it retakes
                        // the state lock to clear `record.runtime`, so a waiter landing
                        // in that window still gets `Applied` above and lands here.
                        //
                        // Deliberately NO rollback of `external_session_id`. An earlier
                        // revision cleared it, and that is exactly what turned a
                        // suppression into a bug: a thread that is both unclaimed AND
                        // suppressed is stranded forever. Leaving the record's claim
                        // intact is what lets discovery reclaim the thread (see the
                        // closure above) — and `stop_session` deliberately preserves
                        // `external_session_id` precisely so the thread stays resumable.
                        // (`kill_session` is not the owner here: it deletes the record
                        // outright.)
                        suppress_orphaned_new_thread(&thread_id);
                        return;
                    }
                };

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
            Err(err) => {
                handle_shared_codex_thread_setup_response_error_if_current(
                    &waiter_sessions,
                    &waiter_state,
                    &waiter_runtime_id,
                    &waiter_session_id,
                    &request_id,
                    SHARED_CODEX_THREAD_SETUP_TIMEOUT,
                    err,
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
    writer_context: Option<&SharedCodexStdinContextState>,
    session_id: &str,
    thread_id: &str,
    command: CodexPromptCommand,
) -> Result<()> {
    const SHARED_CODEX_TURN_START_TIMEOUT: Duration = Duration::from_secs(120);
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    let request_id = Uuid::new_v4().to_string();

    // A setup in flight here means this is a STALE `StartTurnAfterSetup` hand-off and
    // the session has re-armed underneath it. Abandon it, touching nothing.
    //
    // Why a setup can be here at all — the earlier claim that it could not was wrong,
    // and it was wrong in an expensive way, so state the real shape:
    //
    //   * The waiter clears the setup slot in `complete_shared_codex_thread_setup` and
    //     only THEN sends the hand-off. The writer runs freely in that gap, so a detach
    //     plus a fresh prompt can land in between — and the fresh prompt, finding no
    //     thread and no setup, claims a NEW one.
    //   * Serializing prompt handling and turn start on the writer thread does not help.
    //     The hand-off is enqueued by a WAITER; writer serialization says nothing about
    //     what a waiter puts on the queue.
    //   * The runtime-id check below does not catch it either: every session on the
    //     shared app-server carries the same `runtime_id` (cloned from
    //     `SharedCodexRuntime` in `spawn_codex_runtime`), so detach + re-attach yields
    //     the SAME id and the check returns `Applied`. It is a PROCESS check, not an
    //     ATTACHMENT check.
    //
    // This is checked BEFORE the record is touched so a stale hand-off cannot persist
    // the detached attachment's sandbox/approval/effort onto the re-armed session
    // either. It runs on the writer thread and only the writer claims setups, so the
    // slot cannot fill behind us between here and the block below.
    //
    // The prompt-command fast path never reaches this branch: it decides `StartTurn`
    // only when the slot is empty, and parks otherwise.
    //
    // NOTE: the thread this stale hand-off was carrying is now unowned — nothing
    // suppresses it, so discovery can re-import it. That is the deeper attachment-
    // generation gap (`tm-nqc` and its follow-up), not something this bail can fix.
    let superseding_setup_request_id = {
        let sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions
            .get(session_id)
            .and_then(|session_state| session_state.pending_thread_setup.as_ref())
            .map(|setup| setup.request_id.clone())
    };
    if let Some(superseding_setup_request_id) = superseding_setup_request_id {
        eprintln!(
            "shared codex> abandoning a stale turn hand-off for session `{session_id}` \
             (thread `{thread_id}`): the session re-armed with thread setup \
             `{superseding_setup_request_id}` while the hand-off was in flight"
        );
        return Ok(());
    }

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
        // `clear_shared_codex_turn_session_state` clears `pending_thread_setup` — which
        // owns a user's PROMPT, not just a marker. It cannot be eating one here: we
        // returned early above if a setup was in flight, and only this (writer) thread
        // ever claims one, so the slot cannot have filled behind us.
        //
        // That early bail is the ONLY thing standing between this line and destroying a
        // prompt the user just typed. An earlier revision relied on a `debug_assert!`
        // and a claim that the writer thread's serialization made this unreachable. It
        // was reachable — `stale_start_turn_handoff_leaves_the_setup_that_re_armed_the_session_alone`
        // is the test that proves it. Do not reintroduce that reasoning.
        session_state.thread_id = Some(thread_id.to_owned());
        clear_shared_codex_turn_session_state(session_state);
        session_state.pending_turn_start_request_id = Some(request_id.clone());
        session_state.turn_started = false;
    }

    set_shared_codex_writer_context(
        writer_context,
        format!(
            "jsonrpc_request method=turn/start id={request_id} session={session_id} thread={thread_id}"
        ),
    );
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
        // Same liveness-scaled wait as thread setup: a busy server can ack
        // `turn/start` late without being dead, and the ack mattering less
        // than the events does not make a spurious runtime-wide failure okay.
        let result = wait_for_shared_codex_response_while_server_active(
            &wait_pending_requests,
            pending_turn_request,
            "turn/start",
            &wait_state,
            &wait_runtime_id,
            SHARED_CODEX_TURN_START_TIMEOUT,
            SHARED_CODEX_RESPONSE_MAX_WAIT_WHILE_ACTIVE,
            SHARED_CODEX_RESPONSE_POLL_SLICE,
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
            Err(CodexResponseError::JsonRpc(detail)) => {
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
            Err(CodexResponseError::Timeout(detail)) => {
                let should_fail_runtime = {
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
                if should_fail_runtime {
                    handle_shared_codex_startup_response_error(
                        &wait_state,
                        &wait_runtime_id,
                        &wait_session_id,
                        SHARED_CODEX_TURN_START_TIMEOUT,
                        CodexResponseError::Timeout(detail),
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

/// `response_timeout` is the silence budget the timed-out request waited under
/// (`SHARED_CODEX_THREAD_SETUP_TIMEOUT` / `SHARED_CODEX_TURN_START_TIMEOUT` —
/// the liveness-scaled waiter may have stayed longer, but only while the
/// server kept talking); the `Timeout` arm compares it against the server's
/// stdout silence to pick between failing one turn and retiring the runtime.
fn handle_shared_codex_startup_response_error(
    state: &AppState,
    runtime_id: &str,
    session_id: &str,
    response_timeout: Duration,
    err: CodexResponseError,
) {
    match err {
        CodexResponseError::JsonRpc(detail) => {
            if let Err(err) = state.fail_turn_if_runtime_matches(
                session_id,
                &RuntimeToken::Codex(runtime_id.to_owned()),
                &detail,
            ) {
                eprintln!(
                    "runtime state warning> failed to mark shared Codex turn error for session `{session_id}`: {err:#}"
                );
            }
        }
        CodexResponseError::Timeout(detail) => {
            // A response timeout on its own does not prove the server is gone.
            // Dead servers are detected independently of these per-request
            // timeouts — the stdout reader's EOF path and the `wait()` thread
            // fail all pending requests and retire the runtime the moment the
            // process actually dies — so a timeout only ever fires against a
            // process that is still running. Two very different states look
            // identical from the unanswered request alone:
            //
            //   * BUSY: the server is grinding through expensive work (e.g.
            //     resuming a thread whose rollout is tens or hundreds of MB
            //     while another session's turn streams). Tearing down here
            //     kills every sibling session's in-flight turn AND forces the
            //     replacement server to re-parse the same rollouts from
            //     scratch — which is precisely the condition that produced
            //     the timeout, so the teardown seeds its own repeat.
            //   * WEDGED: the event loop is stuck. Keeping the runtime would
            //     route every later Codex session into the same dead process,
            //     and nothing else would ever retire it (the stdin watchdog
            //     only covers blocked WRITES).
            //
            // The stdout reader's activity stamp separates them: a server
            // that emitted ANYTHING during this request's wait window is
            // alive and merely slow — fail only this turn and leave the
            // runtime (and every other session's turn) alone. A server that
            // was silent for the entire window is wedge-shaped — keep the
            // old teardown so it cannot poison later sessions. When the
            // runtime slot no longer holds this runtime the probe returns
            // `None` and the teardown path is a token-guarded no-op cascade,
            // matching the pre-probe behaviour.
            //
            // A late response from a server left alive is harmless: the
            // pending entry was removed on timeout and unknown-id responses
            // are dropped (`codex_events.rs`).
            let server_spoke_during_wait = state
                .shared_codex_stdout_silence_if_matches(runtime_id)
                .is_some_and(|silence| silence < response_timeout);
            if server_spoke_during_wait {
                fail_shared_codex_turn_without_runtime_exit(
                    state,
                    session_id,
                    runtime_id,
                    &format!(
                        "{detail}; the shared app-server is still emitting events for \
                         other sessions, so only this turn was abandoned — send the \
                         prompt again to retry"
                    ),
                    "shared Codex response timeout on a busy app-server",
                );
            } else if let Err(err) = state.handle_shared_codex_runtime_exit(
                runtime_id,
                Some(&shared_codex_runtime_command_error_detail(&anyhow!(detail))),
            ) {
                eprintln!(
                    "runtime state warning> failed to tear down shared Codex runtime for session `{session_id}`: {err:#}"
                );
            }
        }
        CodexResponseError::Transport(detail) => {
            if let Err(err) = state.handle_shared_codex_runtime_exit(
                runtime_id,
                Some(&shared_codex_runtime_command_error_detail(&anyhow!(detail))),
            ) {
                eprintln!(
                    "runtime state warning> failed to tear down shared Codex runtime for session `{session_id}`: {err:#}"
                );
            }
        }
    }
}

/// What a prompt should do about this session's Codex thread, decided in a
/// single critical section so the session can never be caught mid-transition.
enum CodexThreadSetupDecision {
    /// `{thread bound}` — go straight to `turn/start`.
    ///
    /// Carries the prompt back out so the other arms can MOVE it into the setup
    /// instead of cloning: a `CodexPromptCommand` owns its prompt text and its
    /// base64 image attachments, and parking used to copy all of that and then
    /// drop the original.
    StartTurn(String, CodexPromptCommand),
    /// `{setup in flight}` — the prompt was parked on it; its waiter will run it.
    Parked,
    /// `{no thread, no setup}` — we claimed the setup slot and must now fire it.
    ///
    /// Carries what the setup REQUEST needs, cloned out of the command inside the one
    /// arm that actually uses it. The command itself has been moved into the slot by
    /// then, so these cannot be read back off it — and cloning them *before* the
    /// decision, as this used to, made every prompt on the `StartTurn` fast path pay
    /// for two `String`s it immediately threw away.
    StartSetup(CodexThreadSetupRequest),
}

/// The parameters of the `thread/start` / `thread/resume` request itself.
///
/// These come from the prompt that OPENED the setup. The turn that eventually runs
/// uses the PARKED command's parameters instead — see `PendingCodexThreadSetup`.
/// These shape the thread; that shapes the turn.
struct CodexThreadSetupRequest {
    approval_policy: CodexApprovalPolicy,
    cwd: String,
    model: String,
    resume_thread_id: Option<String>,
    sandbox_mode: CodexSandboxMode,
}

/// Reports whether `request_id` is still this session's current thread setup.
///
/// Peeks WITHOUT clearing. The in-flight marker must survive until the thread is
/// actually bound: a session observed as neither `{setup in flight}` nor
/// `{thread bound}` is free to start a new setup, which is exactly how one prompt
/// used to make the app-server mint several threads.
fn shared_codex_thread_setup_is_current(
    sessions: &SharedCodexSessionMap,
    session_id: &str,
    request_id: &str,
) -> bool {
    let sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    sessions.get(session_id).is_some_and(|session_state| {
        session_state
            .pending_thread_setup
            .as_ref()
            .is_some_and(|setup| setup.request_id == request_id)
    })
}

/// Outcome of releasing an in-flight Codex thread setup.
///
/// `#[must_use]`: discarding this silently assumes the setup was still ours. A
/// caller that then fails "the session's turn" can be failing a turn that now
/// belongs to somebody else.
#[must_use]
enum CodexThreadSetupAbort {
    /// The setup was ours: it is released and any prompt parked on it is dropped.
    Released,
    /// The slot has already moved on to a different setup. Nothing was touched.
    NotCurrent,
}

/// Releases an in-flight thread setup and drops the prompt parked on it.
///
/// For setups that can never complete: the request never went out, the
/// app-server errored or timed out, or the response carried no thread id.
/// Clearing the marker and dropping the parked prompt in the SAME critical
/// section keeps the state machine honest — a later prompt must find
/// `{no thread, no setup}` and be free to start a fresh setup, and must never
/// inherit a command belonging to a dead one.
fn abort_shared_codex_thread_setup(
    sessions: &SharedCodexSessionMap,
    session_id: &str,
    request_id: &str,
) -> CodexThreadSetupAbort {
    let mut sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let Some(session_state) = sessions.get_mut(session_id) else {
        return CodexThreadSetupAbort::NotCurrent;
    };
    let is_current = session_state
        .pending_thread_setup
        .as_ref()
        .is_some_and(|setup| setup.request_id == request_id);
    if !is_current {
        return CodexThreadSetupAbort::NotCurrent;
    }
    // Drops the parked prompt with the setup — they are one value precisely so a
    // later setup cannot inherit a command belonging to a dead one.
    session_state.pending_thread_setup = None;
    CodexThreadSetupAbort::Released
}

/// Releases a claimed thread-setup slot unless the request reached the wire.
///
/// Guards the window in `handle_shared_codex_prompt_command` between claiming the
/// slot and the setup request actually going out. Every early return in that window
/// — including a `?` nobody has written yet — must release the slot, or the session
/// wedges in `{setup in flight}` and parks every later prompt behind a setup that
/// can never fire.
///
/// This exists because "release the slot in each failure arm" is a rule a human has
/// to keep, and we did not keep it: of the two hand-written aborts it replaces, the
/// one on the MCP-config path was exercised by no test at all — deleting it left the
/// suite green. A `Drop` cannot be forgotten by the next `?`, and it is exercised by
/// every test that fails a setup before the write.
///
/// Deliberately NOT used for post-write failures. Once the request is on the wire the
/// waiter owns the slot, and releasing it from here would abort a live setup — which
/// is why the disarm is explicit rather than implied by reaching the end of the
/// function.
struct PendingCodexThreadSetupGuard<'a> {
    armed: bool,
    request_id: &'a str,
    session_id: &'a str,
    sessions: &'a SharedCodexSessionMap,
}

impl<'a> PendingCodexThreadSetupGuard<'a> {
    fn new(
        sessions: &'a SharedCodexSessionMap,
        session_id: &'a str,
        request_id: &'a str,
    ) -> Self {
        Self {
            armed: true,
            request_id,
            session_id,
            sessions,
        }
    }

    /// The request is in flight; the waiter owns the slot from here.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingCodexThreadSetupGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The caller is returning `Err` either way; the slot just must not stay
        // claimed. `NotCurrent` is fine and expected — a detach can have taken the
        // whole session out from under us while we were failing.
        let _ = abort_shared_codex_thread_setup(self.sessions, self.session_id, self.request_id);
    }
}

/// Outcome of completing an in-flight Codex thread setup.
enum CodexThreadSetupCompletion {
    /// Thread bound. Carries the prompt parked on the setup — the newest one
    /// handed to it, not necessarily the one that opened it.
    ///
    /// Not an `Option`: a setup always owns its prompt, so there is no "fall back
    /// to the command that opened the setup" case. That fallback would answer the
    /// *older* prompt, silently — the exact failure this change exists to prevent.
    Completed(CodexPromptCommand),
    /// The session was detached, stopped, or rebound while the response was in
    /// flight. The thread belongs to nobody; the caller must disown it.
    Superseded,
}

/// Binds `thread_id` to the session, takes the parked prompt, and clears the
/// in-flight marker — in ONE critical section.
///
/// Ordering here is the whole point. The marker is cleared *last*, after the
/// thread is bound, so the session steps straight from `{setup in flight}` to
/// `{thread bound}` with no observable gap: previously the marker was cleared
/// first and `thread_id` set only after a persist round-trip, and a prompt
/// landing in that window saw neither and started a second thread.
///
/// The setup — and with it the parked prompt — is taken *before*
/// `clear_shared_codex_turn_session_state` runs, so this does not depend on that
/// routine's field-by-field behaviour. It has no exhaustiveness guard, and it does
/// clear `pending_thread_setup`; taking the prompt first means a reordering there
/// cannot make the waiter answer the *older* prompt.
fn complete_shared_codex_thread_setup(
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    session_id: &str,
    request_id: &str,
    thread_id: &str,
) -> CodexThreadSetupCompletion {
    let (previous_thread_id, parked_command) = {
        let mut sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let Some(session_state) = sessions.get_mut(session_id) else {
            return CodexThreadSetupCompletion::Superseded;
        };
        let is_current = session_state
            .pending_thread_setup
            .as_ref()
            .is_some_and(|setup| setup.request_id == request_id);
        if !is_current {
            return CodexThreadSetupCompletion::Superseded;
        }
        // Take the setup (and with it the parked prompt) BEFORE the per-turn reset:
        // `clear_shared_codex_turn_session_state` also clears the setup, and it has
        // no exhaustiveness guard, so relying on its field-by-field behaviour is a
        // trap. Clearing the setup here is also what closes the in-flight window —
        // it happens only now, after `thread_id` is bound below, so the session
        // steps straight from `{setup in flight}` to `{thread bound}` with no gap
        // for another prompt to start a second thread in.
        let setup = session_state
            .pending_thread_setup
            .take()
            .expect("setup presence checked above");
        clear_shared_codex_turn_session_state(session_state);
        let previous_thread_id = session_state.thread_id.replace(thread_id.to_owned());
        (previous_thread_id, setup.command)
    };

    let mut thread_sessions = thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned");
    if let Some(previous_thread_id) = previous_thread_id {
        if previous_thread_id != thread_id {
            thread_sessions.remove(&previous_thread_id);
        }
    }
    thread_sessions.insert(thread_id.to_owned(), session_id.to_owned());

    CodexThreadSetupCompletion::Completed(parked_command)
}

/// Applies thread-setup failures only for the currently pending setup
/// request, so stale waiters from a stopped/restarted session cannot retire
/// a newer turn on the same shared Codex app-server.
fn handle_shared_codex_thread_setup_response_error_if_current(
    sessions: &SharedCodexSessionMap,
    state: &AppState,
    runtime_id: &str,
    session_id: &str,
    request_id: &str,
    response_timeout: Duration,
    err: CodexResponseError,
) {
    // Release the setup and drop the prompt parked on it in one step: the setup
    // this prompt was waiting for has failed, so a later prompt must be free to
    // start a fresh one and must not inherit this dead command.
    if matches!(
        abort_shared_codex_thread_setup(sessions, session_id, request_id),
        CodexThreadSetupAbort::NotCurrent
    ) {
        return;
    }
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    if !state.session_matches_runtime_token(session_id, &runtime_token) {
        return;
    }
    handle_shared_codex_startup_response_error(state, runtime_id, session_id, response_timeout, err);
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
