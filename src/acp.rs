// ACP (Agent Client Protocol) runtime implementation.
//
// Covers spawning, session configuration (including agent-specific authority
// reconciliation that must gate prompt dispatch), prompt dispatch,
// message/notification handling, and JSON-RPC request wiring for ACP-protocol
// agents (Gemini CLI, Cursor, OpenCode). Executable discovery and launch
// construction stay in the small agent boundary files. The ACP-specific
// protocol types (`AcpRuntimeCommand`,
// `AcpPromptCommand`, `AcpPendingApproval`, `AcpRuntimeState`, `AcpTurnState`,
// `AcpJsonRpcError`, `AcpResponseError`, `PendingAcpJsonRpcRequest`) stay in
// runtime.rs next to the Codex + Claude types — this file owns only the
// implementation.
//
// Extracted from runtime.rs into its own `include!()` fragment so the ACP
// subsystem lives in one place. The crate still compiles as one flat module,
// so no visibility changes are required.
//
// Protocol flow contract
// ----------------------
//
// Every ACP session goes through a strictly ordered handshake before it
// can dispatch a prompt. `spawn_acp_runtime` launches the agent
// subprocess and then drives the handshake through these phases in
// sequence, failing the runtime (and the session) if any phase errors:
//
// 1. **initialize** — send `initialize` with our protocol version and
//    capability declaration; receive the agent's capabilities +
//    supported auth methods.
// 2. **authenticate** (optional) — if the agent advertises an auth
//    method, send `authenticate` and wait for success before
//    proceeding. Gemini is the typical caller; Claude Code usually
//    skips this phase.
// 3. **session/resume, session/load, or session/new** — use resume when
//    explicitly advertised, otherwise use the legacy load capability when
//    supported. A typed method-not-found can downgrade optimistic legacy load
//    probing for non-OpenCode agents. OpenCode load/resume failures always
//    surface and preserve continuity; the user starts a separate session if
//    recovery is impossible.
// 4. **session/set_mode** + **session/set_model** — apply the user's
//    saved approval-mode / model preferences before the first prompt
//    so the agent doesn't default to something the user didn't pick.
// 5. **normal operation** — `session/prompt`, `session/request_permission`
//    (from agent → TermAl), `session/update` notifications, etc.
//
// Pending JSON-RPC request ownership
// ----------------------------------
//
// Every outbound request is registered in `PendingAcpJsonRpcRequest`
// before the write hits the wire; the reader thread matches responses
// by `id` and removes the entry. If the subprocess exits before a
// response arrives, the reader thread drains the pending map and
// cancels each entry with a runtime-exit error. Callers that await
// those channels therefore always wake up — no response can be lost
// in the dead-process path.
//
// Timeouts
// --------
//
// The initialize + session/load handshake uses a short timeout
// (`ACP_HANDSHAKE_TIMEOUT`) because a misbehaving agent would
// otherwise keep the session stuck in spawn-pending forever.
// `session/prompt` runs without a hard timeout — agents may
// legitimately take minutes for a single turn — but the writer
// enforces a stdin-write watchdog so a hung pipe fails fast.
//
// Fallback rules for session load
// -------------------------------
//
// OpenCode 1.18.8 has no typed invalid-session discriminator on load/resume,
// so it never participates in an automatic fallback that would silently
// replace conversation memory. Gemini retains its documented typed
// invalid-session compatibility fallback; legacy non-OpenCode agents may also
// fall back after a typed JSON-RPC method-not-found proves `session/load`
// unsupported.

const ACP_CANCEL_GRACE: Duration = Duration::from_secs(2);
const MAX_ACP_OPTION_LABEL_CHARS: usize = 200;
const MAX_ACP_OPTION_DESCRIPTION_CHARS: usize = 1_000;

