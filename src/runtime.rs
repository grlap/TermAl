/*
Agent runtime adapters
                    +----------------------+
session dispatch -->| runtime spawn/attach |
                    +----------+-----------+
                               |
          +--------------------+--------------------+
          |                    |                    |
          v                    v                    v
   Claude stdio         shared Codex         ACP stdio
   per session          app-server           per session
                        shared by all        Cursor / Gemini
                        Codex sessions
Each adapter translates agent-native protocol events into recorder callbacks so
the rest of the backend can work with one message model.
*/

/// Resolves source Codex home dir.
fn resolve_source_codex_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    let home = resolve_home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".codex"))
}

/// Resolves TermAl data dir.
fn resolve_termal_data_dir(default_workdir: &str) -> PathBuf {
    let base = resolve_home_dir().unwrap_or_else(|| PathBuf::from(default_workdir));
    base.join(".termal")
}

/// Resolves home dir.
fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Returns the current stderr log timestamp.
fn runtime_stderr_timestamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

/// Formats a runtime stderr log prefix.
fn format_runtime_stderr_prefix(label: &str, timestamp: &str) -> String {
    format!("{label} stderr [{timestamp}]>")
}

/// Resolves TermAl Codex home.
fn resolve_termal_codex_home(default_workdir: &str, scope: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir)
        .join("codex-home")
        .join(scope)
}

/// Prepares TermAl Codex home.
fn prepare_termal_codex_home(default_workdir: &str, scope: &str) -> Result<PathBuf> {
    let target_home = resolve_termal_codex_home(default_workdir, scope);
    fs::create_dir_all(&target_home)
        .with_context(|| format!("failed to create `{}`", target_home.display()))?;
    if let Ok(source_home) = resolve_source_codex_home_dir() {
        seed_termal_codex_home_from(&source_home, &target_home)?;
    }
    Ok(target_home)
}

/// Seeds TermAl Codex home from.
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

/// Syncs Codex home entry.
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

/// Syncs Codex home directory.
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

/// Syncs Codex home file.
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

/// Represents a Codex runtime command.
#[derive(Clone)]
enum CodexRuntimeCommand {
    Prompt {
        session_id: String,
        command: CodexPromptCommand,
    },
    /// Internal: sent by the thread-setup waiter after extracting the thread id
    /// from a `thread/start` or `thread/resume` response. The writer thread
    /// picks this up and fires the `turn/start` request.
    StartTurnAfterSetup {
        session_id: String,
        thread_id: String,
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
    /// Fire-and-forget JSON-RPC notification (no response expected).
    JsonRpcNotification { method: String },
    InterruptTurn {
        response_tx: Sender<std::result::Result<(), String>>,
        thread_id: String,
        turn_id: String,
    },
    RefreshModelList {
        response_tx: Sender<std::result::Result<Vec<SessionModelOption>, String>>,
    },
    /// Internal: sent by the model-list waiter to fetch the next page.
    RefreshModelListPage {
        cursor: String,
        accumulated: Vec<SessionModelOption>,
        page_count: usize,
        response_tx: Sender<std::result::Result<Vec<SessionModelOption>, String>>,
    },
}

/// Represents a Codex prompt command.
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

/// Represents a Codex JSON RPC response command.
#[derive(Clone, Debug, PartialEq)]
enum CodexJsonRpcResponsePayload {
    Result(Value),
    Error { code: i64, message: String },
}

#[derive(Clone, Debug, PartialEq)]
struct CodexJsonRpcResponseCommand {
    request_id: Value,
    payload: CodexJsonRpcResponsePayload,
}

const JSON_RPC_VERSION: &str = "2.0";

#[derive(Serialize)]
struct JsonRpcNotificationMessage<'a> {
    jsonrpc: &'static str,
    method: &'a str,
}

#[derive(Serialize)]
struct JsonRpcRequestMessage<'a> {
    jsonrpc: &'static str,
    id: Value,
    method: &'a str,
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResultResponseMessage {
    jsonrpc: &'static str,
    id: Value,
    result: Value,
}

