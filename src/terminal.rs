// Terminal command execution — HTTP handlers (`run_terminal_command`,
// `run_terminal_command_stream`), synchronous + streaming shell spawners,
// Windows job-object / Unix process-group lifecycle, SSE event framing,
// and the shared terminal output buffer with UTF-8 safe streaming.
//
// Extracted from api.rs into its own `include!()` fragment so the terminal
// subsystem lives in one place. The crate still compiles as one flat
// module, so no visibility changes are required.

/// Runs a terminal command.
async fn run_terminal_command(
    State(state): State<AppState>,
    Json(request): Json<TerminalCommandRequest>,
) -> Result<Json<TerminalCommandResponse>, ApiError> {
    let command = validate_terminal_command(&request.command)?;
    let workdir_request = validate_terminal_workdir(&request.workdir)?;
    // Phase 1 delegated children are local-only, so this cheap session gate
    // catches child writes before remote routing. Project/workdir scope checks
    // still run after routing decides the request is local.
    state.ensure_read_only_delegation_allows_session_write_action(
        request.session_id.as_deref(),
        "terminal commands",
    )?;

    // Peek whether this request will resolve to a remote scope using only
    // in-memory state (no network I/O). We must acquire the 429 permit
    // *before* calling the full `remote_scope_for_request`, because inside
    // that call `ensure_remote_project_binding` can issue an unbounded
    // `POST /api/projects` to bind a first-time project — which would
    // otherwise escape the rate limit on a burst of first-time remote
    // terminal requests.
    //
    // ACCEPTED RACE: `terminal_request_is_remote` and the subsequent
    // `remote_scope_for_request` each take `state.inner` independently,
    // with a gap between them for the permit acquisition and the move
    // onto the blocking pool. If a concurrent request mutates the
    // project's remote binding in that window (bind → unbind, or vice
    // versa), the permit we charge here can belong to the "wrong" budget
    // relative to the scope that actually runs. Both sides fail closed
    // (the mismatched request returns safely with an `ApiError`, and
    // `remote_scope_for_request` returning `None` after a positive peek
    // produces an internal error rather than silently re-routing the
    // command to the local path), so the only observable effect is a
    // transient asymmetry in the local-vs-remote 429 counters. The race
    // requires a concurrent binding mutation to be reachable, and
    // closing it would mean reintroducing blocking I/O on the async
    // worker thread (undoing the round-99 fix) — not worth the
    // complexity. Documented here so a future reader tracing the
    // counters does not chase this as a bug.
    if state.terminal_request_is_remote(
        request.session_id.as_deref(),
        request.project_id.as_deref(),
    ) {
        let permit = state
            .terminal_remote_command_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "too many remote terminal commands are already running; limit is {TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT}"
                    ),
                )
            })?;
        // Run the full scope resolution AND the terminal proxy call inside
        // `run_blocking_api` (i.e. on the blocking pool). `remote_scope_for_request`
        // can reach `ensure_remote_project_binding`, which sends a
        // synchronous blocking `reqwest` POST to `/api/projects` on a
        // first-time-bound remote project; resolving it on the async
        // worker thread would pin Tokio workers for up to
        // `REMOTE_TERMINAL_COMMAND_TIMEOUT` per first-time remote terminal
        // request. The permit + `terminal_request_is_remote` peek above
        // already bound us to the remote concurrency cap before we enter
        // the blocking pool, so the move is a pure async-safety win.
        let response: Result<TerminalCommandResponse, ApiError> = run_blocking_api(move || {
            let _permit = permit;
            let scope = state
                .remote_scope_for_request(
                    request.session_id.as_deref(),
                    request.project_id.as_deref(),
                )?
                .ok_or_else(|| {
                    ApiError::internal(
                        "remote scope resolution returned none after in-memory peek indicated remote",
                    )
                })?;
            // `scope` is borrowed — not moved — into
            // `remote_post_json_with_timeout` below. We still clone
            // `scope.remote.name` here so the `map_err` closure can build
            // the prefixed 429 message without re-borrowing `scope` across
            // the closure boundary. Without the prefix, a user submitting
            // a remote-scoped terminal command would see the raw "too
            // many local terminal commands..." message from the remote
            // host's own local semaphore, which is misleading from their
            // perspective (the command never touched anything local on
            // their side).
            let remote_name = scope.remote.name.clone();
            state
                .remote_post_json_with_timeout(
                    &scope,
                    "/api/terminal/run",
                    json!({
                        "command": command,
                        "workdir": workdir_request,
                    }),
                    REMOTE_TERMINAL_COMMAND_TIMEOUT,
                )
                .map_err(|err| annotate_remote_terminal_429(err, &remote_name))
        })
        .await;
        return response.map(Json);
    }

    let permit = state
        .terminal_local_command_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                format!(
                    "too many local terminal commands are already running; limit is {TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT}"
                ),
            )
        })?;
    let response = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        state.ensure_read_only_delegation_allows_write_action(
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            Some(&workdir_request),
            "terminal commands",
        )?;
        let workdir = resolve_project_scoped_requested_path(
            &state,
            request.session_id.as_deref(),
            request.project_id.as_deref(),
            &workdir_request,
            ScopedPathMode::ExistingPath,
        )?;
        run_terminal_shell_command(&command, &workdir)
    })
    .await
    .map_err(|err| ApiError::internal(format!("terminal command task failed: {err}")))??;
    Ok(Json(response))
}