/// Spawns ACP runtime.
fn spawn_acp_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    agent: AcpAgent,
    gemini_approval_mode: Option<GeminiApprovalMode>,
) -> Result<AcpRuntimeHandle> {
    if !state.agent_runtime_spawning_enabled {
        bail!("agent runtime spawning is disabled for this AppState");
    }
    let runtime_id = Uuid::new_v4().to_string();
    let cwd = normalize_local_user_facing_path(&cwd);
    let mut command = agent.command(AcpLaunchOptions {
        gemini_approval_mode,
    })?;
    if agent == AcpAgent::Gemini {
        if let Some(settings_path) = prepare_termal_gemini_system_settings(&cwd)? {
            command.env("GEMINI_CLI_SYSTEM_SETTINGS_PATH", settings_path);
        }
        for (key, value) in gemini_dotenv_env_pairs() {
            if !env_var_present(&key) {
                command.env(key, value);
            }
        }
    }
    command
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {} ACP runtime in `{cwd}`", agent.label()))?;
    let stdin = child
        .stdin
        .take()
        .with_context(|| format!("failed to capture {} ACP stdin", agent.label()))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("failed to capture {} ACP stdout", agent.label()))?;
    let stderr = child
        .stderr
        .take()
        .with_context(|| format!("failed to capture {} ACP stderr", agent.label()))?;
    let process = Arc::new(
        SharedChild::new(child)
            .with_context(|| format!("failed to share {} ACP runtime child", agent.label()))?,
    );
    let (input_tx, input_rx) = mpsc::channel::<AcpRuntimeCommand>();
    let pending_requests: AcpPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let runtime_state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let turn_lifecycle: AcpTurnLifecycle = Arc::new((Mutex::new(false), Condvar::new()));

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_runtime_state = runtime_state.clone();
        let writer_turn_lifecycle = turn_lifecycle.clone();
        let writer_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        let writer_cwd = cwd.clone();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            let initialize_result = send_acp_json_rpc_request(
                &mut stdin,
                &writer_pending_requests,
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientInfo": {
                        "name": "termal",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "clientCapabilities": {},
                }),
                Duration::from_secs(15),
                agent,
            )
            .and_then(|result| {
                update_acp_runtime_capabilities(&writer_runtime_state, &result);
                maybe_authenticate_acp_runtime(
                    &mut stdin,
                    &writer_pending_requests,
                    &result,
                    agent,
                    &writer_cwd,
                )
            });

            if let Err(err) = initialize_result {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!(
                        "failed to initialize {} ACP session: {err:#}",
                        agent.label()
                    )),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let command_result = match command {
                    AcpRuntimeCommand::Prompt(prompt) => handle_acp_prompt_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_session_id,
                        &writer_runtime_state,
                        &writer_turn_lifecycle,
                        &writer_runtime_token,
                        agent,
                        prompt,
                    ),
                    AcpRuntimeCommand::JsonRpcMessage(message) => {
                        write_acp_json_rpc_message(&mut stdin, &message, agent)
                    }
                    AcpRuntimeCommand::Cancel => handle_acp_cancel_command(
                        &mut stdin,
                        &writer_runtime_state,
                        agent,
                    ),
                    AcpRuntimeCommand::RefreshSessionConfig {
                        command,
                        response_tx,
                    } => {
                        let refresh_result = handle_acp_session_config_refresh(
                            &mut stdin,
                            &writer_pending_requests,
                            &writer_state,
                            &writer_session_id,
                            &writer_runtime_state,
                            agent,
                            command,
                        )
                        .map_err(|err| format!("{err:#}"));
                        match refresh_result {
                            Ok(()) => {
                                let _ = response_tx.send(Ok(()));
                                Ok(())
                            }
                            Err(detail) => {
                                let _ = response_tx.send(Err(detail.clone()));
                                Err(anyhow!(detail))
                            }
                        }
                    }
                    AcpRuntimeCommand::ApplyOpenCodeConfig {
                        selections,
                        execution_deadline,
                        started_tx,
                        proceed_rx,
                        response_tx,
                    } => handle_opencode_config_apply_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_session_id,
                        &writer_runtime_state,
                        agent,
                        selections,
                        execution_deadline,
                        started_tx,
                        proceed_rx,
                        response_tx,
                    ),
                    AcpRuntimeCommand::ReconcileOpenCodeConfig { config_result } => {
                        handle_opencode_config_reconcile_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_session_id,
                        &writer_runtime_state,
                        agent,
                        &config_result,
                        )
                    }
                };

                if let Err(err) = command_result {
                    let _ = writer_state.handle_runtime_exit_if_matches(
                        &writer_session_id,
                        &writer_runtime_token,
                        Some(&format!(
                            "failed to communicate with {} ACP runtime: {err:#}",
                            agent.label()
                        )),
                    );
                    break;
                }
            }
        });
    }

    {
        let reader_session_id = session_id.clone();
        let reader_state = state.clone();
        let reader_pending_requests = pending_requests.clone();
        let reader_runtime_state = runtime_state.clone();
        let reader_input_tx = input_tx.clone();
        let reader_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let mut turn_state = AcpTurnState::default();
            let mut recorder =
                SessionRecorder::new(reader_state.clone(), reader_session_id.clone());

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!(
                                "failed to read stdout from {} ACP runtime: {err}",
                                agent.label()
                            ),
                        );
                        break;
                    }
                };

                if bytes_read == 0 {
                    break;
                }

                let message: Value = match serde_json::from_str(raw_line.trim_end()) {
                    Ok(message) => message,
                    Err(err) => {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to parse {} ACP JSON line: {err}", agent.label()),
                        );
                        break;
                    }
                };

                if let Err(err) = handle_acp_message(
                    &message,
                    &reader_state,
                    &reader_session_id,
                    &reader_runtime_token,
                    &reader_pending_requests,
                    &reader_runtime_state,
                    &reader_input_tx,
                    &mut turn_state,
                    &mut recorder,
                    agent,
                ) {
                    let _ = reader_state.fail_turn_if_runtime_matches(
                        &reader_session_id,
                        &reader_runtime_token,
                        &format!("failed to handle {} ACP event: {err:#}", agent.label()),
                    );
                    break;
                }
            }

            fail_pending_acp_requests(
                &reader_pending_requests,
                &format!(
                    "{} ACP runtime stopped while waiting for a pending response",
                    agent.label()
                ),
            );
            let _ = finish_acp_turn_state(&mut recorder, &mut turn_state, agent);
            let _ = recorder.finish_streaming_text();
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let label = agent.label().to_lowercase();
                let timestamp = runtime_stderr_timestamp();
                let prefix = format_runtime_stderr_prefix(&label, &timestamp);
                eprintln!("{prefix} {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_pending_requests = pending_requests.clone();
        let wait_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || match wait_process.wait() {
            Ok(status) if status.success() => {
                fail_pending_acp_requests(
                    &wait_pending_requests,
                    &format!(
                        "{} ACP runtime exited while waiting for a pending response",
                        agent.label()
                    ),
                );
                let _ = wait_state.handle_runtime_exit_if_matches(
                    &wait_session_id,
                    &wait_runtime_token,
                    None,
                );
            }
            Ok(status) => {
                let detail = format!("{} session exited with status {status}", agent.label());
                fail_pending_acp_requests(&wait_pending_requests, &detail);
                let _ = wait_state.handle_runtime_exit_if_matches(
                    &wait_session_id,
                    &wait_runtime_token,
                    Some(&detail),
                );
            }
            Err(err) => {
                let detail = format!("failed waiting for {} session: {err}", agent.label());
                fail_pending_acp_requests(&wait_pending_requests, &detail);
                let _ = wait_state.handle_runtime_exit_if_matches(
                    &wait_session_id,
                    &wait_runtime_token,
                    Some(&detail),
                );
            }
        });
    }

    Ok(AcpRuntimeHandle {
        agent,
        runtime_id,
        input_tx,
        process,
        turn_lifecycle,
    })
}

/// Handles maybe authenticate ACP runtime.
fn maybe_authenticate_acp_runtime(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    initialize_result: &Value,
    agent: AcpAgent,
    workdir: &str,
) -> Result<()> {
    let Some(method_id) = select_acp_auth_method(initialize_result, agent, workdir) else {
        return Ok(());
    };

    send_acp_json_rpc_request(
        writer,
        pending_requests,
        "authenticate",
        json!({ "methodId": method_id }),
        Duration::from_secs(30),
        agent,
    )?;
    Ok(())
}

/// Upgrades `capabilities.supports_session_load` to `Some(true)`
/// after an observed-working `session/load` (or a
/// wrong-session-id-but-method-exists error). Safe to call when the
/// capabilities bundle has not yet been initialized; inserts a
/// default bundle in that case.
fn note_acp_session_load_supported(runtime_state: &Arc<Mutex<AcpRuntimeState>>) {
    let mut state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    state
        .capabilities
        .get_or_insert_with(AcpCapabilities::default)
        .supports_session_load = Some(true);
}

