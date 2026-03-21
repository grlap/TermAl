fn resolve_source_codex_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = resolve_home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".codex"))
}

fn resolve_termal_data_dir(default_workdir: &str) -> PathBuf {
    let base = resolve_home_dir().unwrap_or_else(|| PathBuf::from(default_workdir));
    base.join(".termal")
}

fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn resolve_termal_codex_home(default_workdir: &str, scope: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir)
        .join("codex-home")
        .join(scope)
}

fn prepare_termal_codex_home(default_workdir: &str, scope: &str) -> Result<PathBuf> {
    let target_home = resolve_termal_codex_home(default_workdir, scope);
    fs::create_dir_all(&target_home)
        .with_context(|| format!("failed to create `{}`", target_home.display()))?;
    if let Ok(source_home) = resolve_source_codex_home_dir() {
        seed_termal_codex_home_from(&source_home, &target_home)?;
    }
    Ok(target_home)
}

fn seed_termal_codex_home_from(source_home: &FsPath, target_home: &FsPath) -> Result<()> {
    if !source_home.exists() {
        return Ok(());
    }

    let source_home = fs::canonicalize(source_home).unwrap_or_else(|_| source_home.to_path_buf());
    let target_home = fs::canonicalize(target_home).unwrap_or_else(|_| target_home.to_path_buf());

    if source_home == target_home {
        return Ok(());
    }

    for name in [
        "auth.json",
        "config.toml",
        "models_cache.json",
        ".codex-global-state.json",
    ] {
        sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
    }

    for name in ["rules", "memories", "skills"] {
        sync_codex_home_entry(&source_home.join(name), &target_home.join(name))?;
    }

    Ok(())
}

fn sync_codex_home_entry(source: &FsPath, target: &FsPath) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }

    let metadata =
        fs::metadata(source).with_context(|| format!("failed to read `{}`", source.display()))?;

    if metadata.is_dir() {
        sync_codex_home_directory(source, target)
    } else if metadata.is_file() {
        sync_codex_home_file(source, target, &metadata)
    } else {
        Ok(())
    }
}

fn sync_codex_home_directory(source: &FsPath, target: &FsPath) -> Result<()> {
    if target.is_file() {
        fs::remove_file(target)
            .with_context(|| format!("failed to remove `{}`", target.display()))?;
    }

    fs::create_dir_all(target)
        .with_context(|| format!("failed to create `{}`", target.display()))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read `{}`", source.display()))?
    {
        let entry = entry?;
        sync_codex_home_entry(&entry.path(), &target.join(entry.file_name()))?;
    }

    Ok(())
}

fn sync_codex_home_file(
    source: &FsPath,
    target: &FsPath,
    source_metadata: &fs::Metadata,
) -> Result<()> {
    let should_copy = match fs::metadata(target) {
        Ok(target_metadata) => {
            if target_metadata.is_dir() {
                fs::remove_dir_all(target)
                    .with_context(|| format!("failed to remove `{}`", target.display()))?;
                true
            } else if source_metadata.len() != target_metadata.len() {
                true
            } else {
                match (
                    source_metadata.modified().ok(),
                    target_metadata.modified().ok(),
                ) {
                    (Some(source_modified), Some(target_modified)) => {
                        source_modified > target_modified
                    }
                    _ => false,
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read `{}`", target.display()));
        }
    };

    if !should_copy {
        return Ok(());
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy `{}` to `{}`",
            source.display(),
            target.display()
        )
    })?;
    fs::set_permissions(target, source_metadata.permissions())
        .with_context(|| format!("failed to update permissions on `{}`", target.display()))?;
    Ok(())
}

#[derive(Clone)]
enum CodexRuntimeCommand {
    Prompt {
        session_id: String,
        command: CodexPromptCommand,
    },
    JsonRpcRequest {
        method: String,
        params: Value,
        timeout: Duration,
        response_tx: Sender<std::result::Result<Value, String>>,
    },
    JsonRpcResponse {
        response: CodexJsonRpcResponseCommand,
    },
    InterruptTurn {
        response_tx: Sender<std::result::Result<(), String>>,
        thread_id: String,
        turn_id: String,
    },
    RefreshModelList {
        response_tx: Sender<std::result::Result<Vec<SessionModelOption>, String>>,
    },
}

#[derive(Clone)]
struct CodexPromptCommand {
    approval_policy: CodexApprovalPolicy,
    attachments: Vec<PromptImageAttachment>,
    cwd: String,
    model: String,
    prompt: String,
    reasoning_effort: CodexReasoningEffort,
    resume_thread_id: Option<String>,
    sandbox_mode: CodexSandboxMode,
}

#[derive(Clone)]
struct CodexJsonRpcResponseCommand {
    request_id: Value,
    result: Value,
}

#[derive(Clone)]
enum CodexApprovalKind {
    CommandExecution,
    FileChange,
    Permissions { requested_permissions: Value },
}

#[derive(Clone)]
struct CodexPendingApproval {
    kind: CodexApprovalKind,
    request_id: Value,
}

#[derive(Clone)]
struct CodexPendingUserInput {
    questions: Vec<UserInputQuestion>,
    request_id: Value,
}

#[derive(Clone)]
struct CodexPendingMcpElicitation {
    request: McpElicitationRequestPayload,
    request_id: Value,
}

#[derive(Clone)]
struct CodexPendingAppRequest {
    request_id: Value,
}

#[derive(Clone)]
struct ClaudePromptCommand {
    attachments: Vec<PromptImageAttachment>,
    text: String,
}

#[derive(Clone)]
enum ClaudeRuntimeCommand {
    Prompt(ClaudePromptCommand),
    PermissionResponse(ClaudePermissionDecision),
    SetModel(String),
    SetPermissionMode(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromptImageAttachment {
    data: String,
    metadata: MessageImageAttachment,
}

#[derive(Clone)]
struct ClaudePendingApproval {
    permission_mode_for_session: Option<String>,
    request_id: String,
    tool_input: Value,
}

#[derive(Clone)]
enum ClaudePermissionDecision {
    Allow {
        request_id: String,
        updated_input: Value,
    },
    Deny {
        request_id: String,
        message: String,
    },
}

enum ClaudeControlRequestAction {
    QueueApproval {
        title: String,
        command: String,
        detail: String,
        approval: ClaudePendingApproval,
    },
    Respond(ClaudePermissionDecision),
}

#[derive(Clone)]
enum AcpRuntimeCommand {
    Prompt(AcpPromptCommand),
    JsonRpcMessage(Value),
    RefreshSessionConfig {
        command: AcpPromptCommand,
        response_tx: Sender<std::result::Result<(), String>>,
    },
}

#[derive(Clone, Copy, Default)]
struct AcpLaunchOptions {
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

#[derive(Clone)]
struct AcpPromptCommand {
    cwd: String,
    cursor_mode: Option<CursorMode>,
    model: String,
    prompt: String,
    resume_session_id: Option<String>,
}

#[derive(Clone)]
struct AcpPendingApproval {
    allow_once_option_id: Option<String>,
    allow_always_option_id: Option<String>,
    reject_option_id: Option<String>,
    request_id: Value,
}

#[derive(Default)]
struct AcpRuntimeState {
    current_session_id: Option<String>,
    is_loading_history: bool,
}

#[derive(Default)]
struct AcpTurnState {
    current_agent_message_id: Option<String>,
    thinking_buffer: String,
}

#[derive(Clone)]
struct TurnConfig {
    codex_approval_policy: Option<CodexApprovalPolicy>,
    codex_reasoning_effort: Option<CodexReasoningEffort>,
    codex_sandbox_mode: Option<CodexSandboxMode>,
    agent: Agent,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    cwd: String,
    model: String,
    prompt: String,
    external_session_id: Option<String>,
}

enum TurnDispatch {
    PersistentClaude {
        command: ClaudePromptCommand,
        sender: Sender<ClaudeRuntimeCommand>,
        session_id: String,
    },
    PersistentCodex {
        command: CodexPromptCommand,
        sender: Sender<CodexRuntimeCommand>,
        session_id: String,
    },
    PersistentAcp {
        command: AcpPromptCommand,
        sender: Sender<AcpRuntimeCommand>,
        session_id: String,
    },
}

enum DispatchTurnResult {
    Dispatched(TurnDispatch),
    Queued,
}

type CodexPendingRequestMap =
    Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, String>>>>>;
type AcpPendingRequestMap = Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, String>>>>>;

struct PendingAcpJsonRpcRequest {
    request_id: String,
    response_rx: mpsc::Receiver<std::result::Result<Value, String>>,
}

struct PendingSubagentResult {
    title: String,
    summary: String,
    conversation_id: Option<String>,
    turn_id: Option<String>,
}

#[derive(Default)]
struct CodexTurnState {
    current_agent_message_id: Option<String>,
    streamed_agent_message_text_by_item_id: HashMap<String, String>,
    streamed_agent_message_item_ids: HashSet<String>,
    pending_subagent_results: Vec<PendingSubagentResult>,
    assistant_output_started: bool,
    first_visible_assistant_message_id: Option<String>,
}

#[derive(Default)]
struct SessionRecorderState {
    command_messages: HashMap<String, String>,
    parallel_agents_messages: HashMap<String, String>,
    streaming_text_message_id: Option<String>,
}

#[derive(Default)]
struct SharedCodexSessionState {
    recorder: SessionRecorderState,
    thread_id: Option<String>,
    turn_id: Option<String>,
    turn_state: CodexTurnState,
}

type SharedCodexSessionMap = Arc<Mutex<HashMap<String, SharedCodexSessionState>>>;
type SharedCodexThreadMap = Arc<Mutex<HashMap<String, String>>>;

fn spawn_acp_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    agent: AcpAgent,
    gemini_approval_mode: Option<GeminiApprovalMode>,
) -> Result<AcpRuntimeHandle> {
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = agent.command(AcpLaunchOptions {
        gemini_approval_mode,
    })?;
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

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_runtime_state = runtime_state.clone();
        let writer_runtime_token = RuntimeToken::Acp(runtime_id.clone());
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
                maybe_authenticate_acp_runtime(&mut stdin, &writer_pending_requests, &result, agent)
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
                        &writer_runtime_token,
                        agent,
                        prompt,
                    ),
                    AcpRuntimeCommand::JsonRpcMessage(message) => {
                        write_acp_json_rpc_message(&mut stdin, &message, agent)
                    }
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
                eprintln!("{} stderr> {line}", agent.label().to_lowercase());
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_pending_requests = pending_requests.clone();
        let wait_runtime_token = RuntimeToken::Acp(runtime_id.clone());
        std::thread::spawn(move || {
            match wait_process.wait() {
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
            }
        });
    }