/// Streams a terminal command, emitting stdout/stderr chunks before the final result.
async fn run_terminal_command_stream(
    State(state): State<AppState>,
    Json(request): Json<TerminalCommandRequest>,
) -> Result<Sse<impl futures_core::Stream<Item = std::result::Result<Event, Infallible>>>, ApiError>
{
    let command = validate_terminal_command(&request.command)?;
    let workdir_request = validate_terminal_workdir(&request.workdir)?;
    // Phase 1 delegated children are local-only, so this cheap session gate
    // catches child writes before remote routing. Project/workdir scope checks
    // still run after routing decides the request is local.
    state.ensure_read_only_delegation_allows_session_write_action(
        request.session_id.as_deref(),
        "terminal commands",
    )?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = TerminalStreamCancelGuard {
        cancellation: cancellation.clone(),
    };
    let (event_tx, event_rx) =
        tokio::sync::mpsc::channel::<TerminalCommandStreamEvent>(
            TERMINAL_STREAM_EVENT_QUEUE_CAPACITY,
        );

    if state.terminal_request_is_remote(
        request.session_id.as_deref(),
        request.project_id.as_deref(),
    ) {
        let permit = state
            .terminal_remote_command_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "too many remote terminal commands are already running; limit is {TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT}"
                    ),
                )
            })?;
        let scope_state = state.clone();
        let session_id = request.session_id.clone();
        let project_id = request.project_id.clone();
        let scope = run_blocking_api(move || {
            scope_state
                .remote_scope_for_request(session_id.as_deref(), project_id.as_deref())?
                .ok_or_else(|| {
                    ApiError::internal(
                        "remote scope resolution returned none after in-memory peek indicated remote",
                    )
                })
        })
        .await?;
        let task_tx = event_tx.clone();
        let task_cancellation = cancellation.clone();
        spawn_terminal_stream_worker(event_tx.clone(), async move {
            let remote_stream_tx = task_tx.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let remote_name = scope.remote.name.clone();
                let payload = json!({
                    "command": command,
                    "workdir": workdir_request,
                });
                // Remote streamed terminals also intentionally avoid a response
                // timeout so long-running user commands can keep producing
                // output for as long as the remote backend allows.
                let response = state.remote_post_response_without_timeout(
                    &scope,
                    "/api/terminal/run/stream",
                    payload.clone(),
                )?;
                if matches!(
                    response.status(),
                    StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                )
                {
                    return state
                        .remote_post_json_with_timeout(
                            &scope,
                            "/api/terminal/run",
                            payload,
                            REMOTE_TERMINAL_COMMAND_TIMEOUT,
                        )
                        .map_err(|err| annotate_remote_terminal_429(err, &remote_name));
                }

                forward_remote_terminal_stream_response(
                    response,
                    &remote_stream_tx,
                    &task_cancellation,
                )
                    .map_err(|err| annotate_remote_terminal_429(err, &remote_name))
            })
            .await
            .map_err(|err| ApiError::internal(format!("terminal command task failed: {err}")))
            .and_then(|result| result);
            send_terminal_stream_result(&task_tx, result).await;
        });
    } else {
        let permit = state
            .terminal_local_command_semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::from_status(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!(
                        "too many local terminal commands are already running; limit is {TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT}"
                    ),
                )
            })?;
        let workdir = run_blocking_api({
            let state = state.clone();
            let session_id = request.session_id.clone();
            let project_id = request.project_id.clone();
            move || {
                state.ensure_read_only_delegation_allows_write_action(
                    session_id.as_deref(),
                    project_id.as_deref(),
                    Some(&workdir_request),
                    "terminal commands",
                )?;
                resolve_project_scoped_requested_path(
                    &state,
                    session_id.as_deref(),
                    project_id.as_deref(),
                    &workdir_request,
                    ScopedPathMode::ExistingPath,
                )
            }
        })
        .await?;
        let task_tx = event_tx.clone();
        let task_cancellation = cancellation.clone();
        spawn_terminal_stream_worker(event_tx.clone(), async move {
            let command_stream_tx = task_tx.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                run_terminal_shell_command_streaming(
                    &command,
                    &workdir,
                    command_stream_tx,
                    task_cancellation,
                )
            })
            .await
            .map_err(|err| ApiError::internal(format!("terminal command task failed: {err}")))
            .and_then(|result| result);
            send_terminal_stream_result(&task_tx, result).await;
        });
    }

    let stream = TerminalCommandSseStream {
        event_rx,
        _cancel_on_drop: cancel_on_drop,
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn spawn_terminal_stream_worker<F>(event_tx: TerminalCommandStreamSender, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let panic_tx = event_tx.clone();
    let worker = tokio::spawn(future);
    tokio::spawn(async move {
        match worker.await {
            Ok(()) => {}
            Err(err) if err.is_panic() => {
                send_terminal_stream_result(
                    &panic_tx,
                    Err(ApiError::internal("terminal stream task panicked")),
                )
                .await;
            }
            Err(err) => {
                send_terminal_stream_result(
                    &panic_tx,
                    Err(ApiError::internal(format!("terminal stream task failed: {err}"))),
                )
                .await;
            }
        }
    });
}

async fn send_terminal_stream_result(
    event_tx: &TerminalCommandStreamSender,
    result: Result<TerminalCommandResponse, ApiError>,
) {
    let event = match result {
        Ok(response) => TerminalCommandStreamEvent::Complete(response),
        Err(err) => TerminalCommandStreamEvent::Error {
            error: err.message,
            status: err.status.as_u16(),
        },
    };
    let _ = event_tx.send(event).await;
}

fn terminal_command_sse_event(event: TerminalCommandStreamEvent) -> Event {
    match event {
        TerminalCommandStreamEvent::Output { stream, text } => {
            match serde_json::to_string(&TerminalOutputStreamPayload { stream, text }) {
                Ok(data) => Event::default().event("output").data(data),
                Err(err) => terminal_error_sse_event(
                    format!("failed to serialize terminal output event: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                ),
            }
        }
        TerminalCommandStreamEvent::Complete(response) => match serde_json::to_string(&response) {
            Ok(data) => Event::default().event("complete").data(data),
            Err(err) => terminal_error_sse_event(
                format!("failed to serialize terminal command response: {err}"),
                StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
            ),
        },
        TerminalCommandStreamEvent::Error { error, status } => {
            terminal_error_sse_event(error, status)
        }
    }
}

fn terminal_error_sse_event(error: String, status: u16) -> Event {
    Event::default()
        .event("error")
        .data(json!({ "error": error, "status": status }).to_string())
}

fn validate_terminal_command(command: &str) -> Result<String, ApiError> {
    let command = command.trim();
    if command.is_empty() {
        return Err(ApiError::bad_request("terminal command cannot be empty"));
    }
    if command.chars().count() > TERMINAL_COMMAND_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "terminal command cannot exceed {TERMINAL_COMMAND_MAX_CHARS} characters"
        )));
    }

    Ok(command.to_owned())
}