/// Downgrades legacy optimistic `session/load` probing after a typed
/// method-not-found response. Future prompts start a fresh session directly
/// instead of repeatedly exercising an unsupported method.
fn note_acp_session_load_unsupported(runtime_state: &Arc<Mutex<AcpRuntimeState>>) {
    let mut state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    state
        .capabilities
        .get_or_insert_with(AcpCapabilities::default)
        .supports_session_load = Some(false);
}

/// Records ACP runtime capabilities from initialize.
///
/// Installs an `AcpCapabilities` bundle on the runtime state the first
/// time the initialize response arrives. If the response omits the
/// capability flag entirely (older agents), leaves
/// `supports_session_load = None` so `ensure_acp_session_ready` can
/// probe optimistically — see `AcpCapabilities::session_load_supported_or_unknown`.
fn update_acp_runtime_capabilities(
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    initialize_result: &Value,
) {
    let supports_session_load = acp_supports_session_load(initialize_result);
    let supports_session_resume = acp_supports_session_resume(initialize_result);
    let mut state = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    let capabilities = state.capabilities.get_or_insert_with(AcpCapabilities::default);
    if supports_session_load.is_some() {
        capabilities.supports_session_load = supports_session_load;
    }
    if supports_session_resume.is_some() {
        capabilities.supports_session_resume = supports_session_resume;
    }
}

/// Returns whether ACP initialize reported session/load support.
fn acp_supports_session_load(initialize_result: &Value) -> Option<bool> {
    initialize_result
        .pointer("/agentCapabilities/loadSession")
        .and_then(Value::as_bool)
        .or_else(|| {
            initialize_result
                .pointer("/capabilities/loadSession")
                .and_then(Value::as_bool)
        })
}

/// Returns whether ACP initialize explicitly advertised session/resume.
///
/// ACP v1 represents session capabilities as objects (`"resume": {}`), while
/// a few implementations use booleans. Presence of a non-null object is
/// therefore affirmative; explicit `false` remains authoritative.
fn acp_supports_session_resume(initialize_result: &Value) -> Option<bool> {
    initialize_result
        .pointer("/agentCapabilities/sessionCapabilities/resume")
        .or_else(|| initialize_result.pointer("/capabilities/sessionCapabilities/resume"))
        .map(|value| value.as_bool().unwrap_or(!value.is_null()))
}

/// Handles select ACP auth method.
fn select_acp_auth_method(
    initialize_result: &Value,
    agent: AcpAgent,
    workdir: &str,
) -> Option<String> {
    let methods = initialize_result
        .get("authMethods")
        .and_then(Value::as_array)?;

    let has_method = |target: &str| {
        methods.iter().any(|method| {
            method
                .get("id")
                .and_then(Value::as_str)
                .map(|id| id == target)
                .unwrap_or(false)
        })
    };

    match agent {
        AcpAgent::Cursor => has_method("cursor_login").then_some("cursor_login".to_owned()),
        AcpAgent::Gemini => {
            if gemini_api_key_source().is_some() && has_method("gemini-api-key") {
                Some("gemini-api-key".to_owned())
            } else if gemini_vertex_auth_source(workdir).is_some() && has_method("vertex-ai") {
                Some("vertex-ai".to_owned())
            } else {
                None
            }
        }
        AcpAgent::OpenCode => None,
    }
}

/// Handles ACP prompt command.
fn handle_acp_prompt_command(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    turn_lifecycle: &AcpTurnLifecycle,
    runtime_token: &RuntimeToken,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
    let external_session_id = match ensure_acp_session_ready(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
    ) {
        Ok(external_session_id) => external_session_id,
        Err(err) => {
            set_acp_turn_active(turn_lifecycle, false);
            return Err(err);
        }
    };

    // Direct test/runtime callers may not pass through `deliver_turn_dispatch`;
    // publishing here is idempotent. Production publishes before channel
    // enqueue so stop cannot race this writer-side activation point.
    set_acp_turn_active(turn_lifecycle, true);
    let pending_prompt_request = match start_acp_json_rpc_request(
        writer,
        pending_requests,
        "session/prompt",
        json!({
            "sessionId": external_session_id,
            "prompt": [
                {
                    "type": "text",
                    "text": command.prompt,
                }
            ],
        }),
        agent,
    ) {
        Ok(pending) => pending,
        Err(err) => {
            set_acp_turn_active(turn_lifecycle, false);
            return Err(err);
        }
    };

    let pending_requests = pending_requests.clone();
    let wait_turn_lifecycle = turn_lifecycle.clone();
    let wait_state = state.clone();
    let wait_session_id = session_id.to_owned();
    let wait_runtime_token = runtime_token.clone();
    std::thread::spawn(move || {
        let result = wait_for_acp_json_rpc_response(
            &pending_requests,
            pending_prompt_request,
            "session/prompt",
            None,
            agent,
        );
        set_acp_turn_active(&wait_turn_lifecycle, false);

        match result {
            Ok(_) => {
                if let Err(err) = wait_state
                    .finish_turn_ok_if_runtime_matches(&wait_session_id, &wait_runtime_token)
                {
                    eprintln!(
                        "runtime state warning> failed to finalize ACP turn for session `{}`: {err:#}",
                        wait_session_id
                    );
                }
            }
            Err(err) => {
                let _ = wait_state.handle_runtime_exit_if_matches(
                    &wait_session_id,
                    &wait_runtime_token,
                    Some(&format!(
                        "failed to communicate with {} ACP runtime: {err:#}",
                        agent.label()
                    )),
                );
            }
        }
    });

    Ok(())
}

/// Sends the ACP `session/cancel` notification for the active external session.
/// The protocol does not acknowledge this notification; TermAl proves a clean
/// stop by observing the in-flight prompt settle before the grace deadline.
fn handle_acp_cancel_command(
    writer: &mut impl Write,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
) -> Result<()> {
    let Some(external_session_id) = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .clone()
    else {
        // A stop can race initialization or a naturally completed turn. With
        // no active external session there is nothing on the wire to cancel,
        // and failing the writer loop here would unnecessarily discard a
        // persisted session id that remains safe to resume.
        return Ok(());
    };
    write_acp_json_rpc_message(
        writer,
        &json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": "session/cancel",
            "params": { "sessionId": external_session_id },
        }),
        agent,
    )
}