    Ok(AcpRuntimeHandle {
        agent,
        runtime_id,
        input_tx,
        process,
    })
}

fn maybe_authenticate_acp_runtime(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    initialize_result: &Value,
    agent: AcpAgent,
) -> Result<()> {
    let Some(method_id) = select_acp_auth_method(initialize_result, agent) else {
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

fn select_acp_auth_method(initialize_result: &Value, agent: AcpAgent) -> Option<String> {
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
            if std::env::var_os("GEMINI_API_KEY").is_some() && has_method("gemini-api-key") {
                Some("gemini-api-key".to_owned())
            } else if (std::env::var_os("GOOGLE_GENAI_USE_VERTEXAI").is_some()
                || std::env::var_os("GOOGLE_GENAI_USE_GCA").is_some())
                && has_method("vertex-ai")
            {
                Some("vertex-ai".to_owned())
            } else {
                None
            }
        }
    }
}

fn handle_acp_prompt_command(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    runtime_token: &RuntimeToken,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
    let external_session_id = ensure_acp_session_ready(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
    )?;

    let pending_prompt_request = start_acp_json_rpc_request(
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
    )?;

    let pending_requests = pending_requests.clone();
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

        match result {
            Ok(_) => {
                let _ = wait_state
                    .finish_turn_ok_if_runtime_matches(&wait_session_id, &wait_runtime_token);
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

fn handle_acp_session_config_refresh(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: AcpPromptCommand,
) -> Result<()> {
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

fn ensure_acp_session_ready(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: &AcpPromptCommand,
) -> Result<String> {
    if let Some(existing_session_id) = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .clone()
    {
        return Ok(existing_session_id);
    }

    let session_result = if let Some(resume_session_id) = command.resume_session_id.as_deref() {
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
                "mcpServers": [],
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
        result.map(|value| (resume_session_id.to_owned(), value))?
    } else {
        let result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/new",
            json!({
                "cwd": command.cwd,
                "mcpServers": [],
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
        (created_session_id, result)
    };

    let (external_session_id, session_config) = session_result;
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
        acp_model_options(&session_config),
    )?;
    state.set_external_session_id(session_id, external_session_id.clone())?;
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id = Some(external_session_id.clone());
    Ok(external_session_id)
}

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
                    "optionId": "model",
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
                        "optionId": "mode",
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

fn current_acp_config_option_value(config_result: &Value, option_id: &str) -> Option<String> {
    acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))
        .and_then(|entry| entry.get("currentValue").and_then(Value::as_str))
        .map(str::to_owned)
}

fn matching_acp_config_option_value(
    config_result: &Value,
    option_id: &str,
    requested_value: &str,
) -> Option<String> {
    let requested = requested_value.trim();
    if requested.is_empty() {
        return None;
    }
    let requested_normalized = requested.to_ascii_lowercase();
    let option = acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))?;
    let options = option.get("options").and_then(Value::as_array)?;
    options.iter().find_map(|entry| {
        let value = entry.get("value").and_then(Value::as_str)?;
        let name = entry
            .get("name")
            .or_else(|| entry.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let value_normalized = value.to_ascii_lowercase();
        let name_normalized = name.to_ascii_lowercase();
        if value_normalized == requested_normalized || name_normalized == requested_normalized {
            Some(value.to_owned())
        } else {
            None
        }
    })
}

fn acp_model_options(config_result: &Value) -> Vec<SessionModelOption> {
    let Some(option) = acp_config_options(config_result).and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some("model"))
    }) else {
        return Vec::new();
    };

    option
        .get("options")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let value = entry.get("value").and_then(Value::as_str)?.trim();
                    if value.is_empty() {
                        return None;
                    }
                    let label = entry
                        .get("name")
                        .or_else(|| entry.get("label"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|label| !label.is_empty())
                        .unwrap_or(value);
                    let description = entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|description| !description.is_empty())
                        .map(str::to_owned);
                    Some(SessionModelOption {
                        label: label.to_owned(),
                        value: value.to_owned(),
                        description,
                        badges: Vec::new(),
                        supported_claude_effort_levels: Vec::new(),
                        default_reasoning_effort: None,
                        supported_reasoning_efforts: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn acp_config_options(config_result: &Value) -> Option<&Vec<Value>> {
    config_result
        .get("configOptions")
        .or_else(|| config_result.get("config_options"))
        .and_then(Value::as_array)
}

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
                    Err(summarize_acp_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    ))
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
        turn_state,
        recorder,
        agent,
    )
}

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
            let tool_name = params
                .get("toolName")
                .and_then(Value::as_str)
                .unwrap_or("Tool");
            let description = params
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or(tool_name);
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

            if let Some(option_id) =
                acp_permission_response_option_id(agent, state, session_id, &approval)?
            {
                input_tx
                    .send(AcpRuntimeCommand::JsonRpcMessage(json!({
                        "id": approval.request_id.clone(),
                        "result": {
                            "outcome": {
                                "outcome": "selected",
                                "optionId": option_id,
                            }
                        }
                    })))
                    .map_err(|err| {
                        anyhow!(
                            "failed to deliver automatic {} approval response: {err}",
                            agent.label()
                        )
                    })?;
            } else {
                recorder.push_acp_approval(
                    &format!("{} needs approval", agent.label()),
                    description,
                    &format!("{} requested approval for `{tool_name}`.", agent.label()),
                    approval,
                )?;
            }
        }
        _ => {
            let _ = input_tx.send(AcpRuntimeCommand::JsonRpcMessage(json!({
                "id": request_id,
                "error": {
                    "code": -32601,
                    "message": format!("unsupported ACP request `{method}`"),
                }
            })));
            log_unhandled_acp_event(agent, &format!("unhandled ACP request `{method}`"), message);
        }
    }

    Ok(())
}

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
        AcpAgent::Gemini => Ok(None),
    }
}

