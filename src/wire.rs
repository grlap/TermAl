// Wire contract — all serde-serialized types used by the HTTP API + SSE
// stream, plus the small helpers (preview text, deep-link construction,
// done-summary selection) that operate on those types.
//
// This is the single source of truth for the JSON shape of every
// request/response envelope, every delta event, every persisted-through-
// the-wire structure. Everything here uses `#[serde(rename_all = "camelCase")]`
// so the Rust snake_case sees the right camelCase on the wire.
//
// Covers (previously lines 1725-4075 of api.rs):
//
// Error plumbing: `ApiErrorKind`, `ApiError` (+ IntoResponse).
//
// Agent + session taxonomy: `Agent`, `Project`, `Session`,
// `CodexThreadState`, `CodexApprovalPolicy`, `CodexSandboxMode`,
// `CodexReasoningEffort`, `ClaudeApprovalMode`, `ClaudeEffortLevel`,
// `CursorMode`, `GeminiApprovalMode`, `SessionStatus`, `Author`,
// `CommandStatus`, `ChangeType`, `ApprovalDecision`, `ParallelAgentStatus`.
//
// Interaction payloads: `UserInputQuestion`+`UserInputQuestionOption`,
// `McpElicitation*`, `InteractionRequestState`, `ParallelAgentProgress`,
// `PendingPrompt`.
//
// Message enum + rendering helpers: `Message`, `parallel_agents_preview_text`,
// `MessageImageAttachment`.
//
// Request / response DTOs: `ApprovalRequest`, `UserInputSubmissionRequest`,
// `McpElicitationSubmissionRequest`, `CodexAppRequestSubmissionRequest`,
// `CreateSessionRequest`, `CreateProjectRequest`, `UpdateAppSettingsRequest`,
// `FileQuery`, `InstructionSearchQuery`, `ReviewQuery`,
// `CodexThreadRollbackRequest`, `WriteFileRequest`, `FileResponse`,
// `DirectoryEntry`+`DirectoryResponse`, and the full Git diff + status
// response families.
//
// Terminal types: `TerminalCommandRequest`, `TerminalCommandResponse`,
// `TerminalStreamCancelGuard`, `TerminalCommandSseStream`,
// `TerminalCommandStreamEvent`, `TerminalOutputStream` +
// `TerminalOutputStreamPayload`, `TerminalStreamErrorPayload` + the
// terminal limit constants.
//
// Review document schema: `ReviewDocument`, `ReviewOrigin`,
// `ReviewFileEntry`, `ReviewThread`+`ReviewThreadComment`,
// `ReviewAnchor`, `ReviewCommentAuthor`, `ReviewThreadStatus`,
// `ReviewDocumentResponse`+`ReviewSummaryResponse`+`ReviewDocumentSummary`.
//
// Git operations: `GitFileActionRequest`, `GitCommitRequest`,
// `GitRepoActionRequest`.
//
// Top-level responses: `ErrorResponse`, `HealthResponse`,
// `StateResponse`, `SessionResponse`, `CreateSessionResponse`,
// `CreateProjectResponse`, `WorkspaceLayoutsResponse`,
// `WorkspaceLayoutResponse`, `WorkspaceLayoutSummary`,
// `WorkspaceLayoutDocument`, `PutWorkspaceLayoutRequest`,
// `WorkspaceControlPanelSide`.
//
// Agent commands + instructions: `AgentCommand`+`AgentCommandKind`,
// `AgentCommandsResponse`, `InstructionDocumentKind`,
// `InstructionRelation`, `InstructionSearchResponse`,
// `InstructionSearchMatch`, `InstructionRootPath`+`InstructionPathStep`,
// `InstructionDocumentInternal`, `InstructionSearchGraph`.
//
// Codex state: `CodexState`, `CodexNoticeKind`, `CodexNoticeLevel`,
// `CodexNotice`, `CodexRateLimits`+`CodexRateLimitWindow`.
//
// Agent readiness: `AgentReadinessStatus`, `AgentReadiness`.
//
// Session model options: `SessionModelOption`.
//
// Project digest: `ProjectDigestAction`, `ProjectDigestResponse`,
// `ProjectDigestInputs`, `ProjectApprovalTarget`,
// `ProjectDigestSummary`, `ProjectActionId`, `PickProjectRootResponse`,
// plus helpers: `build_project_deep_link`, `normalize_project_text`,
// `active_project_status_text`, `select_project_done_summary`,
// `default_project_done_summary`, `project_git_done_summary`,
// `latest_project_progress_summary`,
// `project_progress_summary_for_message`,
// `find_latest_project_pending_approval`, `has_live_pending_approval`,
// `find_latest_project_pending_nonapproval_interaction`.
//
// SSE delta events: `DeltaEvent` (TextDelta / TextReplace /
// CommandUpdate / ParallelAgentsUpdate / SessionCreated /
// MessageCreated / OrchestratorsUpdated).
//
// Extracted from api.rs as the single largest extraction of the refactor.
// api.rs can now stay focused on axum HTTP route handlers + the
// `impl AppState` block.

