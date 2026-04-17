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
    /// Handles bad request.
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
            kind: None,
        }
    }

    /// Handles conflict.
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
            kind: None,
        }
    }

    /// Handles not found.
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
            kind: None,
        }
    }

    /// Handles internal.
    fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: None,
        }
    }

    /// Handles bad gateway.
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
    /// Handles parse.
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

    /// Handles name.
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
        }
    }

    /// Handles avatar.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_prompts: Vec<PendingPrompt>,
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
    /// Handles initial cli permission mode.
    fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::Ask | Self::AutoApprove => None,
        }
    }

    /// Handles session cli permission mode.
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
    /// Handles label.
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

/// Enumerates parallel agent states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ParallelAgentStatus {
    Initializing,
    Running,
    Completed,
    Error,
}

/// Represents user input question option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInputQuestionOption {
    description: String,
    label: String,
}

/// Represents user input question.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInputQuestion {
    header: String,
    id: String,
    #[serde(default, rename = "isOther")]
    is_other: bool,
    #[serde(default, rename = "isSecret")]
    is_secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    options: Option<Vec<UserInputQuestionOption>>,
    question: String,
}

/// Enumerates MCP elicitation actions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

/// Enumerates MCP elicitation request modes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
enum McpElicitationRequestMode {
    Form {
        #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        message: String,
        #[serde(rename = "requestedSchema")]
        requested_schema: Value,
    },
    Url {
        #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
        #[serde(rename = "elicitationId")]
        elicitation_id: String,
        message: String,
        url: String,
    },
}

/// Represents the MCP elicitation request payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpElicitationRequestPayload {
    #[serde(rename = "threadId")]
    thread_id: String,
    #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    server_name: String,
    #[serde(flatten)]
    mode: McpElicitationRequestMode,
}

/// Tracks interaction request state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum InteractionRequestState {
    Pending,
    Submitted,
    Interrupted,
    Canceled,
}

/// Represents parallel agent progress.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ParallelAgentProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    id: String,
    status: ParallelAgentStatus,
    title: String,
}

/// Represents message image attachment.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageImageAttachment {
    byte_size: usize,
    file_name: String,
    media_type: String,
}

/// Represents pending prompt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingPrompt {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<MessageImageAttachment>,
    id: String,
    timestamp: String,
    text: String,
    #[serde(
        default,
        rename = "expandedText",
        skip_serializing_if = "Option::is_none"
    )]
    expanded_text: Option<String>,
}

/// Defines the message variants.
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Message {
    Text {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<MessageImageAttachment>,
        id: String,
        timestamp: String,
        author: Author,
        text: String,
        #[serde(
            default,
            rename = "expandedText",
            skip_serializing_if = "Option::is_none"
        )]
        expanded_text: Option<String>,
    },
    Thinking {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        lines: Vec<String>,
    },
    Command {
        id: String,
        timestamp: String,
        author: Author,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        output: String,
        #[serde(
            default,
            rename = "outputLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        output_language: Option<String>,
        status: CommandStatus,
    },
    Diff {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(
            default,
            rename = "changeSetId",
            skip_serializing_if = "Option::is_none"
        )]
        change_set_id: Option<String>,
        #[serde(rename = "filePath")]
        file_path: String,
        summary: String,
        diff: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(rename = "changeType")]
        change_type: ChangeType,
    },
    Markdown {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        markdown: String,
    },
    #[serde(rename = "subagentResult")]
    SubagentResult {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        summary: String,
        #[serde(
            default,
            rename = "conversationId",
            skip_serializing_if = "Option::is_none"
        )]
        conversation_id: Option<String>,
        #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    #[serde(rename = "parallelAgents")]
    ParallelAgents {
        id: String,
        timestamp: String,
        author: Author,
        agents: Vec<ParallelAgentProgress>,
    },
    #[serde(rename = "fileChanges")]
    FileChanges {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        files: Vec<FileChangeSummaryEntry>,
    },
    Approval {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        command: String,
        #[serde(
            default,
            rename = "commandLanguage",
            skip_serializing_if = "Option::is_none"
        )]
        command_language: Option<String>,
        detail: String,
        decision: ApprovalDecision,
    },
    #[serde(rename = "userInputRequest")]
    UserInputRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        questions: Vec<UserInputQuestion>,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedAnswers",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_answers: Option<BTreeMap<String, Vec<String>>>,
    },
    #[serde(rename = "mcpElicitationRequest")]
    McpElicitationRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        request: McpElicitationRequestPayload,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedAction",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_action: Option<McpElicitationAction>,
        #[serde(
            default,
            rename = "submittedContent",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_content: Option<Value>,
    },
    #[serde(rename = "codexAppRequest")]
    CodexAppRequest {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        detail: String,
        method: String,
        params: Value,
        state: InteractionRequestState,
        #[serde(
            default,
            rename = "submittedResult",
            skip_serializing_if = "Option::is_none"
        )]
        submitted_result: Option<Value>,
    },
}