fn validate_terminal_workdir(workdir: &str) -> Result<String, ApiError> {
    let workdir = workdir.trim();
    if workdir.is_empty() {
        return Err(ApiError::bad_request("terminal workdir cannot be empty"));
    }
    if workdir.chars().count() > TERMINAL_WORKDIR_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "terminal workdir cannot exceed {TERMINAL_WORKDIR_MAX_CHARS} characters"
        )));
    }
    // Reject interior NUL bytes explicitly at the validator layer so the
    // 400 response names the problem up front instead of deferring to the
    // downstream `fs::canonicalize` syscall, which produces a less-clear
    // message on both platforms.
    if workdir.contains('\0') {
        return Err(ApiError::bad_request(
            "terminal workdir cannot contain NUL bytes",
        ));
    }

    Ok(workdir.to_owned())
}

/// Rewrites a 429 propagated through the remote terminal proxy so the user
/// can tell which side of the proxy throttled them. The local-side 429 says
/// "too many remote terminal commands..." (which is what the user asked
/// for), but a propagated remote-side 429 arrives as
/// "too many local terminal commands..." from the remote host's own
/// semaphore — that phrase is misleading when the user never touched
/// anything on their local machine. Prefixing the message with the
/// remote's display name disambiguates the two cases without changing the
/// wire contract (status code and `{ error: string }` shape).
fn annotate_remote_terminal_429(err: ApiError, remote_name: &str) -> ApiError {
    if err.status != StatusCode::TOO_MANY_REQUESTS {
        return err;
    }
    ApiError {
        status: err.status,
        message: format!("remote {remote_name}: {}", err.message),
        kind: err.kind,
    }
}

/// Runs a terminal shell command in the requested workdir.
fn run_terminal_shell_command(
    command: &str,
    workdir: &FsPath,
) -> Result<TerminalCommandResponse, ApiError> {
    // No production timeout is intentional. Users run long-lived commands
    // here, such as `flutter run`, dev servers, and file watchers.
    run_terminal_shell_command_with_timeout_and_stream(command, workdir, None, None, None)
}

fn run_terminal_shell_command_streaming(
    command: &str,
    workdir: &FsPath,
    event_tx: TerminalCommandStreamSender,
    cancellation: Arc<AtomicBool>,
) -> Result<TerminalCommandResponse, ApiError> {
    // Streaming terminal sessions are expected to stay open until the command
    // exits or the user stops it; do not add a watchdog timeout here.
    run_terminal_shell_command_with_timeout_and_stream(
        command,
        workdir,
        None,
        Some(event_tx),
        Some(cancellation),
    )
}

#[cfg(test)]
fn run_terminal_shell_command_with_timeout(
    command: &str,
    workdir: &FsPath,
    timeout: Duration,
) -> Result<TerminalCommandResponse, ApiError> {
    run_terminal_shell_command_with_timeout_and_stream(command, workdir, Some(timeout), None, None)
}