#[derive(Serialize)]
struct JsonRpcErrorObject {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct JsonRpcErrorResponseMessage {
    jsonrpc: &'static str,
    id: Value,
    error: JsonRpcErrorObject,
}

/// Tracks an in-flight Codex JSON-RPC request.
struct PendingCodexJsonRpcRequest {
    request_id: String,
    response_rx: mpsc::Receiver<std::result::Result<Value, CodexResponseError>>,
}

/// Enumerates Codex approval kinds.
#[derive(Clone)]
enum CodexApprovalKind {
    CommandExecution,
    FileChange,
    Permissions { requested_permissions: Value },
}

/// Represents Codex pending approval.
#[derive(Clone)]
struct CodexPendingApproval {
    kind: CodexApprovalKind,
    request_id: Value,
}

/// Represents Codex pending user input.
#[derive(Clone)]
struct CodexPendingUserInput {
    questions: Vec<UserInputQuestion>,
    request_id: Value,
}

/// Represents Codex pending MCP elicitation.
#[derive(Clone)]
struct CodexPendingMcpElicitation {
    request: McpElicitationRequestPayload,
    request_id: Value,
}

/// Represents the Codex pending app request payload.
#[derive(Clone)]
struct CodexPendingAppRequest {
    request_id: Value,
}

/// Represents a Claude prompt command.
#[derive(Clone)]
struct ClaudePromptCommand {
    attachments: Vec<PromptImageAttachment>,
    text: String,
}

/// Represents a Claude runtime command.
#[derive(Clone)]
enum ClaudeRuntimeCommand {
    Prompt(ClaudePromptCommand),
    PermissionResponse(ClaudePermissionDecision),
    SetModel(String),
    SetPermissionMode(String),
}

/// Represents prompt image attachment.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromptImageAttachment {
    data: String,
    metadata: MessageImageAttachment,
}

/// Represents Claude pending approval.
#[derive(Clone)]
struct ClaudePendingApproval {
    permission_mode_for_session: Option<String>,
    request_id: String,
    tool_input: Value,
}

/// Enumerates Claude permission decisions.
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

/// Enumerates Claude control request actions.
enum ClaudeControlRequestAction {
    QueueApproval {
        title: String,
        command: String,
        detail: String,
        approval: ClaudePendingApproval,
    },
    Respond(ClaudePermissionDecision),
}

/// Represents a ACP runtime command.
#[derive(Clone)]
enum AcpRuntimeCommand {
    Prompt(AcpPromptCommand),
    JsonRpcMessage(Value),
    RefreshSessionConfig {
        command: AcpPromptCommand,
        response_tx: Sender<std::result::Result<(), String>>,
    },
}

/// Holds ACP launch options.
#[derive(Clone, Copy, Default)]
struct AcpLaunchOptions {
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Represents a ACP prompt command.
#[derive(Clone)]
struct AcpPromptCommand {
    cwd: String,
    cursor_mode: Option<CursorMode>,
    model: String,
    prompt: String,
    resume_session_id: Option<String>,
}

/// Represents ACP pending approval.
#[derive(Clone)]
struct AcpPendingApproval {
    allow_once_option_id: Option<String>,
    allow_always_option_id: Option<String>,
    reject_option_id: Option<String>,
    request_id: Value,
}

/// Tracks ACP runtime state.
#[derive(Default)]
struct AcpRuntimeState {
    current_session_id: Option<String>,
    is_loading_history: bool,
    supports_session_load: Option<bool>,
}

/// Tracks ACP turn state.
#[derive(Default)]
struct AcpTurnState {
    current_agent_message_id: Option<String>,
    thinking_buffer: String,
}

/// Holds turn configuration.
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

/// Defines the turn dispatch variants.
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

/// Defines the dispatch turn result variants.
enum DispatchTurnResult {
    Dispatched(TurnDispatch),
    Queued,
}

type CodexPendingRequestMap =
    Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, CodexResponseError>>>>>;
type AcpPendingRequestMap =
    Arc<Mutex<HashMap<String, Sender<std::result::Result<Value, AcpResponseError>>>>>;

/// Represents the pending ACP JSON RPC request payload.
struct PendingAcpJsonRpcRequest {
    request_id: String,
    response_rx: mpsc::Receiver<std::result::Result<Value, AcpResponseError>>,
}

/// Represents a Codex request failure.
#[derive(Clone, Debug, PartialEq)]
enum CodexResponseError {
    JsonRpc(String),
    /// The local wait for a Codex JSON-RPC response exceeded its deadline.
    /// Distinguished from `Transport` because a timeout does not indicate a
    /// broken connection — the app-server may still be healthy but slow. Callers
    /// should treat this as a per-session/per-turn failure rather than tearing
    /// down the entire shared runtime.
    Timeout(String),
    Transport(String),
}

impl CodexResponseError {
    /// Returns the transport detail when the request failed before Codex sent a JSON-RPC result.
    fn as_transport(&self) -> Option<&str> {
        match self {
            Self::JsonRpc(_) | Self::Timeout(_) => None,
            Self::Transport(detail) => Some(detail),
        }
    }
}

impl std::fmt::Display for CodexResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JsonRpc(detail) | Self::Timeout(detail) | Self::Transport(detail) => {
                f.write_str(detail)
            }
        }
    }
}