fn handle_acp_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
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
            handle_acp_session_update(update, state, session_id, turn_state, recorder, agent)?;
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

fn handle_acp_session_update(
    update: &Value,
    state: &AppState,
    session_id: &str,
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
        "config_options_update" | "config_update" => {
            state.sync_session_model_options(
                session_id,
                current_acp_config_option_value(update, "model"),
                acp_model_options(update),
            )?;
            if agent == AcpAgent::Cursor {
                state.sync_session_cursor_mode(session_id, acp_cursor_mode(update))?;
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

fn finish_acp_turn_state(
    recorder: &mut SessionRecorder,
    turn_state: &mut AcpTurnState,
    agent: AcpAgent,
) -> Result<()> {
    finish_acp_thinking(recorder, turn_state, agent)?;
    recorder.finish_streaming_text()
}

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

fn parse_cursor_mode_acp_value(value: &str) -> Option<CursorMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "agent" => Some(CursorMode::Agent),
        "ask" => Some(CursorMode::Ask),
        "plan" => Some(CursorMode::Plan),
        _ => None,
    }
}

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

fn find_acp_permission_option(options: &[Value], hints: &[&str]) -> Option<String> {
    options.iter().find_map(|option| {
        let option_id = option
            .get("optionId")
            .or_else(|| option.get("id"))
            .and_then(Value::as_str)?;
        let normalized = option_id.to_ascii_lowercase();
        hints
            .iter()
            .any(|hint| normalized.contains(&hint.to_ascii_lowercase()))
            .then_some(option_id.to_owned())
    })
}

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

fn send_acp_json_rpc_request_without_timeout(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    method: &str,
    params: Value,
    agent: AcpAgent,
) -> Result<Value> {
    send_acp_json_rpc_request_inner(writer, pending_requests, method, params, None, agent)
}

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
        &json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }),
        agent,
    ) {
        pending_requests
            .lock()
            .expect("ACP pending requests mutex poisoned")
            .remove(&request_id);
        return Err(err);
    }

    Ok(PendingAcpJsonRpcRequest {
        request_id,
        response_rx: rx,
    })
}

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
                return Err(anyhow!(
                    "timed out waiting for {} ACP response to `{method}`: {err}",
                    agent.label()
                ));
            }
        },
        None => match response_rx.recv() {
            Ok(response) => response,
            Err(err) => {
                pending_requests
                    .lock()
                    .expect("ACP pending requests mutex poisoned")
                    .remove(&request_id);
                return Err(anyhow!(
                    "failed waiting for {} ACP response to `{method}`: {err}",
                    agent.label()
                ));
            }
        },
    };

    response.map_err(|err| anyhow!(err))
}

fn fail_pending_acp_requests(pending_requests: &AcpPendingRequestMap, detail: &str) {
    let senders = pending_requests
        .lock()
        .expect("ACP pending requests mutex poisoned")
        .drain()
        .map(|(_, sender)| sender)
        .collect::<Vec<_>>();

    for sender in senders {
        let _ = sender.send(Err(detail.to_owned()));
    }
}

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

fn acp_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

fn summarize_acp_json_rpc_error(error: &Value) -> String {
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    summarize_error(error)
}

fn log_unhandled_acp_event(agent: AcpAgent, context: &str, message: &Value) {
    eprintln!(
        "{} acp diagnostic> {context}: {message}",
        agent.label().to_lowercase()
    );
}

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

fn spawn_shared_codex_runtime(state: AppState) -> Result<SharedCodexRuntime> {
    // Codex threads carry their own cwd, so one shared app-server can serve all sessions.
    let codex_home = prepare_termal_codex_home(&state.default_workdir, "shared-app-server")?;
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = codex_command()?;
    command
        .arg("app-server")
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
    let process = Arc::new(
        SharedChild::new(child).context("failed to share shared Codex app-server child")?,
    );
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions: SharedCodexSessionMap = Arc::new(Mutex::new(HashMap::new()));
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));

    {
        let writer_state = state.clone();
        let writer_pending_requests = pending_requests.clone();
        let writer_sessions = sessions.clone();
        let writer_thread_sessions = thread_sessions.clone();
        let writer_runtime_id = runtime_id.clone();
        std::thread::spawn(move || {
            let mut stdin = stdin;
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
                Duration::from_secs(15),
            )
            .and_then(|_| {
                write_codex_json_rpc_message(&mut stdin, &json!({ "method": "initialized" }))
            });

            if let Err(err) = initialize_result {
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
                    } => handle_shared_codex_prompt_command(
                        &mut stdin,
                        &writer_pending_requests,
                        &writer_state,
                        &writer_sessions,
                        &writer_thread_sessions,
                        &session_id,
                        command,
                    ),
                    CodexRuntimeCommand::JsonRpcRequest {
                        method,
                        params,
                        timeout,
                        response_tx,
                    } => {
                        let request_result = send_codex_json_rpc_request(
                            &mut stdin,
                            &writer_pending_requests,
                            &method,
                            params,
                            timeout,
                        )
                        .map_err(|err| format!("{err:#}"));
                        let _ = response_tx.send(request_result.clone());
                        request_result.map(|_| ()).map_err(anyhow::Error::msg)
                    }
                    CodexRuntimeCommand::JsonRpcResponse { response } => {
                        write_codex_json_rpc_message(
                            &mut stdin,
                            &json!({
                                "id": response.request_id,
                                "result": response.result,
                            }),
                        )
                    }
                    CodexRuntimeCommand::InterruptTurn {
                        response_tx,
                        thread_id,
                        turn_id,
                    } => {
                        let interrupt_result = send_codex_json_rpc_request(
                            &mut stdin,
                            &writer_pending_requests,
                            "turn/interrupt",
                            json!({
                                "threadId": thread_id,
                                "turnId": turn_id,
                            }),
                            Duration::from_secs(30),
                        )
                        .map(|_| ())
                        .map_err(|err| format!("{err:#}"));
                        let _ = response_tx.send(interrupt_result.clone());
                        interrupt_result.map_err(anyhow::Error::msg)
                    }
                    CodexRuntimeCommand::RefreshModelList { response_tx } => {
                        let refresh_result =
                            handle_codex_model_list_refresh(&mut stdin, &writer_pending_requests)
                                .map_err(|err| format!("{err:#}"));
                        match refresh_result {
                            Ok(model_options) => {
                                let _ = response_tx.send(Ok(model_options));
                                Ok(())
                            }
                            Err(detail) => {
                                let _ = response_tx.send(Err(detail.clone()));
                                Err(anyhow!(detail))
                            }
                        }
                    }
                };

                if let Err(err) = command_result {
                    let _ = writer_state.handle_shared_codex_runtime_exit(
                        &writer_runtime_id,
                        Some(&format!(
                            "failed to communicate with shared Codex app-server: {err:#}"
                        )),
                    );
                    break;
                }
            }
        });
    }

    {
        let reader_state = state.clone();
        let reader_pending_requests = pending_requests.clone();
        let reader_sessions = sessions.clone();
        let reader_thread_sessions = thread_sessions.clone();
        let reader_runtime_id = runtime_id.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        let _ = reader_state.handle_shared_codex_runtime_exit(
                            &reader_runtime_id,
                            Some(&format!(
                                "failed to read stdout from shared Codex app-server: {err}"
                            )),
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
                        let _ = reader_state.handle_shared_codex_runtime_exit(
                            &reader_runtime_id,
                            Some(&format!(
                                "failed to parse shared Codex app-server JSON line: {err}"
                            )),
                        );
                        break;
                    }
                };

                if let Err(err) = handle_shared_codex_app_server_message(
                    &message,
                    &reader_state,
                    &reader_runtime_id,
                    &reader_pending_requests,
                    &reader_sessions,
                    &reader_thread_sessions,
                ) {
                    let _ = reader_state.handle_shared_codex_runtime_exit(
                        &reader_runtime_id,
                        Some(&format!(
                            "failed to handle shared Codex app-server event: {err:#}"
                        )),
                    );
                    break;
                }
            }

            let mut sessions = reader_sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            for session_state in sessions.values_mut() {
                session_state.recorder.streaming_text_message_id = None;
                session_state.turn_id = None;
            }
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("codex stderr> {line}");
            }
        });
    }

    {
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_id = runtime_id.clone();
        std::thread::spawn(move || {
            match wait_process.wait() {
                Ok(status) if status.success() => {
                    let _ = wait_state.handle_shared_codex_runtime_exit(&wait_runtime_id, None);
                }
                Ok(status) => {
                    let _ = wait_state.handle_shared_codex_runtime_exit(
                        &wait_runtime_id,
                        Some(&format!("shared Codex app-server exited with status {status}")),
                    );
                }
                Err(err) => {
                    let _ = wait_state.handle_shared_codex_runtime_exit(
                        &wait_runtime_id,
                        Some(&format!("failed waiting for shared Codex app-server: {err}")),
                    );
                }
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
        session_state.turn_id = None;
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

fn codex_message_thread_id<'a>(message: &'a Value) -> Option<&'a str> {
    message
        .pointer("/params/threadId")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/params/thread/id").and_then(Value::as_str))
}

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

fn handle_shared_codex_prompt_command(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    state: &AppState,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    session_id: &str,
    command: CodexPromptCommand,
) -> Result<()> {
    let existing_thread_id = {
        let sessions = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions
            .get(session_id)
            .and_then(|session| session.thread_id.clone())
    };

    let thread_id = if let Some(thread_id) = existing_thread_id {
        thread_id
    } else {
        let result = match command.resume_thread_id.as_deref() {
            Some(thread_id) => send_codex_json_rpc_request(
                writer,
                pending_requests,
                "thread/resume",
                json!({
                    "threadId": thread_id,
                    "cwd": command.cwd,
                    "model": command.model,
                    "sandbox": command.sandbox_mode.as_cli_value(),
                    "approvalPolicy": command.approval_policy.as_cli_value(),
                }),
                Duration::from_secs(30),
            )?,
            None => send_codex_json_rpc_request(
                writer,
                pending_requests,
                "thread/start",
                json!({
                    "cwd": command.cwd,
                    "model": command.model,
                    "sandbox": command.sandbox_mode.as_cli_value(),
                    "approvalPolicy": command.approval_policy.as_cli_value(),
                    "personality": "pragmatic",
                }),
                Duration::from_secs(30),
            )?,
        };

        let thread_id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Codex app-server did not return a thread id"))?
            .to_owned();
        state.set_external_session_id(session_id, thread_id.clone())?;
        remember_shared_codex_thread(sessions, thread_sessions, session_id, thread_id.clone());
        thread_id
    };

    state.record_codex_runtime_config(
        session_id,
        command.sandbox_mode,
        command.approval_policy,
        command.reasoning_effort,
    )?;

    let turn_result = send_codex_json_rpc_request(
        writer,
        pending_requests,
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
        Duration::from_secs(30),
    )?;

    let turn_id = turn_result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = sessions.entry(session_id.to_owned()).or_default();
    session_state.turn_id = turn_id;
    Ok(())
}

fn handle_shared_codex_app_server_message(
    message: &Value,
    state: &AppState,
    runtime_id: &str,
    pending_requests: &CodexPendingRequestMap,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
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
                    Err(summarize_codex_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    ))
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
        return Ok(());
    };
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    if !state.session_matches_runtime_token(&session_id, &runtime_token) {
        return Ok(());
    }

    let mut sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let Some(session_state) = sessions.get_mut(&session_id) else {
        return Ok(());
    };
    let SharedCodexSessionState {
        recorder: recorder_state,
        thread_id,
        turn_id,
        turn_state,
    } = session_state;
    let mut recorder = BorrowedSessionRecorder::new(state, &session_id, recorder_state);

    if message.get("id").is_some() {
        return handle_codex_app_server_request(method, message, &mut recorder);
    }

    handle_shared_codex_app_server_notification(
        method,
        message,
        state,
        &session_id,
        &runtime_token,
        thread_id,
        turn_id,
        turn_state,
        thread_sessions,
        &mut recorder,
    )
}

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

