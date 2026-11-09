// Engram's persistent per-session `engram control` process transport.
//
// Owns: the cached control child per session (`EngramControlProcess`), its
// worker thread, startup handshake and idle lifecycle, the JSON-lines frame
// exchange, and the registry/liveness rules in `ProcessEngramControlTransport`.
// Does not own: the `EngramControlTransport` trait and protocol types, the
// one-shot CLI work-binding reader, doctor diagnostics, or the shared
// `EngramProcessTree` termination wrapper; those stay in
// `engram_host_adapter.rs` (the CLI reader/process-tree seam is tm-81vf).
// Split out of `engram_host_adapter.rs` as a pure code move (tm-tg1g).

const ENGRAM_CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The control process a terminal failure was observed on: its pid and its
/// exit state at that moment, as observed, never inferred afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramControlProcessEvidence {
    pid: u32,
    exit_state: String,
}

fn engram_control_cleanup_pending() -> EngramTransportError {
    engram_control_cleanup_pending_because(None)
}

/// `engram_control_cleanup_pending`, naming why the previous child's cleanup
/// is unconfirmed when that is known.
fn engram_control_cleanup_pending_because(cause: Option<&str>) -> EngramTransportError {
    let mut message =
        "Previous Engram control cleanup is unconfirmed; its exact process remains retained"
            .to_owned();
    if let Some(cause) = cause {
        message.push_str(" (");
        message.push_str(cause);
        message.push(')');
    }
    let mut error = EngramTransportError::local_state(message);
    error.code = Some("control_cleanup_pending".to_owned());
    // The old child exists; only this successor request is provably unsent.
    error.process_never_started = false;
    error.request_never_sent = true;
    error
}

fn engram_control_start_pending() -> EngramTransportError {
    let mut error = engram_control_cleanup_pending();
    error.code = Some("control_start_pending".to_owned());
    error.message = "Engram control startup is already owned by another request".to_owned();
    error
}

fn observe_engram_control_process(process: &SharedChild) -> EngramControlProcessEvidence {
    let exit_state = match process.try_wait() {
        Ok(Some(status)) => format!("exited: {status}"),
        Ok(None) => "running when the failure was seen".to_owned(),
        Err(_) => "unavailable".to_owned(),
    };
    EngramControlProcessEvidence {
        pid: process.id(),
        exit_state,
    }
}

struct EngramProcessRequest {
    /// None is an internal shutdown wake, never a wire frame.
    request: Option<Vec<u8>>,
    response_limit: usize,
    reply: mpsc::Sender<std::result::Result<Value, EngramTransportError>>,
}

struct EngramControlProcess {
    config: EngramConnectionConfig,
    process: Arc<SharedChild>,
    process_tree: Arc<EngramProcessTree>,
    stderr: EngramControlStderr,
    requests: mpsc::Sender<EngramProcessRequest>,
    worker_finished: Arc<AtomicBool>,
    retiring: AtomicBool,
    cleanup_finished: Arc<AtomicBool>,
    /// Why the worker could not confirm its cleanup, once it has given up:
    /// the termination or reap error, or a group still present after the
    /// reap. A successor's refusal names it.
    cleanup_failure: Arc<Mutex<Option<String>>>,
}

impl EngramControlProcess {
    fn observe_failure(&self, error: EngramTransportError) -> EngramTransportError {
        error
            .with_control_process(observe_engram_control_process(&self.process))
            .with_control_stderr(&self.stderr)
    }

    fn request_termination(&self) -> Result<()> {
        self.retiring.store(true, Ordering::Release);
        let (reply, _) = mpsc::channel();
        let _ = self.requests.send(EngramProcessRequest {
            request: None,
            response_limit: ENGRAM_CONTROL_MAX_FRAME_BYTES,
            reply,
        });
        self.process_tree.terminate(&self.process)
    }

    fn terminate(&self) {
        let _ = self.process_tree.terminate(&self.process);
        let _ = self.process.wait();
    }
}