impl std::error::Error for CodexResponseError {}

/// Represents an ACP JSON RPC error payload.
#[derive(Clone, Debug, PartialEq)]
struct AcpJsonRpcError {
    code: Option<i64>,
    message: String,
    data: Option<Value>,
}

impl AcpJsonRpcError {
    /// Returns whether the error explicitly reports an invalid stored session identifier.
    fn is_invalid_session_identifier(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(acp_error_data_indicates_invalid_session_identifier)
            || self.message.contains("Invalid session identifier")
    }
}

impl std::fmt::Display for AcpJsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "ACP JSON-RPC error {code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for AcpJsonRpcError {}

/// Represents an ACP request failure.
#[derive(Clone, Debug, PartialEq)]
enum AcpResponseError {
    JsonRpc(AcpJsonRpcError),
    Transport(String),
}

impl AcpResponseError {
    /// Returns the JSON-RPC error details when the request reached the ACP runtime.
    fn as_json_rpc(&self) -> Option<&AcpJsonRpcError> {
        match self {
            Self::JsonRpc(error) => Some(error),
            Self::Transport(_) => None,
        }
    }
}

impl std::fmt::Display for AcpResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::JsonRpc(error) => error.fmt(f),
            Self::Transport(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for AcpResponseError {}

/// Represents pending subagent result.
struct PendingSubagentResult {
    title: String,
    summary: String,
    conversation_id: Option<String>,
    turn_id: Option<String>,
}

/// Tracks Codex turn state.
#[derive(Default)]
struct CodexTurnState {
    current_agent_message_id: Option<String>,
    streamed_agent_message_text_by_item_id: HashMap<String, String>,
    streamed_agent_message_item_ids: HashSet<String>,
    pending_subagent_results: Vec<PendingSubagentResult>,
    assistant_output_started: bool,
    first_visible_assistant_message_id: Option<String>,
}

/// Tracks session recorder state.
#[derive(Default)]
struct SessionRecorderState {
    command_messages: HashMap<String, String>,
    parallel_agents_messages: HashMap<String, String>,
    streaming_text_message_id: Option<String>,
}

fn reset_recorder_state_fields(recorder_state: &mut SessionRecorderState) {
    recorder_state.command_messages.clear();
    recorder_state.parallel_agents_messages.clear();
    recorder_state.streaming_text_message_id = None;
}

/// Tracks shared Codex session state.
#[derive(Default)]
struct SharedCodexSessionState {
    pending_turn_start_request_id: Option<String>,
    recorder: SessionRecorderState,
    thread_id: Option<String>,
    turn_id: Option<String>,
    completed_turn_id: Option<String>,
    turn_started: bool,
    turn_state: CodexTurnState,
}

#[cfg(test)]
const SHARED_CODEX_COMPLETED_TURN_GRACE_PERIOD: Duration = Duration::from_millis(25);
#[cfg(not(test))]
const SHARED_CODEX_COMPLETED_TURN_GRACE_PERIOD: Duration = Duration::from_secs(5);
const SHARED_CODEX_STDOUT_LINE_MAX_BYTES: usize = 16 * 1024 * 1024;
const SHARED_CODEX_STDOUT_LOG_PREVIEW_MAX_CHARS: usize = 200;
const SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES: usize = 5;

#[derive(Clone)]
struct SharedCodexCompletedTurnCleanup {
    session_id: String,
    completed_turn_id: String,
    due_at: std::time::Instant,
}

struct SharedCodexSessions {
    inner: Mutex<HashMap<String, SharedCodexSessionState>>,
    cleanup_tx: mpsc::Sender<SharedCodexCompletedTurnCleanup>,
}

impl SharedCodexSessions {
    fn new() -> SharedCodexSessionMap {
        let (cleanup_tx, cleanup_rx) = mpsc::channel();
        let sessions = Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            cleanup_tx,
        });
        spawn_shared_codex_completed_turn_cleanup_worker(&sessions, cleanup_rx);
        sessions
    }

    fn lock(
        &self,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'_, HashMap<String, SharedCodexSessionState>>>
    {
        self.inner.lock()
    }

    fn schedule_completed_turn_cleanup(&self, session_id: &str, completed_turn_id: &str) {
        let _ = self.cleanup_tx.send(SharedCodexCompletedTurnCleanup {
            session_id: session_id.to_owned(),
            completed_turn_id: completed_turn_id.to_owned(),
            due_at: std::time::Instant::now() + SHARED_CODEX_COMPLETED_TURN_GRACE_PERIOD,
        });
    }
}