fn run_terminal_shell_command_with_timeout_and_stream(
    command: &str,
    workdir: &FsPath,
    timeout: Option<Duration>,
    event_tx: Option<TerminalCommandStreamSender>,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<TerminalCommandResponse, ApiError> {
    let (shell_label, mut child_command) = build_terminal_shell_command(command);
    configure_terminal_process_tree(&mut child_command);
    let started_at = std::time::Instant::now();
    let process = Arc::new(
        SharedChild::spawn(
            child_command
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .map_err(|err| ApiError::internal(format!("failed to start terminal command: {err}")))?,
    );
    let process_tree = match TerminalProcessTree::attach(&process) {
        Ok(process_tree) => process_tree,
        Err(err) => {
            eprintln!("terminal warning> failed to attach process tree: {err:#}");
            let _ = kill_child_process(&process, "terminal command");
            return Err(ApiError::internal(format!(
                "failed to prepare terminal command process tree: {err:#}"
            )));
        }
    };

    // `Stdio::piped()` above guarantees both are `Some` today, but a
    // refactor that reorders stdio setup or inserts a fallible step could
    // reach these branches with the child already resumed. On Windows the
    // Job Object's `Drop` still cleans up via
    // `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, but the Unix
    // `TerminalProcessTree` is a unit struct with no `Drop`, and
    // `std::process::Child::drop` is a no-op, so the running shell would
    // leak. Kill the tree explicitly on both branches so both platforms
    // get the same cleanup posture.
    //
    // NOTE: we take stdio handles and spawn the reader threads BEFORE
    // `resume_after_attach`. On Windows the child is `CREATE_SUSPENDED`
    // until resume, so spawning the readers first guarantees they are
    // already blocked in `read()` on the pipes when the child starts
    // producing output. Without this ordering, a chatty child that emits
    // a burst of early output (e.g. a tight `for` loop with `echo`) can
    // fill the default ~4KB Windows pipe buffer between `ResumeThread`
    // and the reader-thread spawn, blocking on pipe write. On Unix
    // `resume_after_attach` is a no-op (the child is already running
    // since `spawn`), so the reorder is a no-op there.
    let stdout = match process.take_stdout() {
        Some(stdout) => stdout,
        None => {
            let _ = process_tree.kill(&process, "terminal command");
            return Err(ApiError::internal(
                "failed to capture terminal command stdout",
            ));
        }
    };
    let stderr = match process.take_stderr() {
        Some(stderr) => stderr,
        None => {
            let _ = process_tree.kill(&process, "terminal command");
            return Err(ApiError::internal(
                "failed to capture terminal command stderr",
            ));
        }
    };
    // Shared buffers let the main thread recover whatever prefix the
    // reader has already accumulated if the join deadline hits before the
    // pipe closes. See `join_terminal_output_reader` and the Unix
    // clean-exit limitation in `docs/bugs.md` for the full rationale.
    //
    // Each reader thread is paired with a `sync_channel(1)` completion
    // signal so the main thread can block in `recv_timeout` inside
    // `join_terminal_output_reader` instead of polling `is_finished()`.
    // The previous poll-loop design added ~10ms P50 latency to every
    // local terminal command (a full sleep tick on the happy path where
    // the reader finishes in microseconds); event-based wake removes
    // that tick.
    let stdout_buffer = new_terminal_output_buffer();
    let stderr_buffer = new_terminal_output_buffer();
    let stdout_reader_buffer = stdout_buffer.clone();
    let stderr_reader_buffer = stderr_buffer.clone();
    let streaming_active = event_tx.as_ref().map(|_| Arc::new(AtomicBool::new(true)));
    let stdout_streamer = event_tx
        .clone()
        .zip(streaming_active.clone())
        .map(|(sender, active)| {
            TerminalOutputStreamer::new(sender, TerminalOutputStream::Stdout, active)
        });
    let stderr_streamer = event_tx.zip(streaming_active.clone()).map(|(sender, active)| {
        TerminalOutputStreamer::new(sender, TerminalOutputStream::Stderr, active)
    });
    let (stdout_done_tx, stdout_done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (stderr_done_tx, stderr_done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let stdout_reader = std::thread::spawn(move || {
        let result = read_capped_terminal_output_into_with_stream(
            stdout,
            &stdout_reader_buffer,
            stdout_streamer.as_ref(),
        );
        // Best-effort completion signal: the main thread may have already
        // dropped the receiver on its own timeout, in which case send
        // returns `Err` and the thread simply exits with the result.
        let _ = stdout_done_tx.send(());
        result
    });
    let stderr_reader = std::thread::spawn(move || {
        let result = read_capped_terminal_output_into_with_stream(
            stderr,
            &stderr_reader_buffer,
            stderr_streamer.as_ref(),
        );
        let _ = stderr_done_tx.send(());
        result
    });

    // With readers already attached to the still-suspended pipes, it is
    // now safe to resume the child. If resume itself fails we need to
    // tear down the Job Object (Windows) / process group (Unix); the
    // spawned reader threads will detect EOF when the pipes close and
    // wind down naturally — we deliberately drop their `JoinHandle`s on
    // this error path because a detached reader is bounded by the kill
    // we just issued.
    if let Err(err) = process_tree.resume_after_attach(&process) {
        eprintln!("terminal warning> failed to resume prepared process tree: {err:#}");
        let _ = process_tree.kill(&process, "terminal command");
        return Err(ApiError::internal(format!(
            "failed to resume terminal command process tree: {err:#}"
        )));
    }

    let (mut status, cancelled) = wait_for_terminal_command_status(
        &process,
        timeout,
        cancellation.as_deref(),
    )?;
    let timed_out = status.is_none();
    if timed_out || cancelled {
        process_tree
            .kill(&process, "terminal command")
            .map_err(|err| ApiError::internal(format!("{err:#}")))?;
        status = wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_millis(250),
            "terminal command",
        )
        .map_err(|err| ApiError::internal(format!("{err:#}")))?;
    } else {
        process_tree
            .cleanup_after_shell_exit(&process, "terminal command")
            .map_err(|err| ApiError::internal(format!("{err:#}")))?;
    }

    let (stdout, stdout_truncated) = join_terminal_output_reader(
        stdout_reader,
        stdout_done_rx,
        stdout_buffer,
        "stdout",
        TERMINAL_OUTPUT_READER_JOIN_TIMEOUT,
        streaming_active.as_ref(),
    )?;
    let (stderr, stderr_truncated) = join_terminal_output_reader(
        stderr_reader,
        stderr_done_rx,
        stderr_buffer,
        "stderr",
        TERMINAL_OUTPUT_READER_JOIN_TIMEOUT,
        streaming_active.as_ref(),
    )?;
    if let Some(active) = &streaming_active {
        active.store(false, Ordering::SeqCst);
    }
    let duration_ms = started_at
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    Ok(TerminalCommandResponse {
        command: command.to_owned(),
        duration_ms,
        exit_code: status.and_then(|exit_status| exit_status.code()),
        output_truncated: stdout_truncated || stderr_truncated,
        shell: shell_label.to_owned(),
        stderr,
        stdout,
        success: !timed_out && status.map(|exit_status| exit_status.success()).unwrap_or(false),
        timed_out,
        workdir: normalize_user_facing_path(workdir)
            .to_string_lossy()
            .into_owned(),
    })
}

#[cfg(windows)]
struct TerminalProcessTree {
    job: TerminalJobObject,
}

#[cfg(windows)]
struct TerminalJobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for TerminalJobObject {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
impl TerminalProcessTree {
    fn attach(process: &Arc<SharedChild>) -> Result<Self> {
        let job = create_terminal_job_object().context("failed to create terminal job object")?;
        assign_terminal_process_to_job(&job, process)
            .context("failed to assign terminal process to job object")?;
        Ok(Self { job })
    }

    fn kill(&self, process: &Arc<SharedChild>, label: &str) -> Result<()> {
        terminate_terminal_job(&self.job, label)?;
        kill_child_process(process, label)
    }

    fn resume_after_attach(&self, process: &Arc<SharedChild>) -> Result<()> {
        resume_terminal_process_threads(process.id())
            .context("failed to resume suspended terminal process")
    }

    fn cleanup_after_shell_exit(&self, _process: &Arc<SharedChild>, label: &str) -> Result<()> {
        terminate_terminal_job(&self.job, label)
    }
}

#[cfg(windows)]
fn terminate_terminal_job(job: &TerminalJobObject, label: &str) -> Result<()> {
    unsafe {
        if windows_sys::Win32::System::JobObjects::TerminateJobObject(job.handle, 1) != 0 {
            return Ok(());
        }
    }

    let err = io::Error::last_os_error();
    Err(anyhow!("failed to terminate {label} job object: {err}"))
}

#[cfg(windows)]
fn resume_terminal_process_threads(process_id: u32) -> io::Result<()> {
    // Takes a system-wide thread snapshot via `CreateToolhelp32Snapshot`
    // and filters by `th32OwnerProcessID == process_id`. This is
    // intentionally O(system-wide thread count) per terminal command,
    // not O(child thread count): the `TH32CS_SNAPTHREAD` snapshot kind
    // does not accept a process-id filter (the `pid` parameter is only
    // honored by the module snapshot kinds), so every call enumerates
    // every thread on the entire host — a typical dev workstation has
    // 2-5k, a busy server can have far more.
    //
    // The alternative — capturing the primary thread handle directly
    // from `CreateProcess` via `PROCESS_INFORMATION.hThread` and calling
    // `ResumeThread` on just that one handle — would require bypassing
    // `std::process::Child`'s encapsulation, either with a crate-level
    // extension trait or a direct `CreateProcess` call that mirrors
    // stdlib's stdio plumbing. That is a substantially larger refactor
    // than the ~10μs the snapshot costs in practice, so we leave this
    // as-is. If the snapshot ever becomes a measurable bottleneck
    // (e.g., on servers with tens of thousands of threads), the right
    // fix is to capture `hThread` at spawn time rather than to add
    // ad-hoc snapshot-scope constants that don't exist in the Win32 API.
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        let mut found_thread = false;
        if Thread32First(snapshot, &mut entry) == 0 {
            // Thread32First failing at the start is distinct from an empty
            // iteration: preserve the real Win32 error code rather than
            // reporting a generic NotFound.
            let err = io::Error::last_os_error();
            let _ = CloseHandle(snapshot);
            return Err(err);
        }

        loop {
            if entry.th32OwnerProcessID == process_id {
                found_thread = true;
                let thread_handle = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if thread_handle.is_null() {
                    let err = io::Error::last_os_error();
                    let _ = CloseHandle(snapshot);
                    return Err(err);
                }

                let resume_result = ResumeThread(thread_handle);
                let resume_error = if resume_result == u32::MAX {
                    Some(io::Error::last_os_error())
                } else {
                    None
                };
                if CloseHandle(thread_handle) == 0 {
                    eprintln!(
                        "terminal warning> failed to close resumed thread handle: {}",
                        io::Error::last_os_error()
                    );
                }
                if let Some(err) = resume_error {
                    let _ = CloseHandle(snapshot);
                    return Err(err);
                }
            }

            if Thread32Next(snapshot, &mut entry) == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                    // A mid-iteration Thread32Next failure that is not the
                    // documented end-of-iteration sentinel is a real error.
                    let _ = CloseHandle(snapshot);
                    return Err(err);
                }
                break;
            }
        }

        if CloseHandle(snapshot) == 0 {
            eprintln!(
                "terminal warning> failed to close thread snapshot handle: {}",
                io::Error::last_os_error()
            );
        }

        if !found_thread {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no threads found for terminal process {process_id}"),
            ));
        }

        Ok(())
    }
}