/// Publishes the in-flight prompt state used by graceful cancellation.
fn set_acp_turn_active(turn_lifecycle: &AcpTurnLifecycle, active: bool) {
    let (active_lock, condvar) = &**turn_lifecycle;
    *active_lock
        .lock()
        .expect("ACP turn lifecycle mutex poisoned") = active;
    condvar.notify_all();
}

/// Waits for the current ACP prompt response to settle within a bounded grace.
fn wait_for_acp_turn_settle(turn_lifecycle: &AcpTurnLifecycle, timeout: Duration) -> bool {
    let (active_lock, condvar) = &**turn_lifecycle;
    let active = active_lock
        .lock()
        .expect("ACP turn lifecycle mutex poisoned");
    if !*active {
        return true;
    }
    let (active, _) = condvar
        .wait_timeout_while(active, timeout, |active| *active)
        .expect("ACP turn lifecycle mutex poisoned while waiting");
    !*active
}

/// Handles ACP session config refresh.
fn handle_acp_session_config_refresh(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
    if runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .is_some()
    {
        if agent != AcpAgent::OpenCode {
            // Cursor and Gemini do not expose a live config-query method.
            // Preserve their historical successful no-op; OpenCode uses the
            // controlled restart path in `refresh_session_model_options`.
            return Ok(());
        }
        bail!(
            "{} ACP config refresh requires a fresh runtime handshake",
            agent.label()
        );
    }
    ensure_acp_session_ready(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
    )?;
    Ok(())
}

/// Ensures ACP session ready.
fn ensure_acp_session_ready(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: &AcpPromptCommand,
) -> Result<String> {
    let (existing_session_id, session_load_allowed, session_resume_allowed) = {
        let state = runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned");
        let capabilities = state.capabilities.as_ref();
        let session_load_allowed = capabilities
            .map(AcpCapabilities::session_load_supported_or_unknown)
            // No capabilities bundle yet means initialize has not
            // reported either way — fall back to the optimistic
            // "try anyway" rule, matching the previous
            // `supports_session_load != Some(false)` semantics.
            .unwrap_or(true);
        let session_resume_allowed = capabilities
            .is_some_and(AcpCapabilities::session_resume_supported);
        (
            state.current_session_id.clone(),
            session_load_allowed,
            session_resume_allowed,
        )
    };
    if let Some(existing_session_id) = existing_session_id {
        return Ok(existing_session_id);
    }
    let mcp_servers = state.termal_delegation_mcp_acp_servers(session_id)?;

    let resume_session_id = command.resume_session_id.as_deref();
    let session_result = if let Some(resume_session_id) =
        resume_session_id.filter(|_| session_resume_allowed)
    {
        let result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/resume",
            json!({
                "sessionId": resume_session_id,
                "cwd": command.cwd,
                "mcpServers": mcp_servers.clone(),
            }),
            Duration::from_secs(30),
            agent,
        );
        match result {
            Ok(value) => (resume_session_id.to_owned(), value),
            Err(err)
                if agent != AcpAgent::OpenCode
                    && acp_json_rpc_response_error(&err)
                        .is_some_and(AcpJsonRpcError::is_invalid_session_identifier) =>
            {
                start_acp_session(
                    writer,
                    pending_requests,
                    agent,
                    &command.cwd,
                    mcp_servers.clone(),
                )?
            }
            Err(err) => return Err(opencode_continuity_error(agent, err)),
        }
    } else if let Some(resume_session_id) =
        resume_session_id.filter(|_| session_load_allowed)
    {
        {
            let mut state = runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned");
            state.is_loading_history = true;
        }
        let result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/load",
            json!({
                "sessionId": resume_session_id,
                "cwd": command.cwd,
                "mcpServers": mcp_servers.clone(),
            }),
            Duration::from_secs(30),
            agent,
        );
        {
            let mut state = runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned");
            state.is_loading_history = false;
        }
        match result {
            Ok(value) => {
                note_acp_session_load_supported(runtime_state);
                (resume_session_id.to_owned(), value)
            }
            Err(err) if agent == AcpAgent::Gemini && is_gemini_invalid_session_load_error(&err) => {
                // Gemini's invalid-session-id error still proves the
                // agent HAS `session/load` — the id just didn't match
                // an existing session. Upgrade the capability so
                // subsequent resumes skip the optimistic fallback.
                note_acp_session_load_supported(runtime_state);
                start_acp_session(
                    writer,
                    pending_requests,
                    agent,
                    &command.cwd,
                    mcp_servers.clone(),
                )?
            }
            Err(err)
                if agent != AcpAgent::OpenCode
                    && acp_json_rpc_response_error(&err)
                        .is_some_and(|error| error.code == Some(-32601)) =>
            {
                note_acp_session_load_unsupported(runtime_state);
                start_acp_session(
                    writer,
                    pending_requests,
                    agent,
                    &command.cwd,
                    mcp_servers.clone(),
                )?
            }
            Err(err) => return Err(opencode_continuity_error(agent, err)),
        }
    } else {
        start_acp_session(
            writer,
            pending_requests,
            agent,
            &command.cwd,
            mcp_servers.clone(),
        )?
    };

    let (external_session_id, session_config) = session_result;
    // Publish continuity as soon as the agent allocates or resumes it.
    // Configuration reconciliation may block or fail independently; stop
    // must still be able to cancel the live session, and a later runtime must
    // be able to resume the conversation rather than orphaning it.
    state.set_external_session_id(session_id, external_session_id.clone())?;
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id = Some(external_session_id.clone());
    if agent == AcpAgent::OpenCode {
        reconcile_opencode_config(
            writer,
            pending_requests,
            state,
            session_id,
            agent,
            &external_session_id,
            command,
            &session_config,
        )?;
    } else {
        configure_acp_session(
            writer,
            pending_requests,
            agent,
            &external_session_id,
            &command.model,
            command.cursor_mode,
            &session_config,
        )?;
        state.sync_session_model_options(
            session_id,
            current_acp_config_option_value(&session_config, "model").or_else(|| {
                let requested = command.model.trim();
                (!requested.is_empty()).then(|| requested.to_owned())
            }),
            acp_model_options(&session_config, agent),
        )?;
    }
    Ok(external_session_id)
}