/// Represents the API error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiErrorKind {
    #[cfg_attr(not(unix), allow(dead_code))]
    GitDocumentBecameSymlink,
    GitDocumentInvalidUtf8,
    GitDocumentNotFile,
    GitDocumentNotFound,
    GitDocumentTooLarge,
}

#[derive(Debug)]
struct ApiError {
    message: String,
    status: StatusCode,
    kind: Option<ApiErrorKind>,
}

impl ApiError {
    // Status-specific constructors — mirror the HTTP status codes they
    // wrap. Nothing interesting to document; the method name says it.
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
            kind: None,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
            kind: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
            kind: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: None,
        }
    }

    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_GATEWAY,
            kind: None,
        }
    }

    /// Builds the value from status.
    fn from_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status,
            kind: None,
        }
    }

    /// Tags internal error handling without changing the wire response.
    fn with_kind(mut self, kind: ApiErrorKind) -> Self {
        self.kind = Some(kind);
        self
    }
}

impl IntoResponse for ApiError {
    /// Converts the value into response.
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}


/// Defines the agent variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
enum Agent {
    Codex,
    Claude,
    Cursor,
    Gemini,
}

impl Agent {
    /// Parses the CLI-arg iterator into an `Agent`. Accepts an
    /// `--agent <value>` pair or a standalone agent name; unknown
    /// arguments fail fast. Defaults to `Agent::Codex` if no arg
    /// is given.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--agent" => {
                    let value = args.next().context("missing value after `--agent`")?;
                    return Self::from_str(&value);
                }
                "codex" => return Ok(Self::Codex),
                "claude" => return Ok(Self::Claude),
                "cursor" | "cursor-agent" => return Ok(Self::Cursor),
                "gemini" | "gemini-cli" => return Ok(Self::Gemini),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    /// Builds the value from str.
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "gemini" | "gemini-cli" => Ok(Self::Gemini),
            other => {
                bail!("unknown agent `{other}`; expected `codex`, `claude`, `cursor`, or `gemini`")
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
        }
    }

    fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Cursor => "CR",
            Self::Gemini => "GM",
        }
    }

    /// Returns the default model.
    fn default_model(self) -> &'static str {
        match self {
            Self::Codex => "gpt-5.4",
            Self::Claude => "default",
            Self::Cursor => "auto",
            Self::Gemini => "auto",
        }
    }

    /// Returns whether Codex prompt settings.
    fn supports_codex_prompt_settings(self) -> bool {
        matches!(self, Self::Codex)
    }

    /// Returns whether Claude approval mode.
    fn supports_claude_approval_mode(self) -> bool {
        matches!(self, Self::Claude)
    }

    /// Returns whether cursor mode.
    fn supports_cursor_mode(self) -> bool {
        matches!(self, Self::Cursor)
    }

    /// Returns whether Gemini approval mode.
    fn supports_gemini_approval_mode(self) -> bool {
        matches!(self, Self::Gemini)
    }

    /// Handles ACP runtime.
    fn acp_runtime(self) -> Option<AcpAgent> {
        match self {
            Self::Cursor => Some(AcpAgent::Cursor),
            Self::Gemini => Some(AcpAgent::Gemini),
            _ => None,
        }
    }
}

/// Represents project.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    id: String,
    name: String,
    root_path: String,
    remote_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_project_id: Option<String>,
}

/// Represents session.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    id: String,
    name: String,
    emoji: String,
    agent: Agent,
    workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    model_options: Vec<SessionModelOption>,
    approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_mode: Option<CursorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_effort: Option<ClaudeEffortLevel>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gemini_approval_mode: Option<GeminiApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_session_id: Option<String>,
    #[serde(default)]
    agent_commands_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_thread_state: Option<CodexThreadState>,
    status: SessionStatus,
    preview: String,
    messages: Vec<Message>,
    #[serde(default = "session_messages_loaded_default", rename = "messagesLoaded")]
    messages_loaded: bool,
    #[serde(default, rename = "messageCount")]
    message_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_prompts: Vec<PendingPrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_mutation_stamp: Option<u64>,
}

fn session_messages_loaded_default() -> bool {
    true
}

