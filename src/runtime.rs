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
///
/// Shape mirrors the ACP handshake contract documented in `src/acp.rs`:
/// - `capabilities` is `None` until the `initialize` handshake completes,
///   then carries the typed capability bundle learned from the response.
///   Future capability fields (model list shape, tool rules, etc.) land
///   inside `AcpCapabilities` rather than growing this struct flat.
/// - `current_session_id` is `None` during spawn / initialize /
///   authenticate phases, then carries the ACP conversation id after
///   `session/new` or `session/load` succeeds.
/// - `is_loading_history` is true only during an in-flight `session/load`
///   call. Outside that window it must be false.
#[derive(Default)]
struct AcpRuntimeState {
    capabilities: Option<AcpCapabilities>,
    current_session_id: Option<String>,
    is_loading_history: bool,
}

/// Capability bundle learned from the `initialize` response. `None`
/// inside `AcpRuntimeState.capabilities` means "initialize has not
/// completed yet"; once present, the contract is "the value inside is
/// authoritative for this runtime's lifetime".
#[derive(Clone, Default)]
struct AcpCapabilities {
    /// `Some(true)` — remote confirmed `session/load` support via the
    /// initialize response. `Some(false)` — remote confirmed it is
    /// NOT supported. `None` — initialize response did not carry the
    /// capability flag at all (older agents), so we probe
    /// optimistically and upgrade to `Some(true)` on first successful
    /// load or to `Some(false)` on a definitive not-supported error.
    /// The tri-state is intentional — see `ensure_acp_session_ready`
    /// for the "not known to be unsupported; try anyway" rule.
    supports_session_load: Option<bool>,
}

impl AcpCapabilities {
    /// Returns true unless `supports_session_load` is explicitly
    /// known to be false. Encodes the "try optimistically when
    /// capability flag is absent" rule so call sites don't have to
    /// repeat the negated Option comparison.
    fn session_load_supported_or_unknown(&self) -> bool {
        self.supports_session_load != Some(false)
    }
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