/// Keeps OpenCode continuity failures visible and non-destructive.
///
/// OpenCode 1.18.8 does not expose a typed invalid-session discriminator for
/// `session/load` or `session/resume`; clearing the stored id from prose or
/// generic service metadata would silently replace the conversation. The
/// failed transcript therefore remains an archive and points at the existing
/// agent-scoped recovery path.
fn opencode_continuity_error(agent: AcpAgent, err: anyhow::Error) -> anyhow::Error {
    if agent == AcpAgent::OpenCode {
        err.context(
            "OpenCode could not resume this conversation. Create a new OpenCode session to start fresh",
        )
    } else {
        err
    }
}

/// Starts a new ACP session.
fn start_acp_session(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    agent: AcpAgent,
    cwd: &str,
    mcp_servers: Value,
) -> Result<(String, Value)> {
    let result = send_acp_json_rpc_request(
        writer,
        pending_requests,
        "session/new",
        json!({
            "cwd": cwd,
            "mcpServers": mcp_servers,
        }),
        Duration::from_secs(30),
        agent,
    )?;
    let created_session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "{} ACP session/new did not return a session id",
                agent.label()
            )
        })?
        .to_owned();
    Ok((created_session_id, result))
}

/// Handles configure ACP session.
fn configure_acp_session(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    agent: AcpAgent,
    session_id: &str,
    requested_model: &str,
    requested_cursor_mode: Option<CursorMode>,
    config_result: &Value,
) -> Result<()> {
    if let Some(model_value) =
        matching_acp_config_option_value(config_result, "model", requested_model)
    {
        let current_value = current_acp_config_option_value(config_result, "model");
        if current_value.as_deref() != Some(model_value.as_str()) {
            send_acp_json_rpc_request(
                writer,
                pending_requests,
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    // ACP `session/set_config_option` identifies the option by
                    // `configId`. `optionId` belongs to `session/request_permission`
                    // and is rejected here with -32602 Invalid params — verified
                    // against opencode 1.18.8, where `configId` succeeds and both
                    // `optionId` and `id` fail. Do not "unify" these two names.
                    "configId": "model",
                    "value": model_value,
                }),
                Duration::from_secs(15),
                agent,
            )?;
        }
    }

    if agent == AcpAgent::Cursor {
        let requested_mode = requested_cursor_mode.unwrap_or_else(default_cursor_mode);
        if let Some(mode_value) =
            matching_acp_config_option_value(config_result, "mode", requested_mode.as_acp_value())
        {
            let current_value = current_acp_config_option_value(config_result, "mode");
            if current_value.as_deref() != Some(mode_value.as_str()) {
                send_acp_json_rpc_request(
                    writer,
                    pending_requests,
                    "session/set_config_option",
                    json!({
                        "sessionId": session_id,
                        // See the `configId` note on the model option above.
                        "configId": "mode",
                        "value": mode_value,
                    }),
                    Duration::from_secs(15),
                    agent,
                )?;
            }
        }
    }
    Ok(())
}

/// Handles ACP message.
fn handle_acp_message(
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    pending_requests: &AcpPendingRequestMap,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    input_tx: &Sender<AcpRuntimeCommand>,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    if let Some(response_id) = message.get("id") {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = acp_request_id_key(response_id);
            let sender = pending_requests
                .lock()
                .expect("ACP pending requests mutex poisoned")
                .remove(&key);
            if let Some(sender) = sender {
                let response = if let Some(result) = message.get("result") {
                    Ok(result.clone())
                } else {
                    Err(AcpResponseError::JsonRpc(parse_acp_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    )))
                };
                let _ = sender.send(response);
            }
            return Ok(());
        }
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        log_unhandled_acp_event(agent, "ACP message missing method", message);
        return Ok(());
    };

    if message.get("id").is_some() {
        return handle_acp_request(message, state, session_id, input_tx, recorder, agent);
    }

    handle_acp_notification(
        method,
        message,
        state,
        session_id,
        runtime_token,
        runtime_state,
        input_tx,
        turn_state,
        recorder,
        agent,
    )
}

/// Handles ACP request.
fn handle_acp_request(
    message: &Value,
    state: &AppState,
    session_id: &str,
    input_tx: &Sender<AcpRuntimeCommand>,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("ACP request missing id"))?;
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ACP request missing method"))?;
    let params = message.get("params").unwrap_or(&Value::Null);

    match method {
        "session/request_permission" => {
            let (description, detail) = summarize_acp_permission_request(params, agent);
            let options = params
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            let approval = AcpPendingApproval {
                allow_once_option_id: find_acp_permission_option(
                    &options,
                    &["allow-once", "allow_once", "allow"],
                ),
                allow_always_option_id: find_acp_permission_option(
                    &options,
                    &["allow-always", "allow_always", "always", "acceptForSession"],
                ),
                reject_option_id: find_acp_permission_option(
                    &options,
                    &["reject-once", "reject_once", "reject", "deny", "decline"],
                ),
                request_id,
            };

            let control_plane_option = delegation_control_plane_capability_for_acp_permission(params)
                .filter(|capability| {
                    state.delegation_control_plane_capability_allowed(session_id, *capability)
                })
                .and_then(|_| {
                    approval
                        .allow_once_option_id
                        .clone()
                        .or_else(|| approval.allow_always_option_id.clone())
                });
            let automatic_option = match control_plane_option {
                Some(option_id) => Some(option_id),
                None => acp_permission_response_option_id(
                    agent,
                    state,
                    session_id,
                    &approval,
                )?,
            };
            if let Some(option_id) = automatic_option {
                input_tx
                    .send(AcpRuntimeCommand::JsonRpcMessage(
                        json_rpc_result_response_message(
                            approval.request_id.clone(),
                            json!({
                                "outcome": {
                                    "outcome": "selected",
                                    "optionId": option_id,
                                }
                            }),
                        ),
                    ))
                    .map_err(|err| {
                        anyhow!(
                            "failed to deliver automatic {} approval response: {err}",
                            agent.label()
                        )
                    })?;
            } else {
                recorder.push_acp_approval(
                    &format!("{} needs approval", agent.label()),
                    &description,
                    &detail,
                    approval,
                )?;
            }
        }
        _ => {
            let _ = input_tx.send(AcpRuntimeCommand::JsonRpcMessage(
                json_rpc_error_response_message(
                    request_id.clone(),
                    -32601,
                    format!("unsupported ACP request `{method}`"),
                ),
            ));
            log_unhandled_acp_event(agent, &format!("unhandled ACP request `{method}`"), message);
        }
    }

    Ok(())
}