fn build_shared_codex_runtime_notice(method: &str, message: &Value) -> Option<CodexNotice> {
    build_shared_codex_global_notice(
        CodexNoticeKind::RuntimeNotice,
        infer_shared_codex_notice_level(method, message),
        &format!("Codex notice: {method}"),
        message,
    )
}

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
        &[
            "/title",
            "/name",
            "/warning/title",
            "/deprecation/title",
        ],
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

fn handle_shared_codex_app_server_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    session_thread_id: &mut Option<String>,
    turn_id: &mut Option<String>,
    turn_state: &mut CodexTurnState,
    thread_sessions: &SharedCodexThreadMap,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = message.pointer("/params/thread/id").and_then(Value::as_str) {
                let previous_thread_id = session_thread_id.replace(thread_id.to_owned());
                *turn_id = None;
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
            clear_codex_turn_state(turn_state);
            *turn_id = message
                .pointer("/params/turn/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            recorder.finish_streaming_text()?;
        }
        "turn/completed" => {
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    *turn_id = None;
                    clear_codex_turn_state(turn_state);
                    recorder.finish_streaming_text()?;
                    state.fail_turn_if_runtime_matches(
                        session_id,
                        runtime_token,
                        &summarize_error(error),
                    )?;
                    return Ok(());
                }
            }

            *turn_id = None;
            flush_pending_codex_subagent_results(turn_state, recorder)?;
            clear_codex_turn_state(turn_state);
            recorder.finish_streaming_text()?;
            state.finish_turn_ok_if_runtime_matches(session_id, runtime_token)?;
        }
        "item/started" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_started(item, recorder)?;
            }
        }
        "item/completed" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_completed(item, state, session_id, turn_state, recorder)?;
            }
        }
        "item/agentMessage/delta" => {
            let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(item_id) = message.pointer("/params/itemId").and_then(Value::as_str) else {
                return Ok(());
            };
            record_codex_agent_message_delta(turn_state, recorder, state, session_id, item_id, delta)?;
        }
        "model/rerouted" => {
            handle_shared_codex_model_rerouted(message, state, session_id, turn_id.as_deref(), turn_state, recorder)?;
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
            clear_codex_turn_state(turn_state);
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
                turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message_content_delta" => {
            handle_shared_codex_event_agent_message_content_delta(
                message,
                turn_id.as_deref(),
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

fn shared_codex_event_turn_id<'a>(message: &'a Value) -> Option<&'a str> {
    message
        .pointer("/params/msg/turn_id")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/params/turnId").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn_id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn/id").and_then(Value::as_str))
}

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
    let notice = format!(
        "Codex rerouted this turn from `{from_model}` to `{to_model}`{reason}."
    );
    push_shared_codex_turn_notice(state, session_id, turn_state, &notice)
}

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

    push_shared_codex_turn_notice(state, session_id, turn_state, "Codex compacted the thread context for this turn.")
}