#[cfg(not(windows))]
struct TerminalProcessTree;

#[cfg(not(windows))]
impl TerminalProcessTree {
    fn attach(_process: &Arc<SharedChild>) -> Result<Self> {
        Ok(Self)
    }

    fn kill(&self, process: &Arc<SharedChild>, label: &str) -> Result<()> {
        // This is called on the timeout path, when
        // `wait_for_shared_child_exit_timeout` returned `None`. In the common
        // case the shell is genuinely still alive and `process.id()` maps to
        // a live process group we own, so killpg is safe.
        //
        // However, `wait_for_shared_child_exit_timeout` uses a detached
        // waiter thread that calls `SharedChild::wait` and then sends the
        // result through a bounded mpsc channel. `recv_timeout` can return
        // `Timeout` in a race where the detached thread's `wait` has just
        // completed (and therefore reaped the shell) but its channel send
        // has not yet been observed by the main thread. If that happened,
        // `process.id()` now refers to a freed PID and `libc::killpg` could
        // hit an unrelated recycled process group — the exact hazard that
        // motivated the `cleanup_after_shell_exit` no-op below.
        //
        // Defensively re-check via `try_wait` before signaling. Because
        // `SharedChild` caches the reaped status under its internal mutex,
        // any `wait` that has already completed on the waiter thread will
        // be visible to us here as `Ok(Some(_))`. If we observe that, the
        // shell is gone, grandchildren have re-parented to init, and the
        // safe thing to do is fall through to the same no-op as the
        // clean-exit path. A nanosecond-scale residual window between this
        // `try_wait` and the `killpg` syscall still exists, but it is
        // orders of magnitude narrower than the original and matches the
        // protection Rust's stdlib `Child::send_signal` gives for
        // single-process kills.
        match process.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(err) => {
                return Err(anyhow!("failed checking {label} process status: {err}"));
            }
        }
        terminate_terminal_process_group(terminal_process_group_id(process.id(), label)?, label)?;
        kill_child_process(process, label)
    }

    fn resume_after_attach(&self, _process: &Arc<SharedChild>) -> Result<()> {
        Ok(())
    }

    fn cleanup_after_shell_exit(&self, _process: &Arc<SharedChild>, _label: &str) -> Result<()> {
        // Intentionally a no-op on Unix. See the terminal-hardening preamble
        // in `docs/bugs.md` for the full rationale.
        //
        // By the time this is reached, `wait_for_shared_child_exit_timeout`
        // has already called `Child::wait`, which reaps the shell and
        // releases its PID (and therefore its process group id) to the
        // kernel's pool. Calling `libc::killpg(process.id(), SIGKILL)` here
        // would race with PID reuse: a brand new, unrelated process group
        // with the same numeric id could be targeted and SIGKILLed on a busy
        // system (macOS caps PIDs at 32k, making the race reachable in
        // practice). Rust's stdlib `Child::send_signal` guards against this
        // exact hazard by early-returning once the child has been reaped;
        // calling `libc::killpg` directly would bypass that protection.
        //
        // Any grandchildren still running at this point have re-parented to
        // init and are outside our tree anyway. If they are still holding
        // the inherited stdout/stderr pipes, the per-stream reader-join
        // timeout (`TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`, 5s each) bounds
        // how long the terminal command waits before returning truncated
        // output. Note that `run_terminal_shell_command_with_timeout` joins
        // stdout and stderr sequentially, so the pathological success-path
        // wall-clock wait is up to ~10s (5s + 5s), not 5s — see the
        // "Unix terminal clean-exit cleanup is a no-op" entry in
        // `docs/bugs.md` for the full rationale. The process group SIGKILL
        // is reserved for the timeout path in `kill`, where the shell has
        // not yet been reaped.
        //
        // The `Result<()>` signature is retained to stay symmetric with the
        // Windows impl above, which still legitimately returns errors from
        // `terminate_terminal_job`.
        Ok(())
    }
}