/// Tracks Codex thread state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CodexThreadState {
    Active,
    Archived,
}

/// Defines the Codex approval policy variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl CodexApprovalPolicy {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }
}

/// Enumerates Codex sandbox modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Defines the Codex reasoning effort variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

impl CodexReasoningEffort {
    /// Returns the API value representation.
    fn as_api_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

/// Enumerates Claude approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
    Plan,
}

/// Defines the Claude effort level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ClaudeEffortLevel {
    Default,
    Low,
    Medium,
    High,
    Max,
}

impl ClaudeEffortLevel {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Max => Some("max"),
        }
    }
}

impl ClaudeApprovalMode {
    /// Returns the `--permission-mode` flag value to pass to the
    /// Claude CLI at spawn time, or `None` to let the CLI choose its
    /// default. `Plan` is the only mode that must be requested
    /// explicitly at launch.
    fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Ask | Self::AutoApprove => None,
        }
    }

    /// Returns the `permission_mode` value sent over the Claude
    /// session's NDJSON control channel when the user flips modes
    /// at runtime (vs. at launch). Differs from
    /// `initial_cli_permission_mode` because the runtime channel
    /// uses the string "default" in place of `None`.
    fn session_cli_permission_mode(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask | Self::AutoApprove => "default",
        }
    }
}

/// Enumerates cursor modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorMode {
    Agent,
    Plan,
    Ask,
}

impl CursorMode {
    /// Returns the ACP value representation.
    fn as_acp_value(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

/// Enumerates Gemini approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GeminiApprovalMode {
    Default,
    AutoEdit,
    Yolo,
    Plan,
}

impl GeminiApprovalMode {
    /// Returns the cli value representation.
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AutoEdit => "auto_edit",
            Self::Yolo => "yolo",
            Self::Plan => "plan",
        }
    }
}

/// Enumerates session states.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SessionStatus {
    Active,
    Idle,
    Approval,
    Error,
}

/// Defines the author variants.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Author {
    You,
    Assistant,
}

/// Enumerates command states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum CommandStatus {
    Running,
    Success,
    Error,
}

impl CommandStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Defines the change type variants.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChangeType {
    Edit,
    Create,
}

/// Enumerates approval decisions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ApprovalDecision {
    Pending,
    Interrupted,
    Canceled,
    Accepted,
    AcceptedForSession,
    Rejected,
}


/// Represents the approval request payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    decision: ApprovalDecision,
}

/// Represents the user input submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputSubmissionRequest {
    answers: BTreeMap<String, Vec<String>>,
}

/// Represents the MCP elicitation submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationSubmissionRequest {
    action: McpElicitationAction,
    #[serde(default)]
    content: Option<Value>,
}

/// Represents the Codex app request submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexAppRequestSubmissionRequest {
    result: Value,
}

/// Represents the create session request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    agent: Option<Agent>,
    name: Option<String>,
    workdir: Option<String>,
    project_id: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Represents the create project request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectRequest {
    name: Option<String>,
    root_path: String,
    #[serde(default = "default_local_remote_id")]
    remote_id: String,
}

/// Represents the update app settings request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAppSettingsRequest {
    default_codex_reasoning_effort: Option<CodexReasoningEffort>,
    default_claude_approval_mode: Option<ClaudeApprovalMode>,
    default_claude_effort: Option<ClaudeEffortLevel>,
    remotes: Option<Vec<RemoteConfig>>,
}

/// Represents file query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileQuery {
    path: String,
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents instruction search query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchQuery {
    q: String,
    session_id: String,
}

/// Represents review query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewQuery {
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents the Codex thread rollback request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexThreadRollbackRequest {
    #[serde(default = "default_codex_thread_rollback_turns")]
    num_turns: usize,
}

/// Returns the default Codex thread rollback turns.
fn default_codex_thread_rollback_turns() -> usize {
    1
}

/// Represents the write file request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteFileRequest {
    path: String,
    content: String,
    base_hash: Option<String>,
    #[serde(default)]
    overwrite: bool,
    project_id: Option<String>,
    session_id: Option<String>,
}

/// Represents the file response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileResponse {
    path: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

/// Enumerates file system entry kinds.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum FileSystemEntryKind {
    Directory,
    File,
}

/// Represents directory entry.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    kind: FileSystemEntryKind,
    name: String,
    path: String,
}

/// Represents the directory response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryResponse {
    entries: Vec<DirectoryEntry>,
    name: String,
    path: String,
}


/// Represents the error response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorResponse {
    error: String,
}