fn handle_shared_codex_event_item_completed(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return Ok(());
    }

    let Some(item) = message.pointer("/params/msg/item") else {
        return Ok(());
    };

    match item.get("type").and_then(Value::as_str) {
        Some("AgentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if turn_state.streamed_agent_message_item_ids.contains(item_id) {
                return Ok(());
            }

            let text = item
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| concatenate_codex_text_parts(content));

            if let Some(text) = text.as_deref() {
                begin_codex_assistant_output(turn_state, recorder)?;
                recorder.push_text(text)?;
                remember_codex_first_assistant_message_id(state, session_id, turn_state)?;
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

fn clear_codex_turn_state(turn_state: &mut CodexTurnState) {
    turn_state.current_agent_message_id = None;
    turn_state.streamed_agent_message_text_by_item_id.clear();
    turn_state.streamed_agent_message_item_ids.clear();
    turn_state.pending_subagent_results.clear();
    turn_state.assistant_output_started = false;
    turn_state.first_visible_assistant_message_id = None;
}

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

fn handle_shared_codex_event_agent_message_content_delta(
    message: &Value,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
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

fn handle_shared_codex_event_agent_message(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return Ok(());
    }

    if !turn_state.streamed_agent_message_item_ids.is_empty() {
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

    begin_codex_assistant_output(turn_state, recorder)?;
    recorder.push_text(trimmed)?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

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

fn longest_codex_delta_overlap(existing: &str, incoming: &str) -> usize {
    let max_overlap = existing.len().min(incoming.len());
    for overlap in (1..=max_overlap).rev() {
        if incoming.is_char_boundary(overlap) && existing.ends_with(&incoming[..overlap]) {
            return overlap;
        }
    }

    0
}

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
                permissions_summary.as_deref().filter(|value| !value.is_empty()),
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
                params.get("questions").cloned().unwrap_or_else(|| json!([])),
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
                CodexPendingAppRequest {
                    request_id,
                },
            )?;
        }
    }

    Ok(())
}

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

    if permissions.pointer("/network/enabled").and_then(Value::as_bool) == Some(true) {
        parts.push("network access".to_owned());
    }

    if permissions.pointer("/macos/accessibility").and_then(Value::as_bool) == Some(true) {
        parts.push("macOS accessibility access".to_owned());
    }

    if permissions.pointer("/macos/calendar").and_then(Value::as_bool) == Some(true) {
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
            if !turn_state.streamed_agent_message_item_ids.contains(item_id) {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    begin_codex_assistant_output(turn_state, recorder)?;
                    recorder.push_text(text)?;
                    remember_codex_first_assistant_message_id(state, session_id, turn_state)?;
                }
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

fn send_codex_json_rpc_request(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel();
    pending_requests
        .lock()
        .expect("Codex pending requests mutex poisoned")
        .insert(request_id.clone(), tx);

    if let Err(err) = write_codex_json_rpc_message(
        writer,
        &json!({
            "id": request_id,
            "method": method,
            "params": params,
        }),
    ) {
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .remove(&request_id);
        return Err(err);
    }

    match rx.recv_timeout(timeout) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(anyhow!(err)),
        Err(err) => {
            pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&request_id);
            Err(anyhow!(
                "timed out waiting for Codex app-server response to `{method}`: {err}"
            ))
        }
    }
}

fn handle_codex_model_list_refresh(
    writer: &mut impl Write,
    pending_requests: &CodexPendingRequestMap,
) -> Result<Vec<SessionModelOption>> {
    let mut cursor: Option<String> = None;
    let mut model_options = Vec::new();

    loop {
        let result = send_codex_json_rpc_request(
            writer,
            pending_requests,
            "model/list",
            json!({
                "cursor": cursor,
                "includeHidden": false,
                "limit": 100,
            }),
            Duration::from_secs(30),
        )?;
        model_options.extend(codex_model_options(&result));
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    Ok(model_options)
}

fn claude_model_options(message: &Value) -> Option<Vec<SessionModelOption>> {
    let models = message.pointer("/response/response/models")?.as_array()?;
    Some(
        models
            .iter()
            .filter_map(|entry| {
                let value = entry
                    .get("value")
                    .or_else(|| entry.get("model"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())?
                    .to_owned();
                let label = entry
                    .get("displayName")
                    .or_else(|| entry.get("label"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|label| !label.is_empty())
                    .unwrap_or(&value)
                    .to_owned();
                let description = entry
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|description| !description.is_empty())
                    .map(str::to_owned);
                Some(SessionModelOption {
                    label,
                    value,
                    description,
                    badges: claude_model_badges(entry),
                    supported_claude_effort_levels: entry
                        .get("supportedEffortLevels")
                        .and_then(Value::as_array)
                        .map(|levels| {
                            levels
                                .iter()
                                .filter_map(Value::as_str)
                                .filter_map(parse_claude_effort_level)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    default_reasoning_effort: None,
                    supported_reasoning_efforts: Vec::new(),
                })
            })
            .collect(),
    )
}

fn claude_agent_commands(message: &Value) -> Option<Vec<AgentCommand>> {
    let commands = message.pointer("/response/response/commands")?.as_array()?;
    let parsed = commands
        .iter()
        .filter_map(|entry| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_owned();
            let raw_description = entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            let (description, source) = normalize_claude_agent_command_description(raw_description);
            let argument_hint = entry
                .get("argumentHint")
                .or_else(|| entry.get("argument_hint"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            Some(AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: name.clone(),
                description,
                content: format!("/{name}"),
                source,
                argument_hint,
            })
        })
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        return None;
    }
    Some(dedupe_agent_commands(parsed))
}

fn normalize_claude_agent_command_description(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    for (suffix, source) in [
        ("(bundled)", "Claude bundled command"),
        ("(project)", "Claude project command"),
        ("(user)", "Claude user command"),
    ] {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return (stripped.trim().to_owned(), source.to_owned());
        }
    }
    (trimmed.to_owned(), "Claude native command".to_owned())
}

fn claude_model_badges(entry: &Value) -> Vec<String> {
    let mut badges = Vec::new();
    let display_name = entry
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if entry.get("value").and_then(Value::as_str) == Some("default")
        || display_name.contains("recommended")
    {
        badges.push("Recommended".to_owned());
    }
    if entry
        .get("supportsEffort")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || entry
            .get("supportedEffortLevels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty())
    {
        badges.push("Effort".to_owned());
    }
    if entry
        .get("supportsAdaptiveThinking")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        badges.push("Adaptive".to_owned());
    }
    if entry
        .get("supportsFastMode")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        badges.push("Fast".to_owned());
    }
    badges
}

fn parse_claude_effort_level(value: &str) -> Option<ClaudeEffortLevel> {
    match value.trim() {
        "default" => Some(ClaudeEffortLevel::Default),
        "low" => Some(ClaudeEffortLevel::Low),
        "medium" => Some(ClaudeEffortLevel::Medium),
        "high" => Some(ClaudeEffortLevel::High),
        "max" => Some(ClaudeEffortLevel::Max),
        _ => None,
    }
}

fn parse_codex_reasoning_effort(value: &str) -> Option<CodexReasoningEffort> {
    match value.trim() {
        "none" => Some(CodexReasoningEffort::None),
        "minimal" => Some(CodexReasoningEffort::Minimal),
        "low" => Some(CodexReasoningEffort::Low),
        "medium" => Some(CodexReasoningEffort::Medium),
        "high" => Some(CodexReasoningEffort::High),
        "xhigh" => Some(CodexReasoningEffort::XHigh),
        _ => None,
    }
}

fn codex_reasoning_effort_rank(effort: CodexReasoningEffort) -> usize {
    match effort {
        CodexReasoningEffort::None => 0,
        CodexReasoningEffort::Minimal => 1,
        CodexReasoningEffort::Low => 2,
        CodexReasoningEffort::Medium => 3,
        CodexReasoningEffort::High => 4,
        CodexReasoningEffort::XHigh => 5,
    }
}

fn codex_model_option<'a>(
    model: &str,
    model_options: &'a [SessionModelOption],
) -> Option<&'a SessionModelOption> {
    model_options.iter().find(|option| option.value == model)
}

fn matching_session_model_option_value(
    requested_model: &str,
    model_options: &[SessionModelOption],
) -> Option<String> {
    let trimmed_model = requested_model.trim();
    if trimmed_model.is_empty() {
        return None;
    }

    model_options
        .iter()
        .find(|option| {
            option.value.eq_ignore_ascii_case(trimmed_model)
                || option.label.eq_ignore_ascii_case(trimmed_model)
        })
        .map(|option| option.value.clone())
}

fn normalized_codex_reasoning_effort(
    model: &str,
    current_effort: CodexReasoningEffort,
    model_options: &[SessionModelOption],
) -> Option<CodexReasoningEffort> {
    let option = codex_model_option(model, model_options)?;
    if option.supported_reasoning_efforts.is_empty() {
        return None;
    }
    if option.supported_reasoning_efforts.contains(&current_effort) {
        return Some(current_effort);
    }

    option
        .default_reasoning_effort
        .filter(|effort| option.supported_reasoning_efforts.contains(effort))
        .or_else(|| option.supported_reasoning_efforts.first().copied())
}

fn format_codex_reasoning_efforts(efforts: &[CodexReasoningEffort]) -> String {
    let efforts = efforts
        .iter()
        .map(|effort| effort.as_api_value())
        .collect::<Vec<_>>();
    match efforts.as_slice() {
        [] => "the available reasoning levels".to_owned(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} or {second}"),
        _ => {
            let last = efforts.last().copied().unwrap_or_default();
            format!("{}, or {}", efforts[..efforts.len() - 1].join(", "), last)
        }
    }
}

