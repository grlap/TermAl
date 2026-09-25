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

struct EngramProcessRequest {
    request: Vec<u8>,
    reply: mpsc::Sender<std::result::Result<Value, EngramTransportError>>,
}

struct EngramControlProcess {
    config: EngramConnectionConfig,
    process: Arc<SharedChild>,
    process_tree: Arc<EngramProcessTree>,
    requests: mpsc::Sender<EngramProcessRequest>,
    worker_finished: Arc<AtomicBool>,
}

impl EngramControlProcess {
    fn terminate(&self) {
        let _ = self.process_tree.terminate(&self.process);
        let _ = self.process.wait();
    }
}

impl Drop for EngramControlProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[derive(Clone, Copy)]
struct EngramControlStartupHandshake {
    expected_line: &'static str,
    timeout: Duration,
}

struct ProcessEngramControlTransport {
    processes: Mutex<HashMap<String, Arc<EngramControlProcess>>>,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
}

impl Default for ProcessEngramControlTransport {
    fn default() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: None,
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
        }
    }
}

impl ProcessEngramControlTransport {
    #[cfg(test)]
    fn with_startup_handshake(expected_line: &'static str, timeout: Duration) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout,
            }),
            idle_timeout: ENGRAM_CONTROL_IDLE_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_startup_handshake_and_idle_timeout(
        expected_line: &'static str,
        startup_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            startup_handshake: Some(EngramControlStartupHandshake {
                expected_line,
                timeout: startup_timeout,
            }),
            idle_timeout,
        }
    }

    fn process_for(
        &self,
        connection: &EngramConnectionConfig,
    ) -> std::result::Result<Arc<EngramControlProcess>, EngramTransportError> {
        let stale = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if let Some(existing) = processes.get(&connection.session_id).cloned() {
                if existing.config == *connection
                    && !existing.worker_finished.load(Ordering::Acquire)
                {
                    return Ok(existing);
                }
                processes.remove(&connection.session_id)
            } else {
                None
            }
        };
        if let Some(stale) = stale {
            stale.terminate();
        }

        let process = Arc::new(spawn_engram_control_process(
            connection,
            self.startup_handshake,
            self.idle_timeout,
        )?);
        let (selected, displaced) = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            if let Some(existing) = processes.get(&connection.session_id).cloned()
                && existing.config == *connection
                && !existing.worker_finished.load(Ordering::Acquire)
            {
                (existing, Some(process))
            } else {
                let displaced = processes.insert(connection.session_id.clone(), process.clone());
                (process, displaced)
            }
        };
        if let Some(displaced) = displaced {
            displaced.terminate();
        }
        Ok(selected)
    }

    fn discard_process(&self, session_id: &str, expected: &Arc<EngramControlProcess>) {
        let removed = {
            let mut processes = self
                .processes
                .lock()
                .expect("Engram process registry mutex poisoned");
            let should_remove = processes
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, expected));
            should_remove
                .then(|| processes.remove(session_id))
                .flatten()
        };
        if let Some(process) = removed {
            process.terminate();
        }
    }
}