/// Represents the health response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    #[serde(default)]
    supports_inline_orchestrator_templates: bool,
    /// UUID generated at `AppState::new_with_paths` boot. Stable for
    /// the lifetime of the process, changes on every restart. Clients
    /// use a mismatch between this and their last-seen id to detect a
    /// server restart deterministically — see `shouldAdoptSnapshotRevision`
    /// in the frontend. `#[serde(default)]` so older servers that do
    /// not emit the field still deserialize to an empty string
    /// (treated as "unknown — do not trust for restart detection").
    #[serde(default)]
    server_instance_id: String,
}

/// Represents the send message request payload.
#[derive(Deserialize, Serialize)]
struct SendMessageRequest {
    text: String,
    #[serde(default, rename = "expandedText")]
    expanded_text: Option<String>,
    #[serde(default)]
    attachments: Vec<SendMessageAttachmentRequest>,
}

/// Represents the send message attachment request payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageAttachmentRequest {
    data: String,
    file_name: Option<String>,
    media_type: String,
}

/// Represents the update session settings request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSessionSettingsRequest {
    name: Option<String>,
    model: Option<String>,
    approval_policy: Option<CodexApprovalPolicy>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    cursor_mode: Option<CursorMode>,
    claude_approval_mode: Option<ClaudeApprovalMode>,
    claude_effort: Option<ClaudeEffortLevel>,
    gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Represents session model option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionModelOption {
    label: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_claude_effort_levels: Vec<ClaudeEffortLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_reasoning_effort: Option<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supported_reasoning_efforts: Vec<CodexReasoningEffort>,
}

impl SessionModelOption {
    /// Builds the plain response value.
    #[cfg(test)]
    fn plain(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: None,
            supported_reasoning_efforts: Vec::new(),
        }
    }
}

/// Maximum number of Codex notices retained in state and sent over SSE.
const CODEX_NOTICE_CAP: usize = 5;

/// Tracks Codex state.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limits: Option<CodexRateLimits>,
    /// Most-recent-first Codex notices, capped at [`CODEX_NOTICE_CAP`] by
    /// `AppState::note_codex_notice`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notices: Vec<CodexNotice>,
}

impl CodexState {
    /// Returns whether empty.
    fn is_empty(&self) -> bool {
        self.rate_limits.is_none() && self.notices.is_empty()
    }
}

/// Enumerates Codex notice kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CodexNoticeKind {
    ConfigWarning,
    DeprecationNotice,
    RuntimeNotice,
}

/// Defines the Codex notice level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CodexNoticeLevel {
    Info,
    Warning,
}

/// Represents Codex notice.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexNotice {
    kind: CodexNoticeKind,
    level: CodexNoticeLevel,
    title: String,
    detail: String,
    timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

/// Represents Codex rate limits.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secondary: Option<CodexRateLimitWindow>,
}

/// Represents Codex rate limit window.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_duration_mins: Option<u64>,
}

/// Enumerates agent readiness states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentReadinessStatus {
    Ready,
    Missing,
    NeedsSetup,
}

/// Represents agent readiness.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentReadiness {
    agent: Agent,
    status: AgentReadinessStatus,
    blocking: bool,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_path: Option<String>,
}

/// Represents the state response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateResponse {
    revision: u64,
    /// UUID generated at `AppState::new_with_paths` boot; see
    /// `HealthResponse::server_instance_id` for semantics. Carried on
    /// every snapshot so clients can distinguish "revision decreased
    /// because the server restarted" from "revision decreased because
    /// this response is stale". `#[serde(default)]` for forward-compat
    /// with older servers.
    #[serde(default)]
    server_instance_id: String,
    #[serde(default)]
    codex: CodexState,
    #[serde(default)]
    agent_readiness: Vec<AgentReadiness>,
    preferences: AppPreferences,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default)]
    orchestrators: Vec<OrchestratorInstance>,
    #[serde(default)]
    workspaces: Vec<WorkspaceLayoutSummary>,
    #[serde(default)]
    sessions: Vec<Session>,
}

/// Represents one full session response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    revision: u64,
    session: Session,
    /// See `StateResponse::server_instance_id` — same semantics. The
    /// frontend's `adoptFetchedSession` uses a mismatch against the
    /// last-seen id to accept a revision downgrade after a server
    /// restart. Without this field, a session hydration in flight
    /// across a restart could be silently rejected by the monotonic
    /// revision guard until the safety-net pollers re-fetch.
    /// `#[serde(default)]` for forward-compat with older servers that
    /// do not emit the field.
    #[serde(default)]
    server_instance_id: String,
}

/// Defines the workspace control panel side variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum WorkspaceControlPanelSide {
    Left,
    Right,
}