/// Handles ACP permission response option ID.
fn acp_permission_response_option_id(
    agent: AcpAgent,
    state: &AppState,
    session_id: &str,
    approval: &AcpPendingApproval,
) -> Result<Option<String>> {
    match agent {
        AcpAgent::Cursor => {
            let cursor_mode = state.cursor_mode(session_id)?;
            Ok(match cursor_mode {
                CursorMode::Agent => approval
                    .allow_once_option_id
                    .clone()
                    .or_else(|| approval.allow_always_option_id.clone()),
                CursorMode::Ask => None,
                CursorMode::Plan => approval.reject_option_id.clone(),
            })
        }
        AcpAgent::Gemini | AcpAgent::OpenCode => Ok(None),
    }
}

/// Handles ACP notification.
fn handle_acp_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    input_tx: &Sender<AcpRuntimeCommand>,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    match method {
        "session/update" => {
            if runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned")
                .is_loading_history
            {
                return Ok(());
            }

            let Some(update) = message.pointer("/params/update") else {
                log_unhandled_acp_event(agent, "ACP session/update missing params.update", message);
                return Ok(());
            };
            if agent == AcpAgent::OpenCode
                && update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .is_some_and(is_acp_config_update_kind)
            {
                record_opencode_config_notification(runtime_state, update);
            }
            handle_acp_session_update(
                update,
                state,
                session_id,
                input_tx,
                turn_state,
                recorder,
                agent,
            )?;
        }
        "error" => {
            let payload = message.get("params").unwrap_or(message);
            let detail = summarize_error(payload);
            state.fail_turn_if_runtime_matches(session_id, runtime_token, &detail)?;
        }
        _ => {
            log_unhandled_acp_event(
                agent,
                &format!("unhandled ACP notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

/// Returns whether an ACP session update carries dynamic config options.
///
/// Both the reader-side OpenCode wake-up path and the writer-side dispatcher
/// must use this predicate so a newly supported protocol spelling cannot wake
/// only one half of the model-dependent config flow.
fn is_acp_config_update_kind(kind: &str) -> bool {
    matches!(kind, "config_options_update" | "config_update")
}

/// Handles ACP session update.
fn handle_acp_session_update(
    update: &Value,
    state: &AppState,
    session_id: &str,
    input_tx: &Sender<AcpRuntimeCommand>,
    turn_state: &mut AcpTurnState,
    recorder: &mut SessionRecorder,
    agent: AcpAgent,
) -> Result<()> {
    let Some(update_type) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return Ok(());
    };

    match update_type {
        "agent_thought_chunk" => {
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                turn_state.thinking_buffer.push_str(text);
            }
        }
        "agent_message_chunk" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            let next_message_id = update
                .get("messageId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if turn_state.current_agent_message_id != next_message_id {
                recorder.finish_streaming_text()?;
                turn_state.current_agent_message_id = next_message_id;
            }
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                recorder.text_delta(text)?;
            }
        }
        "tool_call" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            recorder.finish_streaming_text()?;
            if let Some((key, command)) = acp_tool_identity(update) {
                recorder.command_started(&key, &command)?;
            }
        }
        "tool_call_update" => {
            finish_acp_thinking(recorder, turn_state, agent)?;
            if let Some((key, command)) = acp_tool_identity(update) {
                match update.get("status").and_then(Value::as_str) {
                    Some("pending") | Some("in_progress") => {
                        recorder.command_started(&key, &command)?;
                    }
                    Some("completed") | Some("failed") | Some("error") => {
                        recorder.command_completed(
                            &key,
                            &command,
                            &summarize_acp_tool_output(update),
                            acp_tool_status(update),
                        )?;
                    }
                    _ => {}
                }
            }
        }
        kind if is_acp_config_update_kind(kind) => {
            if agent == AcpAgent::OpenCode {
                input_tx
                    .send(AcpRuntimeCommand::ReconcileOpenCodeConfig {
                        config_result: update.clone(),
                    })
                    .map_err(|err| {
                        anyhow!("failed to queue OpenCode config reconciliation: {err}")
                    })?;
            } else {
                state.sync_session_model_options(
                    session_id,
                    current_acp_config_option_value(update, "model"),
                    acp_model_options(update, agent),
                )?;
                if agent == AcpAgent::Cursor {
                    state.sync_session_cursor_mode(session_id, acp_cursor_mode(update))?;
                }
            }
        }
        "available_commands_update" => {}
        "mode_update" => {
            if agent == AcpAgent::Cursor {
                state.sync_session_cursor_mode(session_id, acp_cursor_mode(update))?;
            }
        }
        other => {
            log_unhandled_acp_event(
                agent,
                &format!("unhandled ACP session/update `{other}`"),
                update,
            );
        }
    }

    Ok(())
}

/// Finishes ACP turn state.
fn finish_acp_turn_state(
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    agent: AcpAgent,
) -> Result<()> {
    finish_acp_thinking(recorder, turn_state, agent)?;
    recorder.finish_streaming_text()
}

/// Finishes ACP thinking.
fn finish_acp_thinking(
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    agent: AcpAgent,
) -> Result<()> {
    if turn_state.thinking_buffer.trim().is_empty() {
        turn_state.thinking_buffer.clear();
        return Ok(());
    }

    let lines = turn_state
        .thinking_buffer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    turn_state.thinking_buffer.clear();
    if lines.is_empty() {
        return Ok(());
    }
    recorder.push_thinking(&format!("{} is thinking", agent.label()), lines)
}

/// Handles ACP tool identity.
fn acp_tool_identity(update: &Value) -> Option<(String, String)> {
    let key = update.get("toolCallId").and_then(Value::as_str)?.to_owned();
    let command = update
        .pointer("/rawInput/command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let title = update.get("title").and_then(Value::as_str)?;
            let kind = update.get("kind").and_then(Value::as_str);
            Some(match kind {
                Some(kind) => format!("{title} ({kind})"),
                None => title.to_owned(),
            })
        })
        .unwrap_or_else(|| "Tool call".to_owned());
    Some((key, command))
}