fn codex_model_options(model_list_result: &Value) -> Vec<SessionModelOption> {
    model_list_result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let value = entry
                .get("model")
                .or_else(|| entry.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?
                .to_owned();
            let label = entry
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(&value)
                .to_owned();
            let description = entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|description| !description.is_empty())
                .map(str::to_owned);
            let default_reasoning_effort = entry
                .get("default_reasoning_level")
                .or_else(|| entry.get("defaultReasoningLevel"))
                .and_then(Value::as_str)
                .and_then(parse_codex_reasoning_effort);
            let mut supported_reasoning_efforts = entry
                .get("supported_reasoning_levels")
                .or_else(|| entry.get("supportedReasoningLevels"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| {
                    level
                        .get("effort")
                        .or_else(|| level.get("value"))
                        .and_then(Value::as_str)
                        .or_else(|| level.as_str())
                        .and_then(parse_codex_reasoning_effort)
                })
                .collect::<Vec<_>>();
            supported_reasoning_efforts.sort_by_key(|effort| codex_reasoning_effort_rank(*effort));
            supported_reasoning_efforts.dedup();
            Some(SessionModelOption {
                label,
                value,
                description,
                badges: Vec::new(),
                supported_claude_effort_levels: Vec::new(),
                default_reasoning_effort,
                supported_reasoning_efforts,
            })
        })
        .collect()
}

fn write_codex_json_rpc_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)
        .context("failed to encode Codex app-server message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Codex app-server message delimiter")?;
    writer
        .flush()
        .context("failed to flush Codex app-server stdin")
}

fn codex_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

fn summarize_codex_json_rpc_error(error: &Value) -> String {
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    summarize_error(error)
}

fn codex_sandbox_policy_value(mode: CodexSandboxMode) -> Value {
    match mode {
        CodexSandboxMode::ReadOnly => json!({
            "type": "readOnly",
        }),
        CodexSandboxMode::WorkspaceWrite => json!({
            "type": "workspaceWrite",
        }),
        CodexSandboxMode::DangerFullAccess => json!({
            "type": "dangerFullAccess",
        }),
    }
}

fn codex_command() -> Result<Command> {
    let exe = resolve_codex_executable()?;

    // On Windows, .cmd/.bat shims (from npm) must be run through cmd.exe.
    #[cfg(windows)]
    {
        if let Some(ext) = exe.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
                let mut cmd = Command::new("cmd.exe");
                cmd.args(["/C", &exe.to_string_lossy()]);
                return Ok(cmd);
            }
        }
    }

    Ok(Command::new(exe))
}

fn resolve_codex_executable() -> Result<PathBuf> {
    let launcher =
        find_command_on_path("codex").ok_or_else(|| anyhow!("`codex` was not found on PATH"))?;
    Ok(resolve_codex_native_binary(&launcher).unwrap_or(launcher))
}

fn find_command_on_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(windows)]
    let extensions: &[&str] = &[".exe", ".cmd", ".bat", ""];

    #[cfg(not(windows))]
    let extensions: &[&str] = &[""];

    for dir in std::env::split_paths(&path) {
        for ext in extensions {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn collect_agent_readiness(default_workdir: &str) -> Vec<AgentReadiness> {
    vec![
        agent_readiness_for(Agent::Cursor, default_workdir),
        agent_readiness_for(Agent::Gemini, default_workdir),
    ]
}

fn validate_agent_session_setup(agent: Agent, workdir: &str) -> std::result::Result<(), String> {
    let readiness = agent_readiness_for(agent, workdir);
    if readiness.blocking {
        return Err(readiness.detail);
    }
    Ok(())
}

fn agent_readiness_for(agent: Agent, workdir: &str) -> AgentReadiness {
    match agent {
        Agent::Cursor => cursor_agent_readiness(),
        Agent::Gemini => gemini_agent_readiness(workdir),
        _ => AgentReadiness {
            agent,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("{} is managed by its local CLI runtime.", agent.name()),
            command_path: None,
        },
    }
}

fn cursor_agent_readiness() -> AgentReadiness {
    let command_path = find_command_on_path("cursor-agent").map(|path| path.display().to_string());
    match command_path {
        Some(command_path) => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Cursor Agent is available at `{command_path}`."),
            command_path: Some(command_path),
        },
        None => AgentReadiness {
            agent: Agent::Cursor,
            status: AgentReadinessStatus::Missing,
            blocking: true,
            detail: "Install `cursor-agent` and make sure it is on PATH before creating Cursor sessions."
                .to_owned(),
            command_path: None,
        },
    }
}

fn gemini_agent_readiness(workdir: &str) -> AgentReadiness {
    let command_path = match find_command_on_path("gemini") {
        Some(path) => path,
        None => {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Missing,
                blocking: true,
                detail: "Install the `gemini` CLI and make sure it is on PATH before creating Gemini sessions."
                    .to_owned(),
                command_path: None,
            };
        }
    };
    let command_path_display = command_path.display().to_string();

    if let Some(source) = gemini_api_key_source(workdir) {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Gemini CLI is ready with a Gemini API key from {source}."),
            command_path: Some(command_path_display),
        };
    }

    let selected_auth_type = gemini_selected_auth_type(workdir);
    if selected_auth_type.as_deref() == Some("oauth-personal") {
        if let Some(path) = gemini_oauth_credentials_path().filter(|path| path.is_file()) {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!(
                    "Gemini CLI is ready with Google login credentials from {}.",
                    display_path_for_user(&path)
                ),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: format!(
                "Gemini is configured for Google login, but {} is missing.",
                gemini_oauth_credentials_path()
                    .as_deref()
                    .map(display_path_for_user)
                    .unwrap_or_else(|| "~/.gemini/oauth_creds.json".to_owned())
            ),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("gemini-api-key") {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: gemini_api_key_missing_detail(workdir),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("vertex-ai") {
        if let Some(source) = gemini_vertex_auth_source(workdir) {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "Gemini is configured for Vertex AI, but the required credentials are missing. Set `GOOGLE_API_KEY`, or set both `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`."
                .to_owned(),
            command_path: Some(command_path_display),
        };
    }

    if selected_auth_type.as_deref() == Some("compute-default-credentials") {
        if let Some(source) = gemini_adc_source() {
            return AgentReadiness {
                agent: Agent::Gemini,
                status: AgentReadinessStatus::Ready,
                blocking: false,
                detail: format!(
                    "Gemini CLI is ready with application default credentials from {source}."
                ),
                command_path: Some(command_path_display),
            };
        }
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::NeedsSetup,
            blocking: true,
            detail: "Gemini is configured for application default credentials, but no ADC file was found. Set `GOOGLE_APPLICATION_CREDENTIALS` or run `gcloud auth application-default login`."
                .to_owned(),
            command_path: Some(command_path_display),
        };
    }

    if let Some(source) = gemini_vertex_auth_source(workdir) {
        return AgentReadiness {
            agent: Agent::Gemini,
            status: AgentReadinessStatus::Ready,
            blocking: false,
            detail: format!("Gemini CLI is ready with Vertex AI credentials from {source}."),
            command_path: Some(command_path_display),
        };
    }

    AgentReadiness {
        agent: Agent::Gemini,
        status: AgentReadinessStatus::NeedsSetup,
        blocking: true,
        detail: "Gemini CLI needs auth before TermAl can create sessions. Set `GEMINI_API_KEY`, configure Vertex AI env vars, or choose an auth type in `.gemini/settings.json`."
            .to_owned(),
        command_path: Some(command_path_display),
    }
}

fn gemini_api_key_missing_detail(workdir: &str) -> String {
    let env_file = find_gemini_env_file(workdir)
        .map(|path| display_path_for_user(&path))
        .unwrap_or_else(|| ".env".to_owned());
    format!(
        "Gemini is configured for an API key, but `GEMINI_API_KEY` was not found in the process environment or in {env_file}."
    )
}

fn gemini_api_key_source(workdir: &str) -> Option<String> {
    env_var_source("GEMINI_API_KEY").or_else(|| dotenv_var_source(workdir, "GEMINI_API_KEY"))
}

fn gemini_vertex_auth_source(workdir: &str) -> Option<String> {
    let vertex_enabled = env_var_present("GOOGLE_GENAI_USE_VERTEXAI")
        || env_var_present("GOOGLE_GENAI_USE_GCA")
        || dotenv_var_present(workdir, "GOOGLE_GENAI_USE_VERTEXAI")
        || dotenv_var_present(workdir, "GOOGLE_GENAI_USE_GCA");
    if !vertex_enabled && gemini_selected_auth_type(workdir).as_deref() != Some("vertex-ai") {
        return None;
    }

    if let Some(source) =
        env_var_source("GOOGLE_API_KEY").or_else(|| dotenv_var_source(workdir, "GOOGLE_API_KEY"))
    {
        return Some(source);
    }

    let has_project = env_var_present("GOOGLE_CLOUD_PROJECT")
        || dotenv_var_present(workdir, "GOOGLE_CLOUD_PROJECT");
    let has_location = env_var_present("GOOGLE_CLOUD_LOCATION")
        || dotenv_var_present(workdir, "GOOGLE_CLOUD_LOCATION");
    if has_project && has_location {
        return Some(
            env_var_source("GOOGLE_CLOUD_PROJECT")
                .or_else(|| dotenv_var_source(workdir, "GOOGLE_CLOUD_PROJECT"))
                .unwrap_or_else(|| "workspace configuration".to_owned()),
        );
    }

    None
}

fn gemini_adc_source() -> Option<String> {
    if let Some(path) = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(display_path_for_user(&path));
    }

    let home = home_dir()?;
    let default_path = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).map(|path| {
            path.join("gcloud")
                .join("application_default_credentials.json")
        })
    } else {
        Some(
            home.join(".config")
                .join("gcloud")
                .join("application_default_credentials.json"),
        )
    }?;
    default_path
        .is_file()
        .then(|| display_path_for_user(&default_path))
}