/// Represents the workspace layout document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutDocument {
    id: String,
    revision: u64,
    updated_at: String,
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density_percent: Option<u32>,
    workspace: Value,
}

/// Represents the workspace layout response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutResponse {
    layout: WorkspaceLayoutDocument,
}

/// Summarizes workspace layout.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutSummary {
    id: String,
    revision: u64,
    updated_at: String,
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    density_percent: Option<u32>,
}

/// Represents the workspace layouts response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceLayoutsResponse {
    workspaces: Vec<WorkspaceLayoutSummary>,
}

/// Represents the put workspace layout request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutWorkspaceLayoutRequest {
    control_panel_side: WorkspaceControlPanelSide,
    #[serde(default)]
    theme_id: Option<String>,
    #[serde(default)]
    style_id: Option<String>,
    #[serde(default)]
    font_size_px: Option<u32>,
    #[serde(default)]
    editor_font_size_px: Option<u32>,
    #[serde(default)]
    density_percent: Option<u32>,
    workspace: Value,
}

/// Represents the create session response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
    session: Session,
    revision: u64,
    /// See `StateResponse::server_instance_id` — same semantics. The
    /// frontend's `adoptCreatedSessionResponse` uses a mismatch
    /// against the last-seen id to accept a revision downgrade after
    /// a server restart, which is the common case for this response
    /// (POST sent from a stale browser tab against a freshly started
    /// server). `#[serde(default)]` for forward-compat with older
    /// servers that do not emit the field.
    #[serde(default)]
    server_instance_id: String,
}

/// Represents the create project response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectResponse {
    project_id: String,
    state: StateResponse,
}


/// Represents a agent command.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommand {
    #[serde(default)]
    kind: AgentCommandKind,
    name: String,
    description: String,
    content: String,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    argument_hint: Option<String>,
}

/// Represents the agent commands response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCommandsResponse {
    commands: Vec<AgentCommand>,
}

/// Enumerates agent command kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentCommandKind {
    PromptTemplate,
    NativeSlash,
}

impl Default for AgentCommandKind {
    /// Builds the default value.
    fn default() -> Self {
        Self::PromptTemplate
    }
}

/// Enumerates instruction document kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstructionDocumentKind {
    RootInstruction,
    CommandInstruction,
    ReviewerInstruction,
    RulesInstruction,
    SkillInstruction,
    ReferencedInstruction,
}

/// Defines the instruction relation variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstructionRelation {
    MarkdownLink,
    FileReference,
    DirectoryDiscovery,
}

/// Represents the instruction search response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchResponse {
    matches: Vec<InstructionSearchMatch>,
    query: String,
    workdir: String,
}

/// Represents instruction search match.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionSearchMatch {
    line: usize,
    path: String,
    root_paths: Vec<InstructionRootPath>,
    text: String,
}

/// Represents instruction root path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionRootPath {
    root_kind: InstructionDocumentKind,
    root_path: String,
    steps: Vec<InstructionPathStep>,
}

/// Represents instruction path step.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstructionPathStep {
    excerpt: String,
    from_path: String,
    line: usize,
    relation: InstructionRelation,
    to_path: String,
}

/// Represents instruction document internal.
#[derive(Clone, Debug)]
struct InstructionDocumentInternal {
    kind: InstructionDocumentKind,
    lines: Vec<String>,
    path: PathBuf,
}

/// Represents instruction search graph.
#[derive(Clone, Debug, Default)]
struct InstructionSearchGraph {
    documents: HashMap<String, InstructionDocumentInternal>,
    incoming: HashMap<String, Vec<InstructionPathStep>>,
}

/// Represents the pick project root response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PickProjectRootResponse {
    path: Option<String>,
}


/// Defines the delta event variants.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum DeltaEvent {
    SessionCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        session: Session,
    },
    MessageCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    MessageUpdated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        message: Message,
        preview: String,
        status: SessionStatus,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    TextDelta {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    TextReplace {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    CommandUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        command: String,
        #[serde(rename = "commandLanguage", skip_serializing_if = "Option::is_none")]
        command_language: Option<String>,
        output: String,
        #[serde(rename = "outputLanguage", skip_serializing_if = "Option::is_none")]
        output_language: Option<String>,
        status: CommandStatus,
        preview: String,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    ParallelAgentsUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        agents: Vec<ParallelAgentProgress>,
        preview: String,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    CodexUpdated {
        revision: u64,
        codex: CodexState,
    },
    OrchestratorsUpdated {
        revision: u64,
        orchestrators: Vec<OrchestratorInstance>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<Session>,
    },
}