impl Message {
    /// Handles ID.
    fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::Thinking { id, .. }
            | Self::Command { id, .. }
            | Self::Diff { id, .. }
            | Self::Markdown { id, .. }
            | Self::SubagentResult { id, .. }
            | Self::ParallelAgents { id, .. }
            | Self::FileChanges { id, .. }
            | Self::Approval { id, .. }
            | Self::UserInputRequest { id, .. }
            | Self::McpElicitationRequest { id, .. }
            | Self::CodexAppRequest { id, .. } => id,
        }
    }

    /// Handles preview text.
    fn preview_text(&self) -> Option<String> {
        match self {
            Self::Text {
                text, attachments, ..
            } => Some(prompt_preview_text(text, attachments)),
            Self::Thinking { title, .. } => Some(make_preview(title)),
            Self::Markdown { title, .. } => Some(make_preview(title)),
            Self::Approval { title, .. } => Some(make_preview(title)),
            Self::UserInputRequest { title, .. } => Some(make_preview(title)),
            Self::McpElicitationRequest { title, .. } => Some(make_preview(title)),
            Self::CodexAppRequest { title, .. } => Some(make_preview(title)),
            Self::Diff { summary, .. } => Some(make_preview(summary)),
            Self::SubagentResult { .. } => None,
            Self::FileChanges { .. } => None,
            Self::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
            Self::Command { .. } => None,
        }
    }
}

/// Handles parallel agents preview text.
fn parallel_agents_preview_text(agents: &[ParallelAgentProgress]) -> String {
    let count = agents.len();
    let label = if count == 1 { "agent" } else { "agents" };
    let active_count = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                ParallelAgentStatus::Initializing | ParallelAgentStatus::Running
            )
        })
        .count();

    if active_count > 0 {
        return make_preview(&format!("Running {count} {label}"));
    }

    let error_count = agents
        .iter()
        .filter(|agent| agent.status == ParallelAgentStatus::Error)
        .count();
    if error_count > 0 {
        let errors = if error_count == 1 { "error" } else { "errors" };
        return make_preview(&format!(
            "{count} {label} finished with {error_count} {errors}"
        ));
    }

    make_preview(&format!("Completed {count} {label}"))
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

/// Represents Git status file.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    index_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worktree_status: Option<String>,
}

/// Represents the Git status response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusResponse {
    ahead: usize,
    behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    files: Vec<GitStatusFile>,
    is_clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    workdir: String,
}

/// Defines the Git diff section variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffSection {
    Staged,
    Unstaged,
}

impl GitDiffSection {
    /// Returns the key representation.
    fn as_key(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }

    /// Handles summary label.
    fn summary_label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
        }
    }
}

/// Enumerates Git file actions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitFileAction {
    Stage,
    Unstage,
    Revert,
}

/// Represents the Git diff request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffRequest {
    original_path: Option<String>,
    path: String,
    section_id: GitDiffSection,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Defines the Git diff change type variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffChangeType {
    Edit,
    Create,
}

/// Represents the Git diff response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffResponse {
    change_type: GitDiffChangeType,
    change_set_id: String,
    diff: String,
    diff_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_enrichment_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_content: Option<GitDiffDocumentContent>,
    summary: String,
}

/// Represents full document sides for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentContent {
    before: GitDiffDocumentSide,
    after: GitDiffDocumentSide,
    can_edit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_blocked_reason: Option<String>,
    is_complete_document: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Represents one full document side for a Git diff.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitDiffDocumentSide {
    content: String,
    source: GitDiffDocumentSideSource,
}

/// Defines where a full document side came from.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum GitDiffDocumentSideSource {
    Head,
    Index,
    Worktree,
    Empty,
}