/// Summarizes ACP tool output.
fn summarize_acp_tool_output(update: &Value) -> String {
    let Some(raw_output) = update.get("rawOutput") else {
        return String::new();
    };

    let stdout = raw_output
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = raw_output
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !stdout.is_empty() || !stderr.is_empty() {
        if stdout.is_empty() {
            return stderr.to_owned();
        }
        if stderr.is_empty() {
            return stdout.to_owned();
        }
        return format!("{stdout}\n{stderr}");
    }

    serde_json::to_string_pretty(raw_output).unwrap_or_else(|_| raw_output.to_string())
}

/// Handles ACP tool status.
fn acp_tool_status(update: &Value) -> CommandStatus {
    match update.get("status").and_then(Value::as_str) {
        Some("completed") => {
            if update
                .pointer("/rawOutput/exitCode")
                .and_then(Value::as_i64)
                == Some(0)
            {
                CommandStatus::Success
            } else {
                CommandStatus::Error
            }
        }
        Some("failed") | Some("error") => CommandStatus::Error,
        _ => CommandStatus::Running,
    }
}

/// Parses cursor mode ACP value.
fn parse_cursor_mode_acp_value(value: &str) -> Option<CursorMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "agent" => Some(CursorMode::Agent),
        "ask" => Some(CursorMode::Ask),
        "plan" => Some(CursorMode::Plan),
        _ => None,
    }
}

/// Handles ACP cursor mode.
fn acp_cursor_mode(update: &Value) -> Option<CursorMode> {
    current_acp_config_option_value(update, "mode")
        .or_else(|| {
            update
                .get("mode")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            update
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .and_then(|value| parse_cursor_mode_acp_value(&value))
}

/// Finds ACP permission option.
fn find_acp_permission_option(options: &[Value], hints: &[&str]) -> Option<String> {
    let normalized_hints = hints
        .iter()
        .map(|hint| normalize_acp_permission_hint(hint))
        .collect::<Vec<_>>();
    let normalized_options = options
        .iter()
        .filter_map(|option| {
            let option_id = option
                .get("optionId")
                .or_else(|| option.get("id"))
                .and_then(Value::as_str)?;
            let candidates = [
                Some(option_id),
                option.get("kind").and_then(Value::as_str),
                option.get("name").and_then(Value::as_str),
            ]
            .into_iter()
            .flatten()
            .map(normalize_acp_permission_hint)
            .collect::<Vec<_>>();
            Some((option_id, candidates))
        })
        .collect::<Vec<_>>();

    // ACP's typed `kind` is authoritative. Search each hint across every
    // option before falling back to fuzzy names so a generic legacy hint such
    // as `allow` cannot bind an earlier `allow_always` option when
    // `allow_once` is present later in the array.
    for hint in &normalized_hints {
        if let Some((option_id, _)) = normalized_options
            .iter()
            .find(|(_, candidates)| candidates.iter().any(|candidate| candidate == hint))
        {
            return Some((*option_id).to_owned());
        }
    }
    None
}

/// Normalizes ACP permission option identifiers across agents.
fn normalize_acp_permission_hint(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
}

/// Summarizes a permission request without losing OpenCode's structured tool
/// context. Cursor and Gemini historically send flat `toolName` +
/// `description` fields; ACP v1 agents such as OpenCode send a nested
/// `toolCall` carrying the title, kind, content, and locations instead.
fn summarize_acp_permission_request(
    params: &Value,
    agent: AcpAgent,
) -> (String, String) {
    const MAX_TITLE_CHARS: usize = 1_000;
    const MAX_TOOL_NAME_CHARS: usize = 200;
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let raw_tool_name = params
        .get("toolName")
        .and_then(Value::as_str)
        .or_else(|| tool_call.get("title").and_then(Value::as_str))
        .or_else(|| tool_call.get("kind").and_then(Value::as_str))
        .unwrap_or("Tool");
    let tool_name = bounded_acp_approval_text(raw_tool_name, MAX_TOOL_NAME_CHARS, "Tool");
    let raw_description = params
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| tool_call.get("title").and_then(Value::as_str))
        .unwrap_or(&tool_name);
    let description =
        bounded_acp_approval_text(raw_description, MAX_TITLE_CHARS, &tool_name);

    let mut details = vec![format!(
        "{} requested approval for `{tool_name}`.",
        agent.label()
    )];
    if let Some(kind) = tool_call.get("kind").and_then(Value::as_str) {
        let kind = bounded_acp_approval_text(kind, MAX_TOOL_NAME_CHARS, "tool");
        if !kind.eq_ignore_ascii_case(&tool_name) {
            details.push(format!("Kind: `{kind}`"));
        }
    }
    for (label, value) in [
        ("Content", tool_call.get("content")),
        ("Locations", tool_call.get("locations")),
    ] {
        let Some(value) = value.filter(|value| !value.is_null()) else {
            continue;
        };
        let rendered =
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        details.push(format!("{label}:\n{}", truncate_acp_approval_detail(&rendered)));
    }

    (
        description,
        truncate_acp_approval_detail(&details.join("\n\n")),
    )
}

/// Bounds single-line approval metadata before transcript persistence.
fn bounded_acp_approval_text(value: &str, max_chars: usize, fallback: &str) -> String {
    let bounded = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>();
    let bounded = bounded.trim();
    if bounded.is_empty() {
        fallback.to_owned()
    } else {
        bounded.to_owned()
    }
}

/// Bounds agent-provided approval detail before storing it in the transcript.
fn truncate_acp_approval_detail(value: &str) -> String {
    const MAX_DETAIL_CHARS: usize = 8_000;
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(MAX_DETAIL_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n…")
    } else {
        truncated
    }
}

/// Handles send ACP JSON RPC request.
fn send_acp_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Duration,
    agent: AcpAgent,
) -> Result<Value> {
    send_acp_json_rpc_request_inner(
        writer,
        pending_requests,
        method,
        params,
        Some(timeout),
        agent,
    )
}

/// Handles send ACP JSON RPC request without timeout.
#[cfg(test)]
fn send_acp_json_rpc_request_without_timeout(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    agent: AcpAgent,
) -> Result<Value> {
    send_acp_json_rpc_request_inner(writer, pending_requests, method, params, None, agent)
}