impl Drop for EngramControlProcess {
    fn drop(&mut self) {
        // The control worker already owns this exact child and its reap. Drop
        // may run on a deadline path; it must not turn that deadline into an
        // unbounded wait. Registered retiring children keep successor admission
        // closed until the worker publishes cleanup completion.
        if !self.cleanup_finished.load(Ordering::Acquire) {
            let _ = self.request_termination();
        }
    }
}

#[derive(Clone, Copy)]
struct EngramControlStartupHandshake {
    expected_line: &'static str,
    timeout: Duration,
}

struct ProcessEngramControlTransport {
    host_workdir: PathBuf,
    // An occupied None reserves this session's cold start. Other sessions
    // remain usable, and no caller spawns a losing, unregistered child.
    processes: Mutex<HashMap<String, Option<Arc<EngramControlProcess>>>>,
    // How many children each session has had: a spawn or respawn starts a
    // new connection, whose capabilities are read afresh
    // (`connection_generation`, src/engram_capabilities.rs).
    generations: Mutex<HashMap<String, u64>>,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
    #[cfg(test)]
    start_gate: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
}

impl ProcessEngramControlTransport {
    fn new(host_workdir: PathBuf) -> Self {
        Self {
            host_workdir,
            processes: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            startup_handshake: None,
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
            #[cfg(test)]
            start_gate: Mutex::new(None),
        }
    }
}