#[cfg(not(windows))]
fn terminate_terminal_process_group(pgid: libc::pid_t, label: &str) -> Result<()> {
    unsafe {
        if libc::killpg(pgid, libc::SIGKILL) == -1 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(anyhow!(
                    "failed to terminate {label} process group {pgid}: {err}"
                ));
            }
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn terminal_process_group_id(process_id: u32, label: &str) -> Result<libc::pid_t> {
    libc::pid_t::try_from(process_id)
        .map_err(|_| anyhow!("{label} process id {process_id} cannot fit in pid_t"))
}

fn wait_for_terminal_command_status(
    process: &Arc<SharedChild>,
    timeout: Option<Duration>,
    cancellation: Option<&AtomicBool>,
) -> Result<(Option<std::process::ExitStatus>, bool), ApiError> {
    if let Some(timeout) = timeout {
        return wait_for_shared_child_exit_timeout(process, timeout, "terminal command")
            .map(|status| (status, false))
            .map_err(|err| ApiError::internal(format!("{err:#}")));
    }
    let Some(cancellation) = cancellation else {
        return process
            .wait()
            .map(|status| (Some(status), false))
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed waiting for terminal command process: {err}"
                ))
            });
    };

    let (done_tx, done_rx) =
        std::sync::mpsc::sync_channel::<io::Result<std::process::ExitStatus>>(1);
    let waiter_process = process.clone();
    std::thread::spawn(move || {
        let result = waiter_process.wait();
        let _ = done_tx.send(result);
    });

    loop {
        if cancellation.load(Ordering::SeqCst) {
            return Ok((None, true));
        }
        match done_rx.recv_timeout(TERMINAL_COMMAND_CANCEL_POLL_INTERVAL) {
            Ok(Ok(status)) => return Ok((Some(status), false)),
            Ok(Err(err)) => {
                return Err(ApiError::internal(format!(
                    "failed waiting for terminal command process: {err}"
                )));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ApiError::internal(
                    "terminal command status waiter exited without returning a status",
                ));
            }
        }
    }
}

#[cfg(windows)]
fn configure_terminal_process_tree(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    command.creation_flags(CREATE_SUSPENDED);
}