impl EngramControlTransport for ProcessEngramControlTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let started_at = std::time::Instant::now();
        let encoded = serde_json::to_vec(request).map_err(|err| {
            EngramTransportError::protocol(format!("failed encoding request: {err}"))
        })?;
        if encoded.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
            return Err(EngramTransportError::protocol(
                "Engram request exceeds the maximum control frame",
            ));
        }

        let process = self.process_for(connection)?;
        if started_at.elapsed() >= timeout {
            self.discard_process(&connection.session_id, &process);
            return Err(EngramTransportError::deadline(
                "Engram admission budget exhausted during process startup",
            ));
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        process
            .requests
            .send(EngramProcessRequest {
                request: encoded,
                reply: reply_tx,
            })
            .map_err(|err| {
                self.discard_process(&connection.session_id, &process);
                EngramTransportError::transport(format!(
                    "Engram control worker is unavailable: {err}"
                ))
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
                self.discard_process(&connection.session_id, &process);
                Err(EngramTransportError::deadline(format!(
                    "Engram control call exceeded {} ms",
                    timeout.as_millis()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.discard_process(&connection.session_id, &process);
                Err(EngramTransportError::transport(
                    "Engram control worker exited before replying",
                ))
            }
        }
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        read_engram_work_binding_from_cli(connection, preference, timeout, false)
    }

    fn read_work_binding_for_boot(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        read_engram_work_binding_from_cli(connection, preference, timeout, true)
    }

    fn shutdown_session(&self, session_id: &str) {
        let process = self
            .processes
            .lock()
            .expect("Engram process registry mutex poisoned")
            .remove(session_id);
        if let Some(process) = process {
            process.terminate();
        }
    }
}

fn spawn_engram_control_process(
    connection: &EngramConnectionConfig,
    startup_handshake: Option<EngramControlStartupHandshake>,
    idle_timeout: Duration,
) -> std::result::Result<EngramControlProcess, EngramTransportError> {
    let mut command = engram_command(&connection.binary_path);
    configure_terminal_process_tree(&mut command);
    apply_engram_connection_environment(&mut command, connection);
    command
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("--home")
        .arg(&connection.home)
        .arg("control")
        .arg("--actor-id")
        .arg(&connection.actor_id);
    if let Some(actor_context) = connection.actor_context.as_deref() {
        command.arg("--actor-context").arg(actor_context);
    }
    let mut child = command
        .arg("--session-id")
        .arg(&connection.session_id)
        .current_dir(&connection.project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
    let worker_process = process.clone();
    let worker_process_tree = process_tree.clone();
    let worker_finished = Arc::new(AtomicBool::new(false));
    let worker_finished_signal = worker_finished.clone();
    let (request_tx, request_rx) = mpsc::channel::<EngramProcessRequest>();
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("engram-control-{}", connection.session_id))
        .spawn(move || {
            let mut stdout = BufReader::new(stdout);
            if let Some(handshake) = startup_handshake {
                let startup_result = read_engram_control_startup_handshake(&mut stdout, handshake);
                let startup_failed = startup_result.is_err();
                let _ = startup_tx.send(startup_result);
                if startup_failed {
                    worker_finished_signal.store(true, Ordering::Release);
                    let _ = worker_process_tree.terminate(&worker_process);
                    let _ = worker_process.wait();
                    return;
                }
            }
            loop {
                match request_rx.recv_timeout(idle_timeout) {
                    Ok(request) => {
                        let result = exchange_engram_control_frame(
                            &mut stdin,
                            &mut stdout,
                            &request.request,
                        );
                        // Engram's host service continues after request-level
                        // `{status:"error"}` replies. Treat only malformed or
                        // broken exchanges as terminal for this sidecar.
                        let is_terminal = result
                            .as_ref()
                            .is_err_and(|error| !error.keeps_control_process_alive());
                        if is_terminal {
                            worker_finished_signal.store(true, Ordering::Release);
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
            let _ = worker_process_tree.terminate(&worker_process);
            let _ = worker_process.wait();
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
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(EngramTransportError::transport(
                "Engram control worker exited before the startup handshake",
            )),
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
        requests: request_tx,
        worker_finished,
    })
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

fn exchange_engram_control_frame(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: &[u8],
) -> std::result::Result<Value, EngramTransportError> {
    stdin
        .write_all(request)
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|err| {
            EngramTransportError::transport(format!("failed writing Engram request: {err}"))
        })?;

    let mut response = Vec::new();
    let read = stdout.read_until(b'\n', &mut response).map_err(|err| {
        EngramTransportError::transport(format!("failed reading Engram response: {err}"))
    })?;
    if read == 0 {
        return Err(EngramTransportError::transport(
            "Engram control reached EOF before replying",
        ));
    }
    if response.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
        return Err(EngramTransportError::protocol(
            "Engram response exceeds the maximum control frame",
        ));
    }
    let envelope: EngramControlResponse = serde_json::from_slice(&response)
        .map_err(|err| EngramTransportError::protocol(format!("invalid Engram response: {err}")))?;
    match envelope {
        EngramControlResponse::Ok { result } => Ok(result),
        EngramControlResponse::Error { error } => Err(EngramTransportError::remote(error)),
    }
}