/// Represents the Git commit response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitResponse {
    status: GitStatusResponse,
    summary: String,
}

/// Represents the Git repo action response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionResponse {
    status: GitStatusResponse,
    summary: String,
}

const TERMINAL_COMMAND_MAX_CHARS: usize = 20_000;
/// Upper bound on the `workdir` field of a terminal command request. Real
/// filesystem paths stay well under this; the cap is a defense-in-depth
/// limit so a client cannot POST a megabyte of whitespace-stripped text
/// that then flows into `resolve_project_scoped_requested_path` or over
/// the wire to the remote proxy. Paired with explicit NUL-byte rejection
/// in `validate_terminal_workdir`.
const TERMINAL_WORKDIR_MAX_CHARS: usize = 4_096;
/// Maximum captured terminal output per stream. Stdout and stderr each get
/// their own budget on both local runs and remote JSON proxy responses.
const TERMINAL_OUTPUT_MAX_BYTES: usize = 512 * 1024;
const TERMINAL_STREAM_EVENT_QUEUE_CAPACITY: usize = 256;
const TERMINAL_COMMAND_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Upper bound on the bytes the remote proxy will buffer while waiting to
/// find the next SSE frame delimiter. A completion frame carries the full
/// `TerminalCommandResponse`, including up to `TERMINAL_OUTPUT_MAX_BYTES` of
/// stdout plus the same of stderr, plus the echoed command string and
/// workdir. JSON encoding can expand each byte up to 6× (ASCII control
/// characters become `\u00XX`) and SSE framing adds further overhead, so the
/// worst-case legitimate completion frame is roughly
/// `TERMINAL_OUTPUT_MAX_BYTES * 12 + ~200 KiB`. Cap at 16× the raw output
/// limit (8 MiB) so that envelope fits with comfortable headroom while still
/// bounding memory if a remote misbehaves and never emits a delimiter.
const TERMINAL_REMOTE_SSE_PENDING_MAX_BYTES: usize = TERMINAL_OUTPUT_MAX_BYTES * 16;

/// Remote proxy timeout for terminal commands. This must cover the remote child
/// wait, post-timeout process cleanup, stdout/stderr reader joins, JSON
/// encoding/decoding, and a small network scheduling margin.
const REMOTE_TERMINAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(90);

/// Maximum time to wait for terminal output-reader threads after the child
/// process exits. Background children that inherit stdout/stderr can keep the
/// pipe open indefinitely; this prevents the request from blocking forever.
const TERMINAL_OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Represents a terminal command request.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandRequest {
    command: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents a terminal command response.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCommandResponse {
    command: String,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    output_truncated: bool,
    shell: String,
    stderr: String,
    stdout: String,
    success: bool,
    timed_out: bool,
    workdir: String,
}

type TerminalCommandStreamSender = tokio::sync::mpsc::Sender<TerminalCommandStreamEvent>;

struct TerminalStreamCancelGuard {
    cancellation: Arc<AtomicBool>,
}

impl Drop for TerminalStreamCancelGuard {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::SeqCst);
    }
}

/// SSE stream adapter for a streaming terminal command.
///
/// **Field drop order is load-bearing.** Rust drops struct fields in
/// declaration order, so `event_rx` is dropped *before* `_cancel_on_drop`.
/// That order is required so that any worker still parked inside
/// `blocking_send(..)` on the matching sender observes the channel closing
/// and returns immediately — the spawned worker then releases its
/// concurrency permit and exits. Only after `event_rx` is torn down does
/// the cancellation guard flip, which asks other parts of the pipeline
/// (the SSE forwarder, the remote read adapter, the streaming child wait)
/// to stop. Swapping the field order to "flip the cancellation flag
/// first" would leave the worker parked inside `blocking_send` for up to
/// one `TERMINAL_COMMAND_CANCEL_POLL_INTERVAL` tick before the next
/// `try_send` sees the flag, regressing cancellation latency without
/// failing any existing test.
struct TerminalCommandSseStream {
    event_rx: tokio::sync::mpsc::Receiver<TerminalCommandStreamEvent>,
    _cancel_on_drop: TerminalStreamCancelGuard,
}