#[cfg(not(windows))]
fn configure_terminal_process_tree(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn create_terminal_job_object() -> io::Result<TerminalJobObject> {
    use std::mem::size_of;
    use std::ptr;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    unsafe {
        let handle = CreateJobObjectW(ptr::null(), ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            let err = io::Error::last_os_error();
            let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
            return Err(err);
        }

        Ok(TerminalJobObject { handle })
    }
}

#[cfg(windows)]
fn assign_terminal_process_to_job(
    job: &TerminalJobObject,
    process: &Arc<SharedChild>,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    unsafe {
        // SAFETY: passing `process.id()` to OpenProcess is PID-stable here
        // because of two load-bearing invariants that any future refactor
        // must preserve:
        //   (a) `SharedChild` wraps `std::process::Child`, which holds the
        //       original Win32 process HANDLE from `CreateProcess`. The
        //       kernel will not recycle the PID while any handle to the
        //       process remains open, so as long as the parent `Child`
        //       has not been reaped and dropped, `process.id()` still
        //       refers to the exact process we just spawned.
        //   (b) The child was spawned with `CREATE_SUSPENDED` in
        //       `configure_terminal_process_tree`, so its primary thread
        //       cannot execute, exit, or be reaped between spawn and this
        //       call. Even the brief window between `SharedChild::spawn`
        //       returning and this OpenProcess call is safe because the
        //       process is literally suspended.
        // If either invariant is removed (e.g., someone drops
        // `CREATE_SUSPENDED`, moves Job Object setup after `resume_after_attach`,
        // or switches to a spawn helper that doesn't hold the HANDLE),
        // this OpenProcess could silently return a handle to a recycled
        // PID belonging to an unrelated process.
        let process_handle =
            OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, process.id());
        if process_handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        let assigned = AssignProcessToJobObject(job.handle, process_handle);
        let assign_error = if assigned == 0 {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        let close_result = CloseHandle(process_handle);
        if let Some(err) = assign_error {
            return Err(err);
        }
        if close_result == 0 {
            eprintln!(
                "terminal warning> failed to close process handle after job assignment: {}",
                io::Error::last_os_error()
            );
        }

        Ok(())
    }
}

/// Builds the platform shell command used by the terminal panel.
fn build_terminal_shell_command(command: &str) -> (&'static str, Command) {
    #[cfg(windows)]
    {
        let mut shell = Command::new("powershell.exe");
        shell.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            command,
        ]);
        ("PowerShell", shell)
    }

    #[cfg(not(windows))]
    {
        // `-l` makes `sh` a login shell so it sources `/etc/profile`
        // and `~/.profile` before executing the command. Users who
        // extend `PATH` from those files (nvm, uv, poetry, pyenv,
        // rbenv, Homebrew on Apple Silicon, `cargo env`, gcloud
        // shims, etc.) expect their tooling to resolve from a
        // terminal panel the same way it does from their desktop
        // terminal emulator — which runs login shells by default on
        // macOS and on most Linux distros via `bash --login` / the
        // session's login-shell entrypoint. Running without `-l`
        // produced "command not found" for users whose only `PATH`
        // adjustment happens inside `.profile`.
        //
        // This is asymmetric with the Windows branch's `-NoProfile`:
        // PowerShell profiles commonly contain heavy per-invocation
        // work (prompt themes, module autoload) and PATH additions
        // on Windows come from the registry rather than the
        // profile, so skipping the profile is the right default
        // there. On Unix the tradeoff tilts the other way.
        let mut shell = Command::new("sh");
        shell.args(["-lc", command]);
        ("sh", shell)
    }
}

/// Accumulator shared between the main thread and a detached terminal
/// output reader thread. The reader thread appends bytes on each `read()`
/// call; the main thread can snapshot the accumulated prefix at any time,
/// including on the reader-join timeout path where the reader is still
/// blocked (e.g. on a Unix backgrounded grandchild that inherited the
/// pipe write end). Without a shared buffer, the main thread would have
/// to drop the `JoinHandle` and lose the entire accumulated prefix.
#[derive(Default)]
struct TerminalOutputBuffer {
    bytes: Vec<u8>,
    emitted_bytes: usize,
    truncated: bool,
}

type SharedTerminalOutputBuffer = Arc<Mutex<TerminalOutputBuffer>>;

#[derive(Clone)]
struct TerminalOutputStreamer {
    sender: TerminalCommandStreamSender,
    stream: TerminalOutputStream,
    active: Arc<AtomicBool>,
}

impl TerminalOutputStreamer {
    fn new(
        sender: TerminalCommandStreamSender,
        stream: TerminalOutputStream,
        active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            sender,
            stream,
            active,
        }
    }

    fn send(&self, text: String) {
        if text.is_empty() || !self.active.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.sender.blocking_send(TerminalCommandStreamEvent::Output {
            stream: self.stream,
            text,
        });
    }
}

fn new_terminal_output_buffer() -> SharedTerminalOutputBuffer {
    Arc::new(Mutex::new(TerminalOutputBuffer::default()))
}

fn snapshot_terminal_output_buffer(buffer: &SharedTerminalOutputBuffer) -> (String, bool) {
    let guard = buffer.lock().expect("terminal output buffer mutex poisoned");
    (
        String::from_utf8_lossy(&guard.bytes).into_owned(),
        guard.truncated,
    )
}

#[cfg(test)]
fn read_capped_terminal_output_into(
    reader: impl std::io::Read,
    buffer: &SharedTerminalOutputBuffer,
) -> io::Result<()> {
    read_capped_terminal_output_into_with_stream(reader, buffer, None)
}