fn gemini_selected_auth_type(workdir: &str) -> Option<String> {
    let workspace_settings = PathBuf::from(workdir).join(".gemini").join("settings.json");
    for path in [
        Some(workspace_settings),
        gemini_user_settings_path(),
        gemini_system_settings_path(),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(selected_type) = gemini_selected_auth_type_from_settings_file(&path) {
            return Some(selected_type);
        }
    }
    None
}

fn gemini_selected_auth_type_from_settings_file(path: &FsPath) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
        return parsed
            .pointer("/security/auth/selectedType")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    [
        "oauth-personal",
        "gemini-api-key",
        "vertex-ai",
        "compute-default-credentials",
    ]
    .iter()
    .find_map(|candidate| raw.contains(candidate).then_some((*candidate).to_owned()))
}

fn find_gemini_env_file(workdir: &str) -> Option<PathBuf> {
    let mut current = PathBuf::from(workdir);
    loop {
        let gemini_env_path = current.join(".gemini").join(".env");
        if gemini_env_path.is_file() {
            return Some(gemini_env_path);
        }
        let env_path = current.join(".env");
        if env_path.is_file() {
            return Some(env_path);
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    let home = home_dir()?;
    let home_gemini_env = home.join(".gemini").join(".env");
    if home_gemini_env.is_file() {
        return Some(home_gemini_env);
    }
    let home_env = home.join(".env");
    home_env.is_file().then_some(home_env)
}

fn dotenv_var_source(workdir: &str, key: &str) -> Option<String> {
    let path = find_gemini_env_file(workdir)?;
    dotenv_file_var_present(&path, key).then(|| display_path_for_user(&path))
}

fn dotenv_var_present(workdir: &str, key: &str) -> bool {
    find_gemini_env_file(workdir)
        .as_deref()
        .map(|path| dotenv_file_var_present(path, key))
        .unwrap_or(false)
}

fn dotenv_file_var_present(path: &FsPath, key: &str) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    raw.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((name, value)) = trimmed.split_once('=') else {
            return false;
        };
        if name.trim() != key {
            return false;
        }
        !value
            .trim()
            .trim_matches(|ch| ch == '"' || ch == '\'')
            .is_empty()
    })
}

fn env_var_source(key: &str) -> Option<String> {
    env_var_present(key).then(|| format!("the `{key}` environment variable"))
}

fn env_var_present(key: &str) -> bool {
    std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn gemini_oauth_credentials_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("oauth_creds.json"))
}

fn gemini_user_settings_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".gemini").join("settings.json"))
}

fn gemini_system_settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH") {
        return Some(PathBuf::from(path));
    }
    Some(if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
    } else if cfg!(windows) {
        PathBuf::from("C:\\ProgramData\\gemini-cli\\settings.json")
    } else {
        PathBuf::from("/etc/gemini-cli/settings.json")
    })
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn display_path_for_user(path: &FsPath) -> String {
    if let Some(home) = home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            };
        }
    }
    path.display().to_string()
}