impl futures_core::Stream for TerminalCommandSseStream {
    type Item = std::result::Result<Event, Infallible>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.event_rx).poll_recv(cx) {
            std::task::Poll::Ready(Some(event)) => {
                std::task::Poll::Ready(Some(Ok(terminal_command_sse_event(event))))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

enum TerminalCommandStreamEvent {
    Output {
        stream: TerminalOutputStream,
        text: String,
    },
    Complete(TerminalCommandResponse),
    Error {
        error: String,
        status: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum TerminalOutputStream {
    Stdout,
    Stderr,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputStreamPayload {
    stream: TerminalOutputStream,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStreamErrorPayload {
    error: String,
    #[serde(default)]
    status: Option<u16>,
}

const REVIEW_DOCUMENT_VERSION: u32 = 1;

/// Represents the review document.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewDocument {
    version: u32,
    #[serde(default)]
    revision: u64,
    change_set_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<ReviewOrigin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<ReviewFileEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    threads: Vec<ReviewThread>,
}

/// Represents review origin.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewOrigin {
    session_id: String,
    message_id: String,
    agent: String,
    workdir: String,
    created_at: String,
}

/// Represents review file entry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewFileEntry {
    file_path: String,
    change_type: ChangeType,
}

/// Represents review thread.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThread {
    id: String,
    anchor: ReviewAnchor,
    status: ReviewThreadStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<ReviewThreadComment>,
}

/// Represents review thread comment.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewThreadComment {
    id: String,
    author: ReviewCommentAuthor,
    body: String,
    created_at: String,
    updated_at: String,
}

/// Defines the review anchor variants.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ReviewAnchor {
    ChangeSet,
    File {
        file_path: String,
    },
    Hunk {
        file_path: String,
        hunk_header: String,
    },
    Line {
        file_path: String,
        hunk_header: String,
        old_line: Option<usize>,
        new_line: Option<usize>,
    },
}

/// Defines the review comment author variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReviewCommentAuthor {
    User,
    Agent,
}

/// Enumerates review thread states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReviewThreadStatus {
    Open,
    Resolved,
    Applied,
    Dismissed,
}

/// Represents the review document response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewDocumentResponse {
    review_file_path: String,
    review: ReviewDocument,
}

/// Represents the review summary response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewSummaryResponse {
    change_set_id: String,
    review_file_path: String,
    thread_count: usize,
    open_thread_count: usize,
    resolved_thread_count: usize,
    comment_count: usize,
    has_threads: bool,
}

/// Summarizes review document.
#[derive(Default)]
struct ReviewDocumentSummary {
    thread_count: usize,
    open_thread_count: usize,
    resolved_thread_count: usize,
    comment_count: usize,
}

/// Represents the Git file action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitFileActionRequest {
    action: GitFileAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    path: String,
    #[serde(default)]
    status_code: Option<String>,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git commit request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitCommitRequest {
    message: String,
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// Represents the Git repo action request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitRepoActionRequest {
    workdir: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
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

/// Tracks Codex state.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate_limits: Option<CodexRateLimits>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<Session>,
    #[serde(default)]
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<StateResponse>,
}

/// Represents the create project response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProjectResponse {
    project_id: String,
    state: StateResponse,
}

/// Enumerates project digest actions.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDigestAction {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    requires_confirmation: bool,
}

/// Represents the project digest response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectDigestResponse {
    project_id: String,
    headline: String,
    done_summary: String,
    current_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    proposed_actions: Vec<ProjectDigestAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deep_link: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_message_ids: Vec<String>,
}

/// Represents project digest inputs.
#[derive(Clone)]
struct ProjectDigestInputs {
    project: Project,
    sessions: Vec<SessionRecord>,
}

/// Represents the project approval target.
#[derive(Clone)]
struct ProjectApprovalTarget {
    session_id: String,
    message_id: String,
}

/// Summarizes project digest.
struct ProjectDigestSummary {
    project_id: String,
    headline: String,
    done_summary: String,
    current_status: String,
    primary_session_id: Option<String>,
    proposed_actions: Vec<ProjectActionId>,
    deep_link: Option<String>,
    pending_approval_target: Option<ProjectApprovalTarget>,
    source_message_ids: Vec<String>,
}