/// Reads command output into `buffer` while storing only a bounded prefix.
/// Runs on a dedicated reader thread; the shared buffer lets the main
/// thread recover the accumulated prefix if the reader-join deadline hits
/// before the pipe closes (see `join_terminal_output_reader` for why
/// this matters on Unix).
fn read_capped_terminal_output_into_with_stream(
    mut reader: impl std::io::Read,
    buffer: &SharedTerminalOutputBuffer,
    streamer: Option<&TerminalOutputStreamer>,
) -> io::Result<()> {
    let mut scratch = [0u8; 8192];

    loop {
        let bytes_read = std::io::Read::read(&mut reader, &mut scratch)?;
        if bytes_read == 0 {
            break;
        }

        let mut guard = buffer
            .lock()
            .expect("terminal output buffer mutex poisoned");
        let remaining = TERMINAL_OUTPUT_MAX_BYTES.saturating_sub(guard.bytes.len());
        if remaining == 0 {
            guard.truncated = true;
            continue;
        }

        let take = bytes_read.min(remaining);
        guard.bytes.extend_from_slice(&scratch[..take]);
        if take < bytes_read {
            guard.truncated = true;
        }
        let chunk = terminal_output_delta_locked(&mut guard, false);
        drop(guard);
        if let (Some(streamer), Some(chunk)) = (streamer, chunk) {
            streamer.send(chunk);
        }
    }

    if let Some(streamer) = streamer {
        if let Some(chunk) = {
            let mut guard = buffer
                .lock()
                .expect("terminal output buffer mutex poisoned");
            terminal_output_delta_locked(&mut guard, true)
        } {
            streamer.send(chunk);
        }
    }

    Ok(())
}

fn terminal_output_delta_locked(
    buffer: &mut TerminalOutputBuffer,
    flush_incomplete_utf8: bool,
) -> Option<String> {
    let streamable_len =
        terminal_streamable_utf8_prefix_len(&buffer.bytes, flush_incomplete_utf8);
    if streamable_len <= buffer.emitted_bytes {
        return None;
    }

    let chunk = String::from_utf8_lossy(&buffer.bytes[buffer.emitted_bytes..streamable_len])
        .into_owned();
    buffer.emitted_bytes = streamable_len;
    Some(chunk)
}

fn terminal_streamable_utf8_prefix_len(bytes: &[u8], flush_incomplete_utf8: bool) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(err) if err.error_len().is_none() && !flush_incomplete_utf8 => err.valid_up_to(),
        Err(_) => bytes.len(),
    }
}

/// Reads command output while storing only a bounded prefix. Test-only
/// convenience wrapper around `read_capped_terminal_output_into` that
/// owns its own buffer; production call sites use the shared-buffer form
/// directly so they can recover the prefix on a reader-join timeout.
#[cfg(test)]
fn read_capped_terminal_output(reader: impl std::io::Read) -> io::Result<(String, bool)> {
    let buffer = new_terminal_output_buffer();
    read_capped_terminal_output_into_with_stream(reader, &buffer, None)?;
    Ok(snapshot_terminal_output_buffer(&buffer))
}

/// Joins a terminal output reader with a bounded timeout. If the reader
/// does not complete within `timeout` — e.g. a Unix backgrounded
/// grandchild still holds the inherited pipe write end after the shell
/// exits — returns whatever prefix the reader has already accumulated
/// into the shared buffer, marked truncated so the caller can tell output
/// is incomplete. The reader thread continues running detached until the
/// pipe finally closes; any bytes written after this timeout are
/// discarded by the thread's own buffer drop on exit.
///
/// Blocks on the reader's `done_rx` completion signal via `recv_timeout`,
/// so the happy-path return wakes as soon as the reader thread sends
/// `()` on exit — no polling tick latency. The previous design polled
/// `handle.is_finished()` with a 10ms sleep, which added ~10ms of P50
/// latency to every local terminal command.
fn join_terminal_output_reader(
    handle: std::thread::JoinHandle<io::Result<()>>,
    done_rx: std::sync::mpsc::Receiver<()>,
    buffer: SharedTerminalOutputBuffer,
    stream_label: &str,
    timeout: Duration,
    streaming_active: Option<&Arc<AtomicBool>>,
) -> Result<(String, bool), ApiError> {
    match done_rx.recv_timeout(timeout) {
        Ok(()) => {
            // Reader thread finished on its own. Join the handle to
            // surface any I/O error or panic, then snapshot the
            // accumulated buffer.
            match handle.join().map_err(|_| {
                ApiError::internal(format!("terminal {stream_label} reader panicked"))
            })? {
                Ok(()) => Ok(snapshot_terminal_output_buffer(&buffer)),
                Err(err) => {
                    eprintln!("terminal warning> failed to read terminal command {stream_label}: {err}");
                    let (bytes, _buffer_truncated) = snapshot_terminal_output_buffer(&buffer);
                    Ok((bytes, true))
                }
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            if let Some(active) = streaming_active {
                active.store(false, Ordering::SeqCst);
            }
            // Reader is still blocked (typically on a Unix backgrounded
            // grandchild that inherited the pipe). Return the prefix
            // accumulated so far, marked truncated. Dropping the
            // `JoinHandle` here detaches the reader thread — it will
            // continue writing into the shared buffer until the pipe
            // closes, but those post-deadline bytes are not observable
            // by the caller we are about to return to. This is the best
            // the synchronous pipe-reading model can do without
            // platform-specific interruption primitives.
            let (bytes, _buffer_truncated) = snapshot_terminal_output_buffer(&buffer);
            Ok((bytes, true))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // The reader closure drops its `SyncSender` when the thread
            // function returns. If `recv_timeout` sees `Disconnected`
            // before an `Ok(())`, the closure either panicked before
            // the `send(())` line or something else in the runtime
            // unwound the thread. Join the handle to surface the panic
            // payload in the error message.
            handle
                .join()
                .map_err(|_| {
                    ApiError::internal(format!("terminal {stream_label} reader panicked"))
                })?
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to read terminal command {stream_label}: {err}"
                    ))
                })?;
            // Unreachable in practice: a thread function that returns
            // `Ok(())` must have executed the preceding `send(())`, so
            // this fallback is only hit if something between the send
            // and the join observed Disconnected for non-panic reasons.
            // Return a truncated snapshot defensively.
            let (bytes, _buffer_truncated) = snapshot_terminal_output_buffer(&buffer);
            Ok((bytes, true))
        }
    }
}