/// Starts ACP JSON RPC request.
fn start_acp_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    agent: AcpAgent,
) -> Result<PendingAcpJsonRpcRequest> {
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .insert(request_id.clone(), tx);

    if let Err(err) = write_acp_json_rpc_message(
        writer,
        &json_rpc_request_message(request_id.clone(), method, params),
        agent,
    ) {
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .remove(&request_id);
        return Err(anyhow!(AcpResponseError::Transport(format!("{err:#}"))));
    }

    Ok(PendingAcpJsonRpcRequest {
        request_id,
        response_rx: rx,
    })
}

/// Handles send ACP JSON RPC request inner.
fn send_acp_json_rpc_request_inner(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Option<Duration>,
    agent: AcpAgent,
) -> Result<Value> {
    let pending_request =
        start_acp_json_rpc_request(writer, pending_requests, method, params, agent)?;
    wait_for_acp_json_rpc_response(pending_requests, pending_request, method, timeout, agent)
}

/// Returns a typed ACP JSON-RPC rejection from an error chain.
fn acp_json_rpc_response_error(err: &anyhow::Error) -> Option<&AcpJsonRpcError> {
    err.chain().find_map(|source| {
        source
            .downcast_ref::<AcpResponseError>()
            .and_then(AcpResponseError::as_json_rpc)
    })
}

/// Returns whether an ACP request failed at the transport boundary rather than
/// receiving a protocol-level JSON-RPC rejection.
fn acp_error_is_transport_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        matches!(
            source.downcast_ref::<AcpResponseError>(),
            Some(AcpResponseError::Transport(_))
        )
    })
}

/// Handles wait for ACP JSON RPC response.
fn wait_for_acp_json_rpc_response(
    pending_requests: &AcpPendingRequestMap,
    pending_request: PendingAcpJsonRpcRequest,
    method: &str,
    timeout: Option<Duration>,
    agent: AcpAgent,
) -> Result<Value> {
    let PendingAcpJsonRpcRequest {
        request_id,
        response_rx,
    } = pending_request;

    let response = match timeout {
        Some(timeout) => match response_rx.recv_timeout(timeout) {
            Ok(response) => response,
            Err(err) => {
                pending_requests
                    .lock()
                    .expect("ACP pending requests mutex poisoned")
                    .remove(&request_id);
                return Err(anyhow!(AcpResponseError::Transport(format!(
                    "timed out waiting for {} ACP response to `{method}`: {err}",
                    agent.label()
                ))));
            }
        },
        None => match response_rx.recv() {
            Ok(response) => response,
            Err(err) => {
                pending_requests
                    .lock()
                    .expect("ACP pending requests mutex poisoned")
                    .remove(&request_id);
                return Err(anyhow!(AcpResponseError::Transport(format!(
                    "failed waiting for {} ACP response to `{method}`: {err}",
                    agent.label()
                ))));
            }
        },
    };

    response.map_err(|err| anyhow!(err))
}

/// Marks pending ACP requests as failed.
fn fail_pending_acp_requests(pending_requests: &AcpPendingRequestMap, detail: &str) {
    let senders = pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .drain()
        .map(|(_, sender)| sender)
        .collect::<Vec<_>>();

    for sender in senders {
        let _ = sender.send(Err(AcpResponseError::Transport(detail.to_owned())));
    }
}

/// Writes ACP JSON RPC message.
fn write_acp_json_rpc_message(
    writer: &mut impl Write,
    message: &Value,
    agent: AcpAgent,
) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)
        .with_context(|| format!("failed to encode {} ACP message", agent.label()))?;
    writer
        .write_all(b"\n")
        .with_context(|| format!("failed to write {} ACP message delimiter", agent.label()))?;
    writer
        .flush()
        .with_context(|| format!("failed to flush {} ACP stdin", agent.label()))
}

/// Handles ACP request ID key.
fn acp_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

/// Parses an ACP JSON RPC error.
fn parse_acp_json_rpc_error(error: &Value) -> AcpJsonRpcError {
    AcpJsonRpcError {
        code: error.get("code").and_then(Value::as_i64),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| summarize_error(error)),
        data: error.get("data").cloned(),
    }
}

/// Returns whether a Gemini session/load failure should fall back to session/new.
fn is_gemini_invalid_session_load_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<AcpResponseError>()
        .and_then(AcpResponseError::as_json_rpc)
        .is_some_and(AcpJsonRpcError::is_invalid_session_identifier)
        || err
            .chain()
            .any(|chain_err| chain_err.to_string().contains("Invalid session identifier"))
}

/// Returns whether ACP error data explicitly reports an invalid session identifier.
fn acp_error_data_indicates_invalid_session_identifier(value: &Value) -> bool {
    acp_error_data_indicates_invalid_session_identifier_with_depth(value, 10)
}

/// Returns whether ACP error data explicitly reports an invalid session identifier.
fn acp_error_data_indicates_invalid_session_identifier_with_depth(
    value: &Value,
    remaining_depth: u8,
) -> bool {
    match value {
        Value::String(reason) => acp_reason_indicates_invalid_session_identifier(reason),
        _ if remaining_depth == 0 => false,
        Value::Array(entries) => entries.iter().any(|entry| {
            acp_error_data_indicates_invalid_session_identifier_with_depth(
                entry,
                remaining_depth - 1,
            )
        }),
        Value::Object(fields) => {
            for key in ["reason", "type", "error", "code", "details"] {
                if fields.get(key).is_some_and(|entry| {
                    acp_error_data_indicates_invalid_session_identifier_with_depth(
                        entry,
                        remaining_depth - 1,
                    )
                }) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// Normalizes reason strings used for invalid-session identifiers across ACP agents.
fn acp_reason_indicates_invalid_session_identifier(reason: &str) -> bool {
    // Gemini uses these typed reason values in its invalid-session envelopes.
    // OpenCode deliberately does not use this classifier: 1.18.8 exposes no
    // typed invalid-session discriminator safe enough to authorize clearing
    // persisted conversation continuity.
    let normalized = reason
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "invalidsessionidentifier" | "invalidsessionid" | "invalidsession"
    )
}

fn log_unhandled_acp_event(agent: AcpAgent, context: &str, message: &Value) {
    eprintln!(
        "{} acp diagnostic> {context}: {message}",
        agent.label().to_lowercase()
    );
}