impl ProjectDigestSummary {
    /// Converts the value into response.
    fn into_response(self) -> ProjectDigestResponse {
        ProjectDigestResponse {
            project_id: self.project_id,
            headline: self.headline,
            done_summary: self.done_summary,
            current_status: self.current_status,
            primary_session_id: self.primary_session_id,
            proposed_actions: self
                .proposed_actions
                .into_iter()
                .map(ProjectActionId::into_digest_action)
                .collect(),
            deep_link: self.deep_link,
            source_message_ids: self.source_message_ids,
        }
    }
}

/// Defines the project action ID variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectActionId {
    Approve,
    Reject,
    ReviewInTermal,
    FixIt,
    Stop,
    AskAgentToCommit,
    KeepIterating,
    Continue,
}

impl ProjectActionId {
    /// Handles parse.
    fn parse(value: &str) -> Result<Self, ApiError> {
        match value.trim() {
            "approve" => Ok(Self::Approve),
            "reject" => Ok(Self::Reject),
            "review-in-termal" => Ok(Self::ReviewInTermal),
            "fix-it" => Ok(Self::FixIt),
            "stop" => Ok(Self::Stop),
            "ask-agent-to-commit" => Ok(Self::AskAgentToCommit),
            "keep-iterating" => Ok(Self::KeepIterating),
            "continue" => Ok(Self::Continue),
            other => Err(ApiError::bad_request(format!(
                "unknown project action `{other}`"
            ))),
        }
    }

    /// Returns the str representation.
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::ReviewInTermal => "review-in-termal",
            Self::FixIt => "fix-it",
            Self::Stop => "stop",
            Self::AskAgentToCommit => "ask-agent-to-commit",
            Self::KeepIterating => "keep-iterating",
            Self::Continue => "continue",
        }
    }

    /// Handles label.
    fn label(self) -> &'static str {
        match self {
            Self::Approve => "Approve",
            Self::Reject => "Reject",
            Self::ReviewInTermal => "Review in TermAl",
            Self::FixIt => "Fix It",
            Self::Stop => "Stop",
            Self::AskAgentToCommit => "Ask Agent to Commit",
            Self::KeepIterating => "Keep Iterating",
            Self::Continue => "Continue",
        }
    }

    /// Handles prompt.
    fn prompt(self) -> Option<&'static str> {
        match self {
            Self::FixIt => Some(
                "The last run failed. Fix the issue, rerun the relevant verification, and summarize what changed.",
            ),
            Self::AskAgentToCommit => Some(
                "If the current changes are ready, create a git commit with a concise message and summarize the result.",
            ),
            Self::KeepIterating => Some(
                "Keep iterating on the current task and report back when the next review point is ready.",
            ),
            Self::Continue => Some(
                "Continue the work on this project and report back when the next review point is ready.",
            ),
            Self::Approve | Self::Reject | Self::ReviewInTermal | Self::Stop => None,
        }
    }

    /// Handles requires confirmation.
    fn requires_confirmation(self) -> bool {
        matches!(self, Self::Stop)
    }

    /// Converts the value into digest action.
    fn into_digest_action(self) -> ProjectDigestAction {
        ProjectDigestAction {
            id: self.as_str().to_owned(),
            label: self.label().to_owned(),
            prompt: self.prompt().map(str::to_owned),
            requires_confirmation: self.requires_confirmation(),
        }
    }
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

/// Builds project deep link.
fn build_project_deep_link(project_id: &str, session_id: Option<&str>) -> String {
    let mut query = format!("/?projectId={}", encode_uri_component(project_id));
    if let Some(session_id) = session_id {
        query.push_str("&sessionId=");
        query.push_str(&encode_uri_component(session_id));
    }
    query
}

/// Normalizes project text.
fn normalize_project_text(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        make_preview(trimmed)
    }
}

/// Returns the active project status text.
fn active_project_status_text(record: &SessionRecord) -> String {
    let queued_count = record.session.pending_prompts.len();
    match queued_count {
        0 => "Agent is working.".to_owned(),
        1 => "Agent is working with 1 queued follow-up.".to_owned(),
        count => format!("Agent is working with {count} queued follow-ups."),
    }
}