impl ProcessEngramControlTransport {
    #[cfg(test)]
    fn with_startup_handshake(
        host_workdir: PathBuf,
        expected_line: &'static str,
        timeout: Duration,
    ) -> Self {
        Self {
            host_workdir,
            processes: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout,
            }),
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
            start_gate: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_startup_handshake_and_idle_timeout(
        host_workdir: PathBuf,
        expected_line: &'static str,
        startup_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            host_workdir,
            processes: Mutex::new(HashMap::new()),
            generations: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout: startup_timeout,
            }),
            idle_timeout,
            start_gate: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn process_for(
        &self,
        connection: &EngramConnectionConfig,
    ) -> std::result::Result<Arc<EngramControlProcess>, EngramTransportError> {
        self.process_for_until(
            connection,
            std::time::Instant::now() + ENGRAM_CONTROL_SETTLE_TIMEOUT,
        )
    }

    fn process_for_until(
        &self,
        connection: &EngramConnectionConfig,
        deadline: std::time::Instant,
    ) -> std::result::Result<Arc<EngramControlProcess>, EngramTransportError> {
        loop {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            let stale = if let Some(entry) = processes.get(&connection.session_id) {
                let Some(existing) = entry.clone() else {
                    drop(processes);
                    if std::time::Instant::now() >= deadline {
                        return Err(engram_control_start_pending());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                if existing.config == *connection
                    && !existing.worker_finished.load(Ordering::Acquire)
                    && !existing.retiring.load(Ordering::Acquire)
                {
                    return Ok(existing);
                }
                existing
            } else {
                processes.insert(connection.session_id.clone(), None);
                break;
            };
            drop(processes);
            stale.request_termination().map_err(|error| {
                engram_control_cleanup_pending_because(Some(&format!(
                    "terminating it failed: {error:#}"
                )))
            })?;
            while !stale.cleanup_finished.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    let failure = stale
                        .cleanup_failure
                        .lock()
                        .expect("Engram control cleanup failure mutex poisoned")
                        .clone();
                    let cause = failure.unwrap_or_else(|| {
                        if stale.worker_finished.load(Ordering::Acquire) {
                            "its worker has not finished cleaning up".to_owned()
                        } else {
                            "its worker is still running".to_owned()
                        }
                    });
                    return Err(engram_control_cleanup_pending_because(Some(&cause)));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if processes
                .get(&connection.session_id)
                .and_then(Option::as_ref)
                .is_some_and(|current| Arc::ptr_eq(current, &stale))
            {
                processes.remove(&connection.session_id);
            }
        }

        #[cfg(test)]
        let start_gate = self.start_gate.lock().unwrap().take();
        #[cfg(test)]
        if let Some((entered, release)) = start_gate {
            let _ = entered.send(());
            let _ = release.recv_timeout(TEST_PHASE_DEADLOCK_GUARD);
        }
        // No registry or StateInner guard spans process creation/handshake.
        let spawned = spawn_engram_control_process(
            connection,
            &self.host_workdir,
            self.startup_handshake,
            self.idle_timeout,
        );
        let mut processes = self
            .processes
            .lock()
            .expect("Engram process registry mutex poisoned");
        match spawned {
            Ok(process) => {
                let process = Arc::new(process);
                // A new child is a new connection: bumped before it is
                // published, so no reader sees it under the old generation.
                *self
                    .generations
                    .lock()
                    .expect("Engram connection generations mutex poisoned")
                    .entry(connection.session_id.clone())
                    .or_insert(0) += 1;
                processes.insert(connection.session_id.clone(), Some(process.clone()));
                Ok(process)
            }
            Err(error) => {
                processes.remove(&connection.session_id);
                Err(error)
            }
        }
    }

    fn discard_process(&self, session_id: &str, expected: &Arc<EngramControlProcess>) {
        let retiring = {
            let processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            let should_remove = processes
                .get(session_id)
                .and_then(Option::as_ref)
                .is_some_and(|current| Arc::ptr_eq(current, expected));
            should_remove.then(|| expected.clone())
        };
        if let Some(process) = retiring {
            let _ = process.request_termination();
        }
    }
}

impl EngramControlTransport for ProcessEngramControlTransport {
    fn has_live_child(&self, session_id: &str) -> bool {
        // A reserved start or a finished or retiring child is not live.
        self.processes
            .lock()
            .expect("Engram process registry mutex poisoned")
            .get(session_id)
            .and_then(Option::as_ref)
            .is_some_and(|process| {
                !process.worker_finished.load(Ordering::Acquire)
                    && !process.retiring.load(Ordering::Acquire)
            })
    }

    fn connection_generation(&self, session_id: &str) -> Option<u64> {
        if !self.has_live_child(session_id) {
            return None;
        }
        self.generations
            .lock()
            .expect("Engram connection generations mutex poisoned")
            .get(session_id)
            .copied()
    }

    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        // Only the control worker writes to the process, and only after it
        // receives this request from the channel below. Every failure before
        // that hand-off provably sent no byte of the request frame, so it is
        // marked `request_never_sent`. A deadline, EOF or protocol failure
        // after the hand-off may follow a partial or complete write and
        // keeps the outcome unknown.
        let started_at = std::time::Instant::now();
        let encoded = encode_engram_control_request(request)?;

        // Spawn, attach, resume and the startup handshake only read from the
        // process; none of them writes this request. A program that could
        // not be resolved at all already says its process never started
        // (`spawn_failed`), which keeps its own meaning.
        let process = self
            .process_for_until(connection, started_at + timeout)
            .map_err(|error| {
                if error.process_never_started {
                    error
                } else {
                    error.request_never_sent()
                }
            })?;
        if started_at.elapsed() >= timeout {
            // This request sent no frame. Leave the registered process for a
            // later request with a fresh budget; repeated cold starts could
            // otherwise prevent bounded lifecycle reads from ever progressing.
            return Err(process
                .observe_failure(EngramTransportError::deadline(
                    "Engram admission budget exhausted during process startup",
                ))
                .request_never_sent());
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        process
            .requests
            .send(EngramProcessRequest {
                request: Some(encoded),
                response_limit: engram_control_response_frame_limit(request),
                reply: reply_tx,
            })
            .map_err(|err| {
                // The worker had already dropped its receiver, so it never
                // took this request and wrote none of it.
                let error = process
                    .observe_failure(EngramTransportError::transport(format!(
                        "Engram control worker is unavailable: {err}"
                    )))
                    .request_never_sent();
                self.discard_process(&connection.session_id, &process);
                error
            })?;

        let remaining = timeout.saturating_sub(started_at.elapsed());
        match reply_rx.recv_timeout(remaining) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                // A valid Engram error envelope is an application response on
                // a healthy JSON-lines connection. Only I/O/protocol failures
                // terminate the worker and require process replacement.
                if !error.keeps_control_process_alive() {
                    self.discard_process(&connection.session_id, &process);
                }
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let error = process.observe_failure(EngramTransportError::deadline(format!(
                    "Engram control call exceeded {} ms",
                    timeout.as_millis()
                )));
                self.discard_process(&connection.session_id, &process);
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let error = process.observe_failure(EngramTransportError::transport(
                    "Engram control worker exited before replying",
                ));
                self.discard_process(&connection.session_id, &process);
                Err(error)
            }
        }
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        read_engram_work_binding_from_cli(connection, preference, timeout, &self.host_workdir)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        read_engram_held_claims_from_cli(connection, timeout, &self.host_workdir)
    }

    fn shutdown_session(&self, session_id: &str) {
        // Startup keeps its registry reservation until it publishes a child
        // or an error. Never remove that reservation and admit another spawn.
        let process = loop {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if processes.get(session_id).is_some_and(Option::is_none) {
                drop(processes);
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            break processes.remove(session_id).flatten();
        };
        if let Some(process) = process {
            process.terminate();
        }
    }

    fn shutdown_session_until(
        &self,
        session_id: &str,
        clock: &EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let process = loop {
            let entry = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned")
                .get(session_id)
                .cloned();
            match entry {
                None => return Ok(()),
                Some(Some(process)) => break process,
                Some(None) => {
                    if clock.now() >= deadline {
                        return Err(engram_control_start_pending());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        process.request_termination().map_err(|_| {
            EngramTransportError::transport(
                "Engram control cleanup is unconfirmed; the original process remains retained",
            )
        })?;
        while !process.cleanup_finished.load(Ordering::Acquire) {
            if clock.now() >= deadline {
                return Err(EngramTransportError::deadline(
                    "Engram control cleanup exceeded the shared reset budget; the original process remains retained",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut registry = self
            .processes
            .lock()
            .expect("Engram process registry mutex poisoned");
        if registry
            .get(session_id)
            .and_then(Option::as_ref)
            .is_some_and(|current| Arc::ptr_eq(current, &process))
        {
            registry.remove(session_id);
        }
        Ok(())
    }
}

fn spawn_engram_control_process(
    connection: &EngramConnectionConfig,
    host_workdir: &FsPath,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
) -> std::result::Result<EngramControlProcess, EngramTransportError> {
    let mut command = engram_host_command(
        &connection.binary_path,
        &connection.project_root,
        &connection.project_file,
        &connection.home,
        host_workdir,
    )?;
    configure_terminal_process_tree(&mut command);
    apply_engram_connection_environment(&mut command, connection);
    command
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("--home")
        .arg(&connection.home)
        .arg("control")
        .arg("--work-session")
        .arg("--actor-id")
        .arg(&connection.actor_id);
    if let Some(actor_context) = connection.actor_context.as_deref() {
        command.arg("--actor-context").arg(actor_context);
    }
    let mut child = command
        .arg("--session-id")
        .arg(&connection.session_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            EngramTransportError::transport(format!("failed spawning Engram control: {err}"))
        })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngramTransportError::transport("Engram control stdin is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngramTransportError::transport("Engram control stdout is unavailable"))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| EngramTransportError::transport("Engram control stderr is unavailable"))?;
    let process = Arc::new(SharedChild::new(child).map_err(|err| {
        EngramTransportError::transport(format!("failed sharing Engram control child: {err}"))
    })?);
    let process_tree = Arc::new(EngramProcessTree::attach(&process).map_err(|err| {
        let _ = kill_child_process(&process, "Engram control");
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed preparing Engram control process tree: {err:#}"
        ))
    })?);
    let stderr = Arc::new(Mutex::new(EngramControlStderrObservation::default()));
    let worker_stderr = stderr.clone();
    let reader_observation = stderr.clone();
    // The reader owns only this pipe and bounded metadata. Exact tree cleanup
    // closes the pipe; EOF reporting never waits for the reader to finish.
    let _stderr_reader = std::thread::Builder::new()
        .name("engram-control-stderr".to_owned())
        .spawn(move || read_engram_control_stderr(stderr_pipe, &reader_observation))
        .map_err(|_| {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            EngramTransportError::transport("failed spawning Engram control stderr reader")
        })?;
    let worker_process = process.clone();
    let worker_process_tree = process_tree.clone();
    let worker_finished = Arc::new(AtomicBool::new(false));
    let worker_finished_signal = worker_finished.clone();
    let cleanup_finished = Arc::new(AtomicBool::new(false));
    let worker_cleanup_finished = cleanup_finished.clone();
    let cleanup_failure = Arc::new(Mutex::new(None));
    let worker_cleanup_failure = cleanup_failure.clone();
    let (request_tx, request_rx) = mpsc::channel::<EngramProcessRequest>();
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("engram-control-{}", connection.session_id))
        .spawn(move || {
            let mut stdout = BufReader::new(stdout);
            if let Some(handshake) = startup_handshake {
                let startup_result = read_engram_control_startup_handshake(&mut stdout, handshake)
                    .map_err(|error| {
                        error
                            .with_control_process(observe_engram_control_process(&worker_process))
                            .with_control_stderr(&worker_stderr)
                    });
                let startup_failed = startup_result.is_err();
                let _ = startup_tx.send(startup_result);
                if startup_failed {
                    worker_finished_signal.store(true, Ordering::Release);
                    finish_engram_control_cleanup(
                        &worker_process_tree,
                        &worker_process,
                        &worker_cleanup_finished,
                        &worker_cleanup_failure,
                    );
                    return;
                }
            }
            loop {
                match request_rx.recv_timeout(idle_timeout) {
                    Ok(request) => {
                        let Some(frame) = request.request else {
                            worker_finished_signal.store(true, Ordering::Release);
                            break;
                        };
                        let mut result = exchange_engram_control_frame_with_limit(
                            &mut stdin,
                            &mut stdout,
                            &frame,
                            request.response_limit,
                        );
                        // Engram's host service continues after request-level
                        // `{status:"error"}` replies. Treat only malformed or
                        // broken exchanges as terminal for this sidecar.
                        let is_terminal = result
                            .as_ref()
                            .is_err_and(|error| !error.keeps_control_process_alive());
                        if is_terminal {
                            worker_finished_signal.store(true, Ordering::Release);
                            // Observed before this worker terminates it.
                            let evidence = observe_engram_control_process(&worker_process);
                            result = result.map_err(|error| {
                                error
                                    .with_control_process(evidence)
                                    .with_control_stderr(&worker_stderr)
                            });
                        }
                        let _ = request.reply.send(result);
                        if is_terminal {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                        worker_finished_signal.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            finish_engram_control_cleanup(
                &worker_process_tree,
                &worker_process,
                &worker_cleanup_finished,
                &worker_cleanup_failure,
            );
        })
        .map_err(|err| {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            EngramTransportError::transport(format!("failed spawning Engram control worker: {err}"))
        })?;

    if let Err(err) = process_tree.resume_after_attach(&process) {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
        return Err(EngramTransportError::transport(format!(
            "failed resuming Engram control process: {err:#}"
        )));
    }

    if let Some(handshake) = startup_handshake {
        let startup_result = match startup_rx.recv_timeout(handshake.timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(EngramTransportError::transport(format!(
                "Engram control startup handshake exceeded {} ms",
                handshake.timeout.as_millis()
            ))
            .with_control_process(observe_engram_control_process(&process))
            .with_control_stderr(&stderr)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngramTransportError::transport(
                "Engram control worker exited before the startup handshake",
            )
            .with_control_process(observe_engram_control_process(&process))
            .with_control_stderr(&stderr)),
        };
        if let Err(error) = startup_result {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            return Err(error);
        }
    }

    Ok(EngramControlProcess {
        config: connection.clone(),
        process,
        process_tree,
        stderr,
        requests: request_tx,
        worker_finished,
        retiring: AtomicBool::new(false),
        cleanup_finished,
        cleanup_failure,
    })
}

/// The control worker's last step for its child: terminate the tree, reap
/// the child, and confirm the tree is gone. Cleanup is published only when
/// all three hold; otherwise the first unmet step is kept for a successor's
/// refusal to name.
fn finish_engram_control_cleanup(
    process_tree: &EngramProcessTree,
    process: &Arc<SharedChild>,
    cleanup_finished: &AtomicBool,
    cleanup_failure: &Mutex<Option<String>>,
) {
    let terminated = process_tree.terminate(process);
    let reaped = process.wait();
    let failure = match (&terminated, &reaped) {
        (Err(error), _) => Some(format!("terminating it failed: {error:#}")),
        (_, Err(error)) => Some(format!("reaping it failed: {error}")),
        _ if !process_tree.confirmed_after_reap() => {
            Some("its process group was still present after the reap".to_owned())
        }
        _ => None,
    };
    match failure {
        None => cleanup_finished.store(true, Ordering::Release),
        Some(failure) => {
            *cleanup_failure
                .lock()
                .expect("Engram control cleanup failure mutex poisoned") = Some(failure);
        }
    }
}

fn read_engram_control_startup_handshake(
    stdout: &mut impl BufRead,
    handshake: EngramControlStartupHandshake,
) -> std::result::Result<(), EngramTransportError> {
    let mut line = String::new();
    let read = stdout.read_line(&mut line).map_err(|err| {
        EngramTransportError::transport(format!(
            "failed reading Engram control startup handshake: {err}"
        ))
    })?;
    if read == 0 {
        return Err(EngramTransportError::transport(
            "Engram control reached EOF before the startup handshake",
        ));
    }
    if line.trim_end_matches(['\r', '\n']) != handshake.expected_line {
        return Err(EngramTransportError::protocol(
            "Engram control returned an unexpected startup handshake",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn exchange_engram_control_frame(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: &[u8],
) -> std::result::Result<Value, EngramTransportError> {
    exchange_engram_control_frame_with_limit(stdin, stdout, request, ENGRAM_CONTROL_MAX_FRAME_BYTES)
}

fn exchange_engram_control_frame_with_limit(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: &[u8],
    response_limit: usize,
) -> std::result::Result<Value, EngramTransportError> {
    stdin
        .write_all(request)
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|err| {
            EngramTransportError::transport(format!("failed writing Engram request: {err}"))
        })?;

    let mut response = Vec::new();
    // Bound allocation before reading; an overlong line retires the sidecar
    // instead of leaving unread bytes available as a successor's reply.
    let read = std::io::Read::take(stdout, (response_limit + 1) as u64)
        .read_until(b'\n', &mut response)
        .map_err(|err| {
            EngramTransportError::transport(format!("failed reading Engram response: {err}"))
        })?;
    if read == 0 {
        return Err(EngramTransportError::transport(
            "Engram control reached EOF before replying",
        ));
    }
    if response.len() > response_limit {
        return Err(EngramTransportError::protocol(
            "Engram response exceeds the maximum control frame",
        ));
    }
    let envelope: EngramControlResponse =
        engram_decode_slice(&response, "invalid Engram response")?;
    match envelope {
        EngramControlResponse::Ok { result } => {
            if response_limit == ACCEPTANCE_PRIVATE_RESPONSE_MAX_BYTES
                && serde_json::to_vec(&result)
                    .map_err(|_| {
                        EngramTransportError::protocol("cannot measure private evidence page")
                    })?
                    .len()
                    > ACCEPTANCE_PRIVATE_PAGE_MAX_BYTES
            {
                return Err(EngramTransportError::protocol(
                    "private evidence page exceeds the supported capacity",
                ));
            }
            Ok(result)
        }
        EngramControlResponse::Error { error } => {
            Err(EngramTransportError::remote(error.into_body()))
        }
    }
}