fn resolve_codex_native_binary(launcher: &PathBuf) -> Option<PathBuf> {
    let launcher = fs::canonicalize(launcher)
        .ok()
        .unwrap_or_else(|| launcher.clone());
    let package_root = launcher.parent()?.parent()?;
    let node_modules_dir = package_root.join("node_modules").join("@openai");
    let target_triple = codex_target_triple()?;
    let binary_name = if cfg!(windows) { "codex.exe" } else { "codex" };

    let entries = fs::read_dir(node_modules_dir).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if !name.starts_with("codex-") {
            continue;
        }
        let candidate = entry
            .path()
            .join("vendor")
            .join(target_triple)
            .join("codex")
            .join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn codex_target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

fn describe_codex_app_server_web_search_command(item: &Value) -> String {
    let query = item
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("open_page") => item
            .pointer("/action/url")
            .and_then(Value::as_str)
            .map(|url| format!("Open page: {url}"))
            .unwrap_or_else(|| "Open page".to_owned()),
        Some("find_in_page") => item
            .pointer("/action/pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("Find in page: {pattern}"))
            .unwrap_or_else(|| "Find in page".to_owned()),
        _ => query
            .map(|value| format!("Web search: {value}"))
            .unwrap_or_else(|| "Web search".to_owned()),
    }
}

fn summarize_codex_app_server_web_search_output(item: &Value) -> String {
    match item.pointer("/action/type").and_then(Value::as_str) {
        Some("search") => {
            let queries = item
                .pointer("/action/queries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !queries.is_empty() {
                return queries.join("\n");
            }
        }
        Some("open_page") => {
            if let Some(url) = item.pointer("/action/url").and_then(Value::as_str) {
                return format!("Opened {url}");
            }
        }
        Some("find_in_page") => {
            let pattern = item.pointer("/action/pattern").and_then(Value::as_str);
            let url = item.pointer("/action/url").and_then(Value::as_str);
            return match (pattern, url) {
                (Some(pattern), Some(url)) => format!("Searched for `{pattern}` in {url}"),
                (Some(pattern), None) => format!("Searched for `{pattern}`"),
                (None, Some(url)) => format!("Searched within {url}"),
                (None, None) => "Find in page completed".to_owned(),
            };
        }
        _ => {}
    }

    item.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Web search completed")
        .to_owned()
}

fn spawn_claude_runtime(
    state: AppState,
    session_id: String,
    cwd: String,
    model: String,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    resume_session_id: Option<String>,
    model_options_tx: Option<Sender<std::result::Result<Vec<SessionModelOption>, String>>>,
) -> Result<ClaudeRuntimeHandle> {
    let runtime_id = Uuid::new_v4().to_string();
    let mut command = Command::new("claude");
    command.current_dir(&cwd).args([
        "--model",
        &model,
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]);
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        command.args(["--permission-mode", permission_mode]);
    }
    if let Some(effort) = effort.as_cli_value() {
        command.args(["--effort", effort]);
    }
    command.env("CLAUDE_CODE_ENTRYPOINT", "termal");
    if let Some(resume_session_id) = resume_session_id {
        command.args(["--resume", &resume_session_id]);
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start Claude in `{cwd}`"))?;

    let stdin = child
        .stdin
        .take()
        .context("failed to capture Claude stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture Claude stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture Claude stderr")?;
    let process = Arc::new(SharedChild::new(child).context("failed to share Claude child")?);

    let (input_tx, input_rx) = mpsc::channel::<ClaudeRuntimeCommand>();

    {
        let writer_session_id = session_id.clone();
        let writer_state = state.clone();
        let writer_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            let mut stdin = stdin;
            if let Err(err) = write_claude_initialize(&mut stdin) {
                let _ = writer_state.handle_runtime_exit_if_matches(
                    &writer_session_id,
                    &writer_runtime_token,
                    Some(&format!("failed to initialize Claude session: {err:#}")),
                );
                return;
            }

            while let Ok(command) = input_rx.recv() {
                let write_result = match command {
                    ClaudeRuntimeCommand::Prompt(prompt) => {
                        write_claude_prompt_message(&mut stdin, &prompt)
                    }
                    ClaudeRuntimeCommand::PermissionResponse(decision) => {
                        write_claude_permission_response(&mut stdin, &decision)
                    }
                    ClaudeRuntimeCommand::SetModel(model) => {
                        write_claude_set_model(&mut stdin, &model)
                    }
                    ClaudeRuntimeCommand::SetPermissionMode(mode) => {
                        write_claude_set_permission_mode(&mut stdin, &mode)
                    }
                };

                if let Err(err) = write_result {
                    let _ = writer_state.handle_runtime_exit_if_matches(
                        &writer_session_id,
                        &writer_runtime_token,
                        Some(&format!("failed to write prompt to Claude stdin: {err:#}")),
                    );
                    break;
                }
            }
        });
    }

    {
        let reader_session_id = session_id.clone();
        let reader_state = state.clone();
        let reader_input_tx = input_tx.clone();
        let reader_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut raw_line = String::new();
            let mut turn_state = ClaudeTurnState::default();
            let mut recorder =
                SessionRecorder::new(reader_state.clone(), reader_session_id.clone());
            let mut resolved_session_id: Option<String> = None;
            let mut initialize_model_options_tx = model_options_tx;

            loop {
                raw_line.clear();
                let bytes_read = match reader.read_line(&mut raw_line) {
                    Ok(bytes_read) => bytes_read,
                    Err(err) => {
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ =
                                tx.send(Err(format!("failed to read stdout from Claude: {err}")));
                        }
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to read stdout from Claude: {err}"),
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
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ =
                                tx.send(Err(format!("failed to parse Claude JSON line: {err}")));
                        }
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to parse Claude JSON line: {err}"),
                        );
                        break;
                    }
                };

                let message_type = message.get("type").and_then(Value::as_str);
                let is_result = message.get("type").and_then(Value::as_str) == Some("result");
                let is_error = message
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let error_summary = is_result.then(|| summarize_error(&message));

                if let Some(agent_commands) = claude_agent_commands(&message) {
                    if let Err(err) =
                        reader_state.sync_session_agent_commands(&reader_session_id, agent_commands)
                    {
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to sync Claude agent commands: {err:#}"),
                        );
                        break;
                    }
                }

                if let Some(model_options) = claude_model_options(&message) {
                    if let Err(err) = reader_state.sync_session_model_options(
                        &reader_session_id,
                        None,
                        model_options.clone(),
                    ) {
                        if let Some(tx) = initialize_model_options_tx.take() {
                            let _ = tx
                                .send(Err(format!("failed to sync Claude model options: {err:#}")));
                        }
                        let _ = reader_state.fail_turn_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                            &format!("failed to sync Claude model options: {err:#}"),
                        );
                        break;
                    }

                    if let Some(tx) = initialize_model_options_tx.take() {
                        let _ = tx.send(Ok(model_options));
                    }
                }

                if message_type == Some("control_request") {
                    let approval_mode = match reader_state.claude_approval_mode(&reader_session_id)
                    {
                        Ok(mode) => mode,
                        Err(err) => {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!(
                                    "failed to resolve Claude approval mode for session: {err:#}"
                                ),
                            );
                            break;
                        }
                    };

                    let action = match classify_claude_control_request(
                        &message,
                        &mut turn_state,
                        approval_mode,
                    ) {
                        Ok(action) => action,
                        Err(err) => {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!("failed to handle Claude control request: {err:#}"),
                            );
                            break;
                        }
                    };

                    if let Some(action) = action {
                        let action_result = match action {
                            ClaudeControlRequestAction::QueueApproval {
                                title,
                                command,
                                detail,
                                approval,
                            } => recorder.push_claude_approval(&title, &command, &detail, approval),
                            ClaudeControlRequestAction::Respond(decision) => reader_input_tx
                                .send(ClaudeRuntimeCommand::PermissionResponse(decision))
                                .map_err(|err| {
                                    anyhow!("failed to auto-approve Claude tool request: {err}")
                                }),
                        };

                        if let Err(err) = action_result {
                            let _ = reader_state.fail_turn_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                &format!("failed to handle Claude control request: {err:#}"),
                            );
                            break;
                        }
                    }
                    continue;
                } else if message_type == Some("control_cancel_request") {
                    if let Some(request_id) = message.get("request_id").and_then(Value::as_str) {
                        let _ = reader_state.clear_claude_pending_approval_by_request(
                            &reader_session_id,
                            request_id,
                        );
                    }
                    continue;
                }

                if let Err(err) = handle_claude_event(
                    &message,
                    &mut resolved_session_id,
                    &mut turn_state,
                    &mut recorder,
                ) {
                    let _ = reader_state.fail_turn_if_runtime_matches(
                        &reader_session_id,
                        &reader_runtime_token,
                        &format!("failed to handle Claude event: {err:#}"),
                    );
                    break;
                }

                if is_result {
                    if is_error {
                        if let Some(detail) = error_summary.as_deref() {
                            let _ = reader_state.mark_turn_error_if_runtime_matches(
                                &reader_session_id,
                                &reader_runtime_token,
                                detail,
                            );
                        }
                    } else {
                        let _ = reader_state.finish_turn_ok_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                        );
                    }
                }
            }

            if let Some(tx) = initialize_model_options_tx.take() {
                let _ = tx.send(Err(
                    "Claude exited before reporting model options".to_owned()
                ));
            }
            let _ = recorder.finish_streaming_text();
        });
    }

    {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("claude stderr> {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || {
            match wait_process.wait() {
                Ok(status) if status.success() => {
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        None,
                    );
                }
                Ok(status) => {
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        Some(&format!("Claude session exited with status {status}")),
                    );
                }
                Err(err) => {
                    let _ = wait_state.handle_runtime_exit_if_matches(
                        &wait_session_id,
                        &wait_runtime_token,
                        Some(&format!("failed waiting for Claude session: {err}")),
                    );
                }
            }
        });
    }

    Ok(ClaudeRuntimeHandle {
        runtime_id,
        input_tx,
        process,
    })
}

fn write_claude_initialize(writer: &mut impl Write) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "initialize",
                "hooks": {},
                "systemPrompt": "",
                "appendSystemPrompt": "",
            }
        }),
    )
}

fn write_claude_prompt_message(
    writer: &mut impl Write,
    prompt: &ClaudePromptCommand,
) -> Result<()> {
    let mut content = Vec::new();
    if !prompt.text.trim().is_empty() {
        content.push(json!({
            "type": "text",
            "text": prompt.text.as_str(),
        }));
    }
    for attachment in &prompt.attachments {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.metadata.media_type.as_str(),
                "data": attachment.data.as_str(),
            }
        }));
    }

    write_claude_message(
        writer,
        &json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        }),
    )
}

fn write_claude_permission_response(
    writer: &mut impl Write,
    decision: &ClaudePermissionDecision,
) -> Result<()> {
    let message = match decision {
        ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        } => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": {
                    "behavior": "allow",
                    "updatedInput": updated_input,
                }
            }
        }),
        ClaudePermissionDecision::Deny {
            request_id,
            message,
        } => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": {
                    "behavior": "deny",
                    "message": message,
                }
            }
        }),
    };

    write_claude_message(writer, &message)
}

fn write_claude_set_permission_mode(writer: &mut impl Write, mode: &str) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "set_permission_mode",
                "mode": mode,
            }
        }),
    )
}

fn write_claude_set_model(writer: &mut impl Write, model: &str) -> Result<()> {
    write_claude_message(
        writer,
        &json!({
            "request_id": Uuid::new_v4().to_string(),
            "type": "control_request",
            "request": {
                "subtype": "set_model",
                "model": model,
            }
        }),
    )
}

fn write_claude_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message).context("failed to encode Claude message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Claude message delimiter")?;
    writer.flush().context("failed to flush Claude stdin")
}