/// Handles select project done summary.
fn select_project_done_summary(
    primary_session: Option<&SessionRecord>,
    git_status: Option<&GitStatusResponse>,
    prefer_git: bool,
) -> (String, Vec<String>) {
    let message_summary = primary_session.and_then(latest_project_progress_summary);
    let git_summary = git_status.and_then(project_git_done_summary);
    if prefer_git {
        if let Some(summary) = git_summary.clone() {
            return (summary, Vec::new());
        }
    }
    if let Some((message_id, summary)) = message_summary {
        return (summary, vec![message_id]);
    }
    if let Some(summary) = git_summary {
        return (summary, Vec::new());
    }
    (
        primary_session
            .map(default_project_done_summary)
            .unwrap_or_else(|| "No agent work has started yet.".to_owned()),
        Vec::new(),
    )
}

/// Returns the default project done summary.
fn default_project_done_summary(record: &SessionRecord) -> String {
    if record.session.messages.is_empty() {
        return "Ready for the next prompt.".to_owned();
    }
    let preview = record.session.preview.trim();
    if preview.is_empty() {
        "Ready for the next prompt.".to_owned()
    } else {
        make_preview(preview)
    }
}

/// Handles project Git done summary.
fn project_git_done_summary(status: &GitStatusResponse) -> Option<String> {
    let changed_files = status.files.len();
    if changed_files == 0 {
        return None;
    }
    Some(match changed_files {
        1 => "Working tree has 1 changed file ready for review.".to_owned(),
        count => format!("Working tree has {count} changed files ready for review."),
    })
}

/// Returns the latest project progress summary.
fn latest_project_progress_summary(record: &SessionRecord) -> Option<(String, String)> {
    record.session.messages.iter().rev().find_map(|message| {
        project_progress_summary_for_message(message)
            .map(|summary| (message.id().to_owned(), summary))
    })
}

/// Handles project progress summary for message.
fn project_progress_summary_for_message(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            attachments,
            ..
        } => Some(prompt_preview_text(text, attachments)),
        Message::Thinking { title, .. } => Some(make_preview(title)),
        Message::Command {
            command, status, ..
        } => match status {
            CommandStatus::Running => None,
            CommandStatus::Success => Some(format!("Ran {} successfully.", make_preview(command))),
            CommandStatus::Error => Some(format!("Command failed: {}.", make_preview(command))),
        },
        Message::Diff { summary, .. } => Some(make_preview(summary)),
        Message::Markdown { title, .. } => Some(make_preview(title)),
        Message::SubagentResult { summary, title, .. } => {
            let detail = summary.trim();
            if detail.is_empty() {
                Some(make_preview(title))
            } else {
                Some(make_preview(detail))
            }
        }
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::FileChanges { .. } => None,
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You,
            ..
        } => None,
    }
}

/// Finds latest project pending approval.
fn find_latest_project_pending_approval<'a>(
    sessions: &'a [SessionRecord],
) -> Option<(&'a SessionRecord, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Approval { id, decision, .. }
                    if *decision == ApprovalDecision::Pending
                        && has_live_pending_approval(record, id) =>
                {
                    Some((record, id.clone()))
                }
                _ => None,
            })
    })
}

/// Returns whether live pending approval.
fn has_live_pending_approval(record: &SessionRecord, message_id: &str) -> bool {
    record.pending_claude_approvals.contains_key(message_id)
        || record.pending_codex_approvals.contains_key(message_id)
        || record.pending_acp_approvals.contains_key(message_id)
}

/// Finds latest project pending nonapproval interaction.
fn find_latest_project_pending_nonapproval_interaction<'a>(
    sessions: &'a [SessionRecord],
) -> Option<(&'a SessionRecord, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::UserInputRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                Message::McpElicitationRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                Message::CodexAppRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending =>
                {
                    Some((record, id.clone()))
                }
                _ => None,
            })
    })
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
        message: Message,
        preview: String,
        status: SessionStatus,
    },
    TextDelta {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    TextReplace {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<String>,
    },
    CommandUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        command: String,
        #[serde(rename = "commandLanguage", skip_serializing_if = "Option::is_none")]
        command_language: Option<String>,
        output: String,
        #[serde(rename = "outputLanguage", skip_serializing_if = "Option::is_none")]
        output_language: Option<String>,
        status: CommandStatus,
        preview: String,
    },
    ParallelAgentsUpdate {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        agents: Vec<ParallelAgentProgress>,
        preview: String,
    },
    OrchestratorsUpdated {
        revision: u64,
        orchestrators: Vec<OrchestratorInstance>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<Session>,
    },
}