type SharedCodexSessionMap = Arc<SharedCodexSessions>;
type SharedCodexThreadMap = Arc<Mutex<HashMap<String, String>>>;



/// Returns the Claude CLI model argument, or None to use Claude's own default selector.
fn claude_cli_model_arg(model: &str) -> Option<&str> {
    let model = model.trim();
    if model.is_empty() || model.eq_ignore_ascii_case("default") {
        return None;
    }
    Some(model)
}

enum ClaudeCliSessionArg<'a> {
    Resume(&'a str),
    SessionId(&'a str),
}

fn push_claude_cli_common_args(
    args: &mut Vec<String>,
    model: &str,
) {
    if let Some(model) = claude_cli_model_arg(model) {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args.extend([
        "-p".to_owned(),
        "--verbose".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
    ]);
}

fn push_claude_cli_permission_args(
    args: &mut Vec<String>,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
) {
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        args.extend(["--permission-mode".to_owned(), permission_mode.to_owned()]);
    }
    if let Some(effort) = effort.as_cli_value() {
        args.extend(["--effort".to_owned(), effort.to_owned()]);
    }
}

fn claude_cli_oneshot_args(
    model: &str,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    session_arg: ClaudeCliSessionArg<'_>,
) -> Vec<String> {
    let mut args = Vec::new();
    push_claude_cli_common_args(&mut args, model);
    args.push("--include-partial-messages".to_owned());
    push_claude_cli_permission_args(&mut args, approval_mode, effort);
    match session_arg {
        ClaudeCliSessionArg::Resume(session_id) => {
            args.extend(["--resume".to_owned(), session_id.to_owned()]);
        }
        ClaudeCliSessionArg::SessionId(session_id) => {
            args.extend(["--session-id".to_owned(), session_id.to_owned()]);
        }
    }
    args
}

fn claude_cli_persistent_args(
    model: &str,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    resume_session_id: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    push_claude_cli_common_args(&mut args, model);
    args.extend([
        "--input-format".to_owned(),
        "stream-json".to_owned(),
        "--include-partial-messages".to_owned(),
        "--permission-prompt-tool".to_owned(),
        "stdio".to_owned(),
    ]);
    push_claude_cli_permission_args(&mut args, approval_mode, effort);
    if let Some(resume_session_id) = resume_session_id {
        args.extend(["--resume".to_owned(), resume_session_id.to_owned()]);
    }
    args
}

/// Handles Claude model options.
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

/// Handles Claude agent commands.
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

/// Normalizes Claude agent command description.
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

/// Handles Claude model badges.
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

/// Parses Claude effort level.
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

/// Parses Codex reasoning effort.
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

/// Handles Codex reasoning effort rank.
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

/// Handles Codex model option.
fn codex_model_option<'a>(
    model: &str,
    model_options: &'a [SessionModelOption],
) -> Option<&'a SessionModelOption> {
    model_options.iter().find(|option| option.value == model)
}

/// Returns the matching session model option value.
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

/// Returns the normalized Codex reasoning effort.
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

/// Formats Codex reasoning efforts.
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

/// Handles Codex model options.
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

/// Writes Codex JSON RPC message.
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

fn json_rpc_notification_message(method: &str) -> Value {
    serde_json::to_value(JsonRpcNotificationMessage {
        jsonrpc: JSON_RPC_VERSION,
        method,
    })
    .expect("JSON-RPC notification should serialize")
}

fn json_rpc_request_message(request_id: impl Into<Value>, method: &str, params: Value) -> Value {
    serde_json::to_value(JsonRpcRequestMessage {
        jsonrpc: JSON_RPC_VERSION,
        id: request_id.into(),
        method,
        params,
    })
    .expect("JSON-RPC request should serialize")
}

fn json_rpc_result_response_message(request_id: impl Into<Value>, result: Value) -> Value {
    serde_json::to_value(JsonRpcResultResponseMessage {
        jsonrpc: JSON_RPC_VERSION,
        id: request_id.into(),
        result,
    })
    .expect("JSON-RPC result response should serialize")
}

fn json_rpc_error_response_message(
    request_id: impl Into<Value>,
    code: i64,
    message: impl Into<String>,
) -> Value {
    serde_json::to_value(JsonRpcErrorResponseMessage {
        jsonrpc: JSON_RPC_VERSION,
        id: request_id.into(),
        error: JsonRpcErrorObject {
            code,
            message: message.into(),
        },
    })
    .expect("JSON-RPC error response should serialize")
}

fn codex_json_rpc_response_message(response: &CodexJsonRpcResponseCommand) -> Value {
    match &response.payload {
        CodexJsonRpcResponsePayload::Result(result) => {
            json_rpc_result_response_message(response.request_id.clone(), result.clone())
        }
        CodexJsonRpcResponsePayload::Error { code, message } => {
            json_rpc_error_response_message(response.request_id.clone(), *code, message.clone())
        }
    }
}

/// Handles Codex request ID key.
fn codex_request_id_key(id: &Value) -> String {
    id.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| id.to_string())
}

/// Summarizes Codex JSON RPC error.
fn summarize_codex_json_rpc_error(error: &Value) -> String {
    if let Some(message) = error.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }

    summarize_error(error)
}

/// Handles Codex sandbox policy value.
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

/// Handles Codex command.
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

/// Resolves Codex executable.
fn resolve_codex_executable() -> Result<PathBuf> {
    let launcher =
        find_command_on_path("codex").ok_or_else(|| anyhow!("`codex` was not found on PATH"))?;
    Ok(resolve_codex_native_binary(&launcher).unwrap_or(launcher))
}

/// Finds command on path.
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



/// Handles home dir.
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

/// Handles display path for user.
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

/// Resolves Codex native binary.
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

/// Handles Codex target triple.
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

/// Describes Codex app server web search command.
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

/// Summarizes Codex app server web search output.
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

/// Spawns Claude runtime.
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
    let cwd = normalize_local_user_facing_path(&cwd);
    let mut command = Command::new("claude");
    command.current_dir(&cwd);
    command.args(claude_cli_persistent_args(
        &model,
        approval_mode,
        effort,
        resume_session_id.as_deref(),
    ));
    command.env("CLAUDE_CODE_ENTRYPOINT", "termal");

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
                        let action_result =
                            finish_claude_assistant_text_stream(&mut turn_state, &mut recorder)
                                .and_then(|_| {
                                    match action {
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
                        }
                                });

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
                        if let Err(err) = reader_state.finish_turn_ok_if_runtime_matches(
                            &reader_session_id,
                            &reader_runtime_token,
                        ) {
                            eprintln!(
                                "runtime state warning> failed to finalize Claude turn for session `{}`: {err:#}",
                                reader_session_id
                            );
                        }
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
                let timestamp = runtime_stderr_timestamp();
                let prefix = format_runtime_stderr_prefix("claude", &timestamp);
                eprintln!("{prefix} {line}");
            }
        });
    }

    {
        let wait_session_id = session_id.clone();
        let wait_state = state.clone();
        let wait_process = process.clone();
        let wait_runtime_token = RuntimeToken::Claude(runtime_id.clone());
        std::thread::spawn(move || match wait_process.wait() {
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
        });
    }

    Ok(ClaudeRuntimeHandle {
        runtime_id,
        input_tx,
        process,
    })
}

/// Writes Claude initialize.
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

/// Writes Claude prompt message.
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

/// Writes Claude permission response.
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

/// Writes Claude set permission mode.
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

/// Writes Claude set model.
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

/// Writes Claude message.
fn write_claude_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message).context("failed to encode Claude message")?;
    writer
        .write_all(b"\n")
        .context("failed to write Claude message delimiter")?;
    writer.flush().context("failed to flush Claude stdin")
}
