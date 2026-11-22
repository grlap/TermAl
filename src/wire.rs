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
// MessageCreated / CodexUpdated / OrchestratorsUpdated).
//
// Extracted from api.rs as the single largest extraction of the refactor.
// api.rs can now stay focused on axum HTTP route handlers + the
// `impl AppState` block.

use super::*;

/// In-process classifier metadata for `ApiError`.
///
/// Most kinds stay local. Typed boundaries, such as federation settings,
/// expose stable refusal codes beside `error`. Remote proxy
/// decoding reconstructs an `ApiError` from status plus `{ error }` (any such
/// code is lost), so any caller that needs typed recovery must attach the kind
/// locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiErrorKind {
    OutputLimitExceeded {
        limit: usize,
        outcome: ResponseMutationOutcome,
    },
    FederationMailboxUpgradeRequired,
    FederationMailboxWriterAdmission,
    FederationMailboxDirectoryFence,
    /// Local cause metadata; never reconstructed from remote error text.
    FederationMailboxCentralDurability,
    FederationMailboxReadExpired,
    FederationMailboxReadConflict,
    /// The initial read selected no page because the mailbox was not visible.
    FederationMailboxReadNotFound,
    /// A public domain mailbox 404 after the original native route missed.
    NativeMailboxRouteMiss,
    /// Retirement is observed, not inferred from a missing row. Only the
    /// initial capture boundary can certify that no page was selected.
    MailboxParticipantRetiredDuringRead {
        before_page: bool,
    },
    #[cfg_attr(not(unix), allow(dead_code))]
    GitDocumentBecameSymlink,
    GitDocumentInvalidUtf8,
    GitDocumentNotFile,
    GitDocumentNotFound,
    GitDocumentTooLarge,
    /// A local session lookup or remote-session-target resolution missed.
    LocalSessionMissing,
    /// A known launch lacks durable identity needed to admit a run wait.
    TestRunWaitRegistrationUnavailable,
    RemoteConnectionUnavailable,
    /// An owned federation SSH child could not be proven reaped. Never start
    /// another process for that pair until explicit operator reconciliation.
    FederationSshCleanupUnknown,
    /// A fresh evaluator's first brief was not acknowledged as durable, so its
    /// delegation was retired before any turn and the request may be repeated.
    AcceptanceEvaluationFirstBriefUnacknowledged,
    /// An evaluation was requested before any turn opened in its newly named
    /// source root: the root takes effect at the naming session's next turn,
    /// whose opening Engram sights, so a later turn may request it again.
    AcceptanceEvaluationNamedRootTurnPending,
    /// The named root has no current source-identity sighting at the judged
    /// cut. A retained revision-only sighting does not satisfy this precondition.
    AcceptanceEvaluationNamedRootSightingUnavailable,
    /// Federation operator settings refusals. The settings route alone puts
    /// their stable code beside `error` (`federation_operator_code`).
    FederationExpectedVersionRequired,
    FederationVersionConflict,
    FederationCentralStale,
    FederationCentralMismatch,
    FederationInvalidBinding,
    FederationBoundSessions,
    /// Retained exact receipt forbids recreating an explicitly retired queue.
    FederationExecutorAssociationRetired,
    /// The project's Engram store refused because it is in repair
    /// (`engram_store_transient`). Its stable code goes beside `error`, so a
    /// caller can resend the unchanged request once the store is ready.
    EngramStoreTransient,
    /// Source-owner metadata was refused because the project has no Engram
    /// binaryPath configured. Its stable code goes beside `error`, so the
    /// launcher can name that remedy for this refusal alone.
    EngramSourceOwnerUnconfigured,
}

impl ApiErrorKind {
    /// The stable code the federation settings route reports for one of its
    /// typed refusals; None for every other kind.
    pub(crate) fn federation_operator_code(self) -> Option<&'static str> {
        Some(match self {
            Self::FederationExpectedVersionRequired => "federation_expected_version_required",
            Self::FederationVersionConflict => "federation_version_conflict",
            Self::FederationCentralStale => "federation_central_stale",
            Self::FederationCentralMismatch => "federation_central_mismatch",
            Self::FederationInvalidBinding => "federation_invalid_binding",
            Self::FederationBoundSessions => "federation_bound_sessions",
            _ => return None,
        })
    }
}

/// Only an operation boundary may expose an enqueued fence's deadline as
/// pending. Shared authority/startup helpers retain the fact without changing
/// their ordinary error responses.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DurabilityPendingOperation {
    TestRunRegistration,
    AcceptanceEvaluationSubmission,
    NamedRootNaming,
    NamedRootRelease,
}

#[derive(Debug)]
pub(crate) struct EnqueuedDurabilityDeadline {
    pub(crate) identity: Value,
    pub(crate) operation: Option<DurabilityPendingOperation>,
    pub(crate) state_lock: Option<StateLockBlock>,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) message: String,
    pub(crate) status: StatusCode,
    // In-process classifier metadata. Only explicit typed boundaries expose
    // selected codes; remote proxy JSON decoding reconstructs `ApiError` with
    // `kind: None`; callers that need typed recovery must tag errors locally.
    pub(crate) kind: Option<ApiErrorKind>,
    pub(crate) durability_deadline: Option<EnqueuedDurabilityDeadline>,
}

impl ApiError {
    // Status-specific constructors — mirror the HTTP status codes they
    // wrap. Nothing interesting to document; the method name says it.
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_REQUEST,
            kind: None,
            durability_deadline: None,
        }
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::CONFLICT,
            kind: None,
            durability_deadline: None,
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::NOT_FOUND,
            kind: None,
            durability_deadline: None,
        }
    }

    pub(crate) fn local_session_missing() -> Self {
        Self::not_found("session not found").with_kind(ApiErrorKind::LocalSessionMissing)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: None,
            durability_deadline: None,
        }
    }

    pub(crate) fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status: StatusCode::BAD_GATEWAY,
            kind: None,
            durability_deadline: None,
        }
    }

    /// A 503 for a refusal that `engram_store_transient` classified: nothing
    /// was done, and the unchanged request may succeed once the store is ready.
    pub(crate) fn engram_store_transient(message: impl Into<String>) -> Self {
        Self::from_status(StatusCode::SERVICE_UNAVAILABLE, message)
            .with_kind(ApiErrorKind::EngramStoreTransient)
    }

    /// Builds the value from status.
    pub(crate) fn from_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status,
            kind: None,
            durability_deadline: None,
        }
    }

    /// Tags error handling; selected kinds also have explicit wire contracts.
    pub(crate) fn with_kind(mut self, kind: ApiErrorKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Call only for an exact Deadline after successfully sending its fence.
    /// Neither a failed send nor another fence error establishes this fact.
    pub(crate) fn enqueued_durability_deadline(
        message: impl Into<String>,
        identity: Value,
    ) -> Self {
        let mut error = Self::internal(message);
        error.durability_deadline = Some(EnqueuedDurabilityDeadline {
            identity,
            operation: None,
            state_lock: None,
        });
        error
    }

    pub(crate) fn is_enqueued_durability_deadline(&self) -> bool {
        self.durability_deadline.is_some()
    }

    pub(crate) fn at_durability_boundary(mut self, operation: DurabilityPendingOperation) -> Self {
        if let Some(deadline) = &mut self.durability_deadline {
            deadline.operation = Some(operation);
            self.status = StatusCode::SERVICE_UNAVAILABLE;
            self.message = format!(
                "{}; the operation may still commit; retry the unchanged original request",
                truncate_chars(&self.message, 800),
            );
        }
        self
    }
}

impl IntoResponse for ApiError {
    /// Converts the value into response.
    fn into_response(self) -> Response {
        if let Some(ApiErrorKind::OutputLimitExceeded { limit, outcome }) = self.kind {
            return OutputLimitResponse { limit, outcome }.response();
        }
        if matches!(self.kind, Some(ApiErrorKind::LocalSessionMissing)) {
            // Remote HTTP errors have no local kind, even when their status is
            // 404. Only absence from this server's lookup can emit this code.
            return (
                self.status,
                Json(json!({
                    "error": self.message,
                    "code": "local_session_missing",
                })),
            )
                .into_response();
        }
        if matches!(self.kind, Some(ApiErrorKind::EngramStoreTransient)) {
            return (
                self.status,
                Json(json!({
                    "error": self.message,
                    "code": ENGRAM_STORE_TRANSIENT_API_CODE,
                })),
            )
                .into_response();
        }
        if self.kind == Some(ApiErrorKind::EngramSourceOwnerUnconfigured) {
            return (
                self.status,
                Json(json!({
                    "error": self.message,
                    "code": "engram_source_owner_unconfigured",
                })),
            )
                .into_response();
        }
        if let Some(ApiErrorKind::MailboxParticipantRetiredDuringRead { before_page }) = self.kind {
            return (
                self.status,
                Json(json!({
                    "error": self.message,
                    "code": "participant_retired_during_read",
                    "effect": if before_page { "notEntered" } else { "unknown" },
                })),
            )
                .into_response();
        }
        if self.kind == Some(ApiErrorKind::NativeMailboxRouteMiss) {
            return (
                self.status,
                Json(json!({ "error": self.message, "code": "native_mailbox_route_miss" })),
            )
                .into_response();
        }
        if let Some(deadline) = self.durability_deadline
            && let Some(operation) = deadline.operation
        {
            let mut body = json!({
                "error": truncate_chars(&self.message, 2048),
                "contract": "termal-durability-pending",
                "version": 1,
                "code": "durability_pending",
                "outcome": "pending",
                "operation": operation,
                "identity": deadline.identity,
                "retry": { "mode": "same_request" },
            });
            if matches!(operation, DurabilityPendingOperation::TestRunRegistration)
                && let Some(block) = deadline.state_lock
            {
                body["stateLock"] = serde_json::to_value(block).unwrap_or(Value::Null);
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        }
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}

/// Defines the agent variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) enum Agent {
    Codex,
    Claude,
    Cursor,
    Gemini,
    OpenCode,
    Kimi,
}

impl Agent {
    /// Parses the CLI-arg iterator into an `Agent`. Accepts an
    /// `--agent <value>` pair or a standalone agent name; unknown
    /// arguments fail fast. Defaults to `Agent::Codex` if no arg
    /// is given.
    pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
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
                "opencode" | "open-code" => return Ok(Self::OpenCode),
                "kimi" | "kimi-code" => return Ok(Self::Kimi),
                other => bail!("unknown argument `{other}`"),
            }
        }

        Ok(Self::Codex)
    }

    /// Builds the value from str.
    pub(crate) fn from_str(value: &str) -> Result<Self> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "gemini" | "gemini-cli" => Ok(Self::Gemini),
            "opencode" | "open-code" => Ok(Self::OpenCode),
            "kimi" | "kimi-code" => Ok(Self::Kimi),
            other => {
                bail!(
                    "unknown agent `{other}`; expected `codex`, `claude`, `cursor`, `gemini`, `opencode`, or `kimi`"
                )
            }
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Gemini => "Gemini",
            Self::OpenCode => "OpenCode",
            Self::Kimi => "Kimi",
        }
    }

    pub(crate) fn avatar(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Cursor => "CR",
            Self::Gemini => "GM",
            Self::OpenCode => "OC",
            Self::Kimi => "KM",
        }
    }

    /// Returns the default model.
    pub(crate) fn default_model(self) -> &'static str {
        match self {
            Self::Codex => "gpt-5.4",
            Self::Claude => "default",
            Self::Cursor | Self::Gemini | Self::OpenCode | Self::Kimi => "auto",
        }
    }

    /// Returns whether Codex prompt settings.
    pub(crate) fn supports_codex_prompt_settings(self) -> bool {
        matches!(self, Self::Codex)
    }

    /// Returns whether Claude approval mode.
    pub(crate) fn supports_claude_approval_mode(self) -> bool {
        matches!(self, Self::Claude)
    }

    /// Returns whether cursor mode.
    pub(crate) fn supports_cursor_mode(self) -> bool {
        matches!(self, Self::Cursor)
    }

    /// Returns whether Gemini approval mode.
    pub(crate) fn supports_gemini_approval_mode(self) -> bool {
        matches!(self, Self::Gemini)
    }

    /// Returns whether dynamic OpenCode model/mode settings are supported.
    pub(crate) fn supports_opencode_settings(self) -> bool {
        matches!(self, Self::OpenCode)
    }

    /// Handles ACP runtime.
    pub(crate) fn acp_runtime(self) -> Option<AcpAgent> {
        match self {
            Self::Cursor => Some(AcpAgent::Cursor),
            Self::Gemini => Some(AcpAgent::Gemini),
            Self::OpenCode => Some(AcpAgent::OpenCode),
            Self::Kimi => Some(AcpAgent::Kimi),
            _ => None,
        }
    }

    /// Returns whether the adapter can authenticate the MCP tool that submits
    /// the authoritative result for a delegated reviewer. Kimi can: its ACP
    /// tool calls carry the server-qualified `mcp__<server>__<tool>` name and
    /// the host answers every permission request of a read-only Kimi child
    /// (docs/features/kimi-cli-integration.md, "Read-only delegation children").
    pub(crate) fn supports_structured_review_results(self) -> bool {
        matches!(self, Self::Codex | Self::Claude | Self::Kimi)
    }

    /// Returns whether the adapter may run an acceptance evaluator. Evaluators
    /// stay Claude or Codex; Kimi's read-only gate covers reviewers only.
    pub(crate) fn supports_acceptance_evaluations(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }
}

/// Represents project.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Project {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root_path: String,
    pub(crate) remote_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_project_id: Option<String>,
    /// Host-local project settings. Client projections remove the values.
    #[serde(default, skip_serializing_if = "ProjectEnvironment::is_empty")]
    pub(crate) environment: ProjectEnvironment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) engram: Option<EngramProjectSettings>,
    /// Durable, redacted operator warning for Engram cleanup that completed
    /// only partially. Stored on the project so the condition remains visible
    /// even when the project has no local sessions to receive a transcript
    /// notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) engram_cleanup_warning: Option<String>,
}

/// Serializes projects with repository-declaration and operator-veto status
/// derived at snapshot time, while hiding host-private Engram store identity.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientProjectSnapshot {
    #[serde(flatten)]
    project: Project,
    engram_declared: bool,
    engram_operator_disabled: bool,
}

pub(crate) fn serialize_client_projects_with_engram_status<S>(
    projects: &[Project],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let sanitized = projects
        .iter()
        .cloned()
        .map(|mut project| {
            project.environment = ProjectEnvironment::default();
            let engram_declared = project.remote_id == LOCAL_REMOTE_ID
                && fs::metadata(FsPath::new(&project.root_path).join(".engram-project"))
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);
            let engram_operator_disabled = engram_declared
                && project
                    .engram
                    .as_ref()
                    .is_some_and(|settings| !settings.enabled);
            if let Some(settings) = project.engram.as_mut() {
                settings.authority_store_key = None;
            }
            ClientProjectSnapshot {
                project,
                engram_declared,
                engram_operator_disabled,
            }
        })
        .collect::<Vec<_>>();
    sanitized.serialize(serializer)
}

/// Enumerates conversation marker categories.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ConversationMarkerKind {
    Checkpoint,
    Decision,
    Review,
    Bug,
    Question,
    Handoff,
    #[serde(other)]
    Custom,
}

/// Enumerates conversation marker authors.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConversationMarkerAuthor {
    User,
    Agent,
    System,
}

/// Represents a user or system marker anchored to a conversation message.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationMarker {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) kind: ConversationMarkerKind,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<String>,
    pub(crate) color: String,
    pub(crate) message_id: String,
    pub(crate) message_index_hint: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end_message_index_hint: Option<usize>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) created_by: ConversationMarkerAuthor,
}

fn parse_conversation_marker_kind_request<E>(value: &str) -> Result<ConversationMarkerKind, E>
where
    E: serde::de::Error,
{
    match value {
        "checkpoint" => Ok(ConversationMarkerKind::Checkpoint),
        "decision" => Ok(ConversationMarkerKind::Decision),
        "review" => Ok(ConversationMarkerKind::Review),
        "bug" => Ok(ConversationMarkerKind::Bug),
        "question" => Ok(ConversationMarkerKind::Question),
        "handoff" => Ok(ConversationMarkerKind::Handoff),
        "custom" => Ok(ConversationMarkerKind::Custom),
        _ => Err(E::custom(format!(
            "unsupported conversation marker kind `{value}`"
        ))),
    }
}

fn deserialize_conversation_marker_kind_request<'de, D>(
    deserializer: D,
) -> Result<ConversationMarkerKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    parse_conversation_marker_kind_request(&value)
}

fn deserialize_optional_conversation_marker_kind_request<'de, D>(
    deserializer: D,
) -> Result<Option<ConversationMarkerKind>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = <Option<String> as serde::Deserialize>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_conversation_marker_kind_request(&value).map(Some)
}

fn deserialize_nullable_marker_field<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

/// Request to create a conversation marker.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateConversationMarkerRequest {
    #[serde(deserialize_with = "deserialize_conversation_marker_kind_request")]
    pub(crate) kind: ConversationMarkerKind,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body: Option<String>,
    pub(crate) color: String,
    pub(crate) message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end_message_id: Option<String>,
}

/// Request to patch a conversation marker.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateConversationMarkerRequest {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_conversation_marker_kind_request",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) kind: Option<ConversationMarkerKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_marker_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) body: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_marker_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) end_message_id: Option<Option<String>>,
}

fn deserialize_nonempty_server_instance_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom(
            "serverInstanceId must be a non-empty string",
        ));
    }
    Ok(value)
}

/// Response containing all conversation markers for one session.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationMarkersResponse {
    pub(crate) markers: Vec<ConversationMarker>,
    pub(crate) revision: u64,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Response containing one conversation marker mutation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationMarkerResponse {
    pub(crate) marker: ConversationMarker,
    pub(crate) revision: u64,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_mutation_stamp: Option<u64>,
}

/// Response containing one deleted conversation marker mutation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeleteConversationMarkerResponse {
    pub(crate) marker_id: String,
    pub(crate) revision: u64,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_mutation_stamp: Option<u64>,
}

/// Represents session.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq_epoch: Option<String>,
    /// Requested Kimi thinking value; None leaves the CLI's current choice alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_current_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) kimi_effort_options: Vec<SessionModelOption>,
    /// TermAl's Kimi permission policy; None reads as `ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_approval_mode: Option<KimiApprovalMode>,
    /// Kimi's own mode for this session; None reads as `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_mode: Option<KimiMode>,
    /// The mode Kimi last reported, for display only; never `kimi_mode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_current_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_approval_mode: Option<OpenCodeApprovalMode>,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) emoji: String,
    pub(crate) agent: Agent,
    pub(crate) workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_id: Option<String>,
    pub(crate) model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) model_options: Vec<SessionModelOption>,
    pub(crate) approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<CodexReasoningEffort>,
    /// Session-scoped Codex Fast-mode authority. The app-server represents
    /// this as the active model's catalog-advertised Fast service-tier id.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) codex_fast_mode: bool,
    pub(crate) sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_mode: Option<CursorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claude_effort: Option<ClaudeEffortLevel>,
    pub(crate) claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gemini_approval_mode: Option<GeminiApprovalMode>,
    /// Persisted OpenCode model selection. `auto` means the agent owns the
    /// effective model, while any explicit provider/model id is reapplied
    /// after session new/resume/load before the next prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_model: Option<String>,
    /// Persisted OpenCode model-variant authority (`auto` or an explicit
    /// model-specific ACP `effort` value). None leaves the consumer on Auto;
    /// loading persisted state must not rewrite it into an explicit selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_effort: Option<String>,
    /// Effective live OpenCode reasoning variant reported by ACP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_current_effort: Option<String>,
    /// Dynamic model-specific OpenCode reasoning variants reported by ACP.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) opencode_effort_options: Vec<SessionModelOption>,
    /// Persisted OpenCode primary-agent selection (`auto` or a live dynamic
    /// mode value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_mode: Option<String>,
    /// Effective live OpenCode primary agent reported by ACP configOptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_current_mode: Option<String>,
    /// Dynamic OpenCode primary-agent options reported by ACP.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) opencode_mode_options: Vec<SessionModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_session_id: Option<String>,
    #[serde(default)]
    pub(crate) agent_commands_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) codex_thread_state: Option<CodexThreadState>,
    /// Explicit current-turn state. UI activity must not depend on which
    /// bounded transcript page happens to be resident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) live_activity: Option<SessionLiveActivity>,
    pub(crate) status: SessionStatus,
    pub(crate) preview: String,
    pub(crate) messages: MessageSequence,
    /// Recent user-authored prompts, ordered oldest to newest.
    ///
    /// This is deliberately independent of the bounded transcript window so
    /// composer Up/Down history remains available after summary-only state
    /// loads and targeted tail hydration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) prompt_history: Vec<String>,
    /// True only when a metadata projection intentionally omits prompt history.
    /// Targeted/full projections leave this false, so an omitted empty array is
    /// distinguishable from redaction on both local and remote clients.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) prompt_history_redacted: bool,
    #[serde(default = "session_messages_loaded_default", rename = "messagesLoaded")]
    pub(crate) messages_loaded: bool,
    #[serde(default, rename = "messageCount")]
    pub(crate) message_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) markers: Vec<ConversationMarker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) pending_prompts: Vec<PendingPrompt>,
    /// Delivery receipts of the session's receipted prompts, oldest first:
    /// pending, handing-off, accepted, rejected or ambiguous
    /// (`prompt_delivery.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) prompt_deliveries: Vec<PromptDeliveryView>,
    /// Actual provider-channel enqueues observed by this host process.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) provider_handoffs: Vec<ProviderHandoffView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_handoff_history: Option<ProviderHandoffHistoryView>,
    /// True while queued prompts are parked behind the explicit-resume latch:
    /// a user Stop (or a failed stop persistence) keeps user and mailbox
    /// prompts durable but refuses to auto-dispatch them until the user sends
    /// a new prompt or resumes the queue. Always serialized so the UI can
    /// distinguish "paused, waiting for the user" from "about to start".
    #[serde(default)]
    pub(crate) queue_paused: bool,
    #[serde(default)]
    pub(crate) provider_capacity_waiting: bool,
    /// True while an actual pending deletion retains observation records.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) source_tracking_recovery: bool,
    /// Opaque identity/disposition fingerprint for the authoritative queued
    /// prompt projection. Broad summaries carry this without prompt bodies so
    /// clients can target hydration only when the queue actually changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) queue_projection_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_mutation_stamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_delegation_id: Option<String>,
}
impl Session {
    pub(crate) fn clone_without_messages(&self) -> Self {
        Self {
            body_seq: self.body_seq.clone(),
            body_seq_epoch: self.body_seq_epoch.clone(),
            kimi_effort: self.kimi_effort.clone(),
            kimi_current_effort: self.kimi_current_effort.clone(),
            kimi_effort_options: self.kimi_effort_options.clone(),
            kimi_approval_mode: self.kimi_approval_mode.clone(),
            kimi_mode: self.kimi_mode.clone(),
            kimi_current_mode: self.kimi_current_mode.clone(),
            opencode_approval_mode: self.opencode_approval_mode.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            emoji: self.emoji.clone(),
            agent: self.agent.clone(),
            workdir: self.workdir.clone(),
            project_id: self.project_id.clone(),
            remote_id: self.remote_id.clone(),
            model: self.model.clone(),
            model_options: self.model_options.clone(),
            approval_policy: self.approval_policy.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            codex_fast_mode: self.codex_fast_mode.clone(),
            sandbox_mode: self.sandbox_mode.clone(),
            cursor_mode: self.cursor_mode.clone(),
            claude_effort: self.claude_effort.clone(),
            claude_approval_mode: self.claude_approval_mode.clone(),
            gemini_approval_mode: self.gemini_approval_mode.clone(),
            opencode_model: self.opencode_model.clone(),
            opencode_effort: self.opencode_effort.clone(),
            opencode_current_effort: self.opencode_current_effort.clone(),
            opencode_effort_options: self.opencode_effort_options.clone(),
            opencode_mode: self.opencode_mode.clone(),
            opencode_current_mode: self.opencode_current_mode.clone(),
            opencode_mode_options: self.opencode_mode_options.clone(),
            external_session_id: self.external_session_id.clone(),
            agent_commands_revision: self.agent_commands_revision.clone(),
            codex_thread_state: self.codex_thread_state.clone(),
            live_activity: self.live_activity.clone(),
            status: self.status.clone(),
            preview: self.preview.clone(),
            messages: MessageSequence::default(),
            prompt_history: self.prompt_history.clone(),
            prompt_history_redacted: self.prompt_history_redacted.clone(),
            messages_loaded: self.messages_loaded.clone(),
            message_count: self.message_count.clone(),
            markers: self.markers.clone(),
            pending_prompts: self.pending_prompts.clone(),
            prompt_deliveries: self.prompt_deliveries.clone(),
            provider_handoffs: self.provider_handoffs.clone(),
            provider_handoff_history: self.provider_handoff_history.clone(),
            queue_paused: self.queue_paused.clone(),
            provider_capacity_waiting: self.provider_capacity_waiting.clone(),
            source_tracking_recovery: self.source_tracking_recovery.clone(),
            queue_projection_hash: self.queue_projection_hash.clone(),
            session_mutation_stamp: self.session_mutation_stamp.clone(),
            parent_delegation_id: self.parent_delegation_id.clone(),
        }
    }
}

/// Transcript-free session metadata carried by broad state snapshots and
/// summary lifecycle deltas. Full messages, prompt history, and queued prompt
/// bodies are available only through targeted session endpoints.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StateSessionSummary {
    #[serde(
        default,
        skip_serializing_if = "SessionTestRunActivitySnapshot::is_empty"
    )]
    pub(crate) test_run_activity: SessionTestRunActivitySnapshot,
    /// Display/routing hint only; every domain operation validates its binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mailbox_participant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq_epoch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_current_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) kimi_effort_options: Vec<SessionModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_approval_mode: Option<KimiApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_mode: Option<KimiMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) kimi_current_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_approval_mode: Option<OpenCodeApprovalMode>,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) emoji: String,
    pub(crate) agent: Agent,
    pub(crate) workdir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_id: Option<String>,
    pub(crate) model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) model_options: Vec<SessionModelOption>,
    pub(crate) approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) codex_fast_mode: bool,
    pub(crate) sandbox_mode: Option<CodexSandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cursor_mode: Option<CursorMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claude_effort: Option<ClaudeEffortLevel>,
    pub(crate) claude_approval_mode: Option<ClaudeApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gemini_approval_mode: Option<GeminiApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_current_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) opencode_effort_options: Vec<SessionModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) opencode_current_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) opencode_mode_options: Vec<SessionModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external_session_id: Option<String>,
    #[serde(default)]
    pub(crate) agent_commands_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) codex_thread_state: Option<CodexThreadState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) live_activity: Option<SessionLiveActivity>,
    pub(crate) status: SessionStatus,
    pub(crate) preview: String,
    #[serde(rename = "messageCount")]
    pub(crate) message_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) markers: Vec<ConversationMarker>,
    pub(crate) queue_paused: bool,
    #[serde(default)]
    pub(crate) provider_capacity_waiting: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) source_tracking_recovery: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) queue_projection_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_mutation_stamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_delegation_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionLiveActivity {
    pub(crate) prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command_status: Option<CommandStatus>,
}

fn session_messages_loaded_default() -> bool {
    true
}

/// Tracks Codex thread state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CodexThreadState {
    Active,
    Archived,
}

/// Defines the Codex approval policy variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CodexApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    /// Host policy, independent of native Codex approval prompting. Routine
    /// tool approvals are automatic; genuine input remains interactive.
    AutoApprove,
    Never,
}

impl CodexApprovalPolicy {
    /// Returns the cli value representation.
    pub(crate) fn as_cli_value(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnFailure => "on-failure",
            Self::OnRequest => "on-request",
            // AutoApprove is a TermAl policy. Full access keeps native asks
            // for host answers. The app-server request builder selects a native
            // granular policy when the current command has a restricted cap.
            Self::AutoApprove => "on-request",
            Self::Never => "never",
        }
    }
}

/// Enumerates Codex sandbox modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    /// Returns the cli value representation.
    pub(crate) fn as_cli_value(self) -> &'static str {
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
pub(crate) enum CodexReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
    Ultra,
}

impl CodexReasoningEffort {
    /// Returns the API value representation.
    pub(crate) fn as_api_value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
}

/// Enumerates Claude approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ClaudeApprovalMode {
    Ask,
    AutoApprove,
    Plan,
    ReadOnlyAutoApprove,
}

/// Defines the Claude effort level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ClaudeEffortLevel {
    Default,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl ClaudeEffortLevel {
    /// Returns the cli value representation.
    pub(crate) fn as_cli_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }
}

impl ClaudeApprovalMode {
    /// Returns the `--permission-mode` flag value to pass to the
    /// Claude CLI at spawn time, or `None` to let the CLI choose its
    /// default. Read-only delegates explicitly select default so inherited
    /// acceptEdits/bypass/auto modes cannot replace host permission admission.
    /// Auto-approve selects default too, the mode a runtime switch selects,
    /// so an inherited mode such as dontAsk cannot refuse a tool before the
    /// CLI asks TermAl, which grants it. Ask keeps the user's own mode.
    pub(crate) fn initial_cli_permission_mode(self) -> Option<&'static str> {
        match self {
            Self::Plan => Some("plan"),
            Self::ReadOnlyAutoApprove | Self::AutoApprove => Some("default"),
            Self::Ask => None,
        }
    }

    /// Returns the `permission_mode` value sent over the Claude
    /// session's NDJSON control channel when the user flips modes
    /// at runtime (vs. at launch). Differs from
    /// `initial_cli_permission_mode` because the runtime channel
    /// uses the string "default" in place of `None`.
    pub(crate) fn session_cli_permission_mode(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Ask | Self::AutoApprove | Self::ReadOnlyAutoApprove => "default",
        }
    }
}

/// Enumerates cursor modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CursorMode {
    Agent,
    Plan,
    Ask,
}

impl CursorMode {
    /// Returns the ACP value representation.
    pub(crate) fn as_acp_value(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

/// TermAl-owned OpenCode permission policy, independent of dynamic ACP modes.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OpenCodeApprovalMode {
    #[default]
    Ask,
    AutoApprove,
}

/// TermAl-owned Kimi permission policy: the same `ask` / `auto-approve` wire
/// values as OpenCode's, and the same meaning. Independent of `KimiMode`,
/// which is Kimi's own setting (docs/features/kimi-cli-integration.md).
pub(crate) type KimiApprovalMode = OpenCodeApprovalMode;

/// Kimi's own mode, set before every prompt and acknowledged by Kimi.
/// `default` asks before commands and edits; `plan` plans only; in `yolo`
/// and `auto` Kimi decides for itself and rarely or never asks TermAl.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum KimiMode {
    #[default]
    Default,
    Plan,
    Yolo,
    Auto,
}

impl KimiMode {
    /// The value Kimi's `mode` config option uses.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::Yolo => "yolo",
            Self::Auto => "auto",
        }
    }
}

/// Enumerates Gemini approval modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GeminiApprovalMode {
    Default,
    AutoEdit,
    Yolo,
    Plan,
}

/// Enumerates session states.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SessionStatus {
    Active,
    Idle,
    Approval,
    Stopping,
    Error,
}

/// Defines the author variants.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Author {
    You,
    Assistant,
    /// Written by TermAl itself, not by the user or the agent: the test-run
    /// card (`test_run_cards.rs`).
    System,
}

/// Enumerates command states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CommandStatus {
    Running,
    Success,
    Error,
}

impl CommandStatus {
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) enum ChangeType {
    Edit,
    Create,
}

/// Enumerates approval decisions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ApprovalDecision {
    Pending,
    Interrupted,
    Canceled,
    Accepted,
    AcceptedForSession,
    Rejected,
}

/// Represents the approval request payload.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApprovalRequest {
    pub(crate) decision: ApprovalDecision,
}

/// Represents the user input submission request payload. `declined: true`
/// skips the questions instead of answering them (`answers` must then be
/// empty or omitted entirely); only declinable requests accept it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserInputSubmissionRequest {
    #[serde(default)]
    pub(crate) answers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub(crate) declined: bool,
}

/// Represents the MCP elicitation submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpElicitationSubmissionRequest {
    pub(crate) action: McpElicitationAction,
    #[serde(default)]
    pub(crate) content: Option<Value>,
}

/// Represents the Codex app request submission request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexAppRequestSubmissionRequest {
    pub(crate) result: Value,
}

/// Represents the create session request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateSessionRequest {
    #[serde(default)]
    pub(crate) kimi_approval_mode: Option<KimiApprovalMode>,
    #[serde(default)]
    pub(crate) kimi_mode: Option<KimiMode>,
    pub(crate) opencode_approval_mode: Option<OpenCodeApprovalMode>,
    pub(crate) agent: Option<Agent>,
    pub(crate) name: Option<String>,
    pub(crate) workdir: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) approval_policy: Option<CodexApprovalPolicy>,
    pub(crate) reasoning_effort: Option<CodexReasoningEffort>,
    pub(crate) sandbox_mode: Option<CodexSandboxMode>,
    pub(crate) cursor_mode: Option<CursorMode>,
    pub(crate) claude_approval_mode: Option<ClaudeApprovalMode>,
    pub(crate) claude_effort: Option<ClaudeEffortLevel>,
    pub(crate) gemini_approval_mode: Option<GeminiApprovalMode>,
}

/// Delegated child session mode.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DelegationMode {
    Reviewer,
    Explorer,
    Worker,
    /// Host-created acceptance evaluator. Only the acceptance-evaluation
    /// request path creates one; the public create route refuses the mode.
    Evaluator,
}

/// Delegated child session lifecycle status.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DelegationStatus {
    Queued,
    Running,
    /// The current attempt is non-terminal but its turn is withheld (an
    /// Engram admission deferred, unconfirmed, interrupted or stopped): no
    /// provider execution is under way and none completed. A stored record
    /// keeps `running` while it holds; clients are told `held` (see
    /// `DelegationAttemptState`).
    Held,
    Completed,
    Failed,
    Canceled,
}

/// Why a delegation's current attempt is held.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationHoldReason {
    /// Engram deferred the admission, or its outcome was parked before any
    /// handoff; the retained prompt was never delivered. Resume retries it.
    AdmissionDeferred,
    /// The host withheld a delivery it proved unsent but could not yet
    /// confirm its own record of that is durable. It retries automatically
    /// once the record is confirmed.
    PersistenceUnknown,
    /// Proven unsent and durably recorded; an automatic retry is scheduled.
    RetryScheduled,
    /// The authorization was interrupted and the host cannot prove the
    /// prompt was not delivered. It will not resend it: cancel it, or
    /// reconcile with Engram first.
    DeliveryUnknown,
    /// The user stopped the authorization. The prompt is retained until it
    /// is canceled.
    Stopped,
}

/// What may be done to a held attempt, offered only when it is safe.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationHoldAction {
    /// Retry the retained prompt now (`termal_resume_session`).
    Resume,
    /// Cancel the delegation (`termal_cancel_session`).
    Cancel,
}

/// A held attempt as clients see it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationHold {
    pub(crate) reason: DelegationHoldReason,
    /// When the attempt's retained prompt was first held (RFC 3339).
    pub(crate) held_since: String,
    /// When the held attempt last changed (it entered the hold, its reason
    /// changed, or a retry was attempted), RFC 3339. Status reads never move
    /// it.
    pub(crate) last_activity_at: String,
    /// Increases each time the attempt enters a hold or its hold changes, so
    /// a notification names the exact hold it reported.
    pub(crate) generation: u64,
    pub(crate) retry_eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_retry_at: Option<String>,
    #[serde(default)]
    pub(crate) actions: Vec<DelegationHoldAction>,
    /// The child's own guidance line for the held prompt.
    #[serde(default)]
    pub(crate) detail: String,
    /// The retained queued prompt of the child the hold is for.
    #[serde(default)]
    pub(crate) prompt_id: String,
    /// Delivery attempts the host aborted for that prompt so far.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub(crate) attempts: u32,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Presentation of a stored `running` attempt that has not reached provider
/// execution. The stored status stays `running`, so the ownership gates of
/// the current attempt are unchanged; every client projection reports the
/// status `public_delegation_status` derives from this state instead.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationAttemptState {
    // Historical field name retained on disk and wire; also records evaluator
    // readiness, using the same exact-thread observation owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) reviewer_mcp_observations: Vec<CodexReviewerMcpObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hold: Option<DelegationHold>,
    /// The current attempt's prompt is admitting or queued behind a child
    /// that is not executing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) pending_start: bool,
    /// The last hold generation used; kept after a hold clears so the next
    /// hold of this delegation never reuses a generation.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub(crate) hold_generation: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// The status clients see for a delegation.
pub(crate) fn public_delegation_status(record: &DelegationRecord) -> DelegationStatus {
    if record.status != DelegationStatus::Running {
        return record.status;
    }
    if record.attempt.hold.is_some() {
        DelegationStatus::Held
    } else if record.attempt.pending_start {
        DelegationStatus::Queued
    } else {
        DelegationStatus::Running
    }
}

/// How the first turn of a created delegation (or a follow-up's turn) was
/// left when the create call returned.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationTurnDeliveryState {
    Delivered,
    Scheduled,
    Queued,
    Held,
    /// A newer owner took the turn over; read the delegation's current state.
    Superseded,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationTurnDelivery {
    pub(crate) state: DelegationTurnDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hold: Option<DelegationHold>,
    /// Why delivery was held when the host reported an uncertainty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

/// Delegated child write policy. Workers require `IsolatedWorktree`;
/// `SharedWorktree` remains deferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DelegationWritePolicy {
    ReadOnly,
    SharedWorktree {
        owned_paths: Vec<String>,
    },
    IsolatedWorktree {
        owned_paths: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        worktree_path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        base: Option<String>,
    },
}

impl<'de> serde::Deserialize<'de> for DelegationWritePolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Serde's internally-tagged unit variants accept otherwise unknown
        // sibling fields even under `deny_unknown_fields`. Decode through an
        // empty struct variant so `{ "kind": "readOnly", ... }` is held to
        // the same exact-shape contract as the variants with payload fields.
        #[derive(Deserialize)]
        #[serde(
            tag = "kind",
            rename_all = "camelCase",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum WirePolicy {
            ReadOnly {},
            SharedWorktree {
                owned_paths: Vec<String>,
            },
            IsolatedWorktree {
                owned_paths: Vec<String>,
                worktree_path: Option<String>,
                base: Option<String>,
            },
        }

        Ok(match WirePolicy::deserialize(deserializer)? {
            WirePolicy::ReadOnly {} => Self::ReadOnly,
            WirePolicy::SharedWorktree { owned_paths } => Self::SharedWorktree { owned_paths },
            WirePolicy::IsolatedWorktree {
                owned_paths,
                worktree_path,
                base,
            } => Self::IsolatedWorktree {
                owned_paths,
                worktree_path,
                base,
            },
        })
    }
}

/// Compact result packet for a completed delegation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResult {
    /// Host-recorded worker source metadata, not a child claim or diff snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) worktree_path: Option<String>,
    /// Host-owned Worker transcript attribution. True means there is no
    /// attributable current-attempt transcript; absent on legacy results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) worker_output_unavailable: Option<bool>,
    // Same historical field name as DelegationAttemptState, for both modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) reviewer_mcp_observations: Vec<CodexReviewerMcpObservation>,
    pub(crate) delegation_id: String,
    pub(crate) child_session_id: String,
    pub(crate) status: DelegationStatus,
    pub(crate) summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) findings: Vec<DelegationFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) changed_files: Vec<String>,
    /// Watcher observations during the run, never attribution to the child.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) observed_workspace_changes: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub(crate) observed_workspace_changes_omitted: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) files_inspected: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) commands_run: Vec<DelegationCommandResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) notes: Vec<String>,
}

/// Strict result envelope sent by a delegated reviewer through its durable
/// mailbox. Markdown remains available as full child output, but it is not a
/// protocol: new review workflows use this validated envelope for compact
/// cards and parent fan-in.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DelegationReviewMailboxResult {
    pub(crate) schema_version: u32,
    pub(crate) kind: String,
    pub(crate) delegation_id: String,
    pub(crate) child_session_id: String,
    pub(crate) submission_attempt: u32,
    pub(crate) status: DelegationStatus,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) findings: Vec<DelegationFinding>,
    #[serde(default)]
    pub(crate) commands_run: Vec<DelegationCommandResult>,
    #[serde(default)]
    pub(crate) files_inspected: Vec<String>,
    #[serde(default)]
    pub(crate) notes: Vec<String>,
    #[serde(default)]
    pub(crate) suggested_tracker_updates: Vec<String>,
}

/// Model-facing request accepted by `termal_submit_review_result`. Identity,
/// routing, topic, and idempotency metadata are derived by TermAl so a review
/// child can submit only its own result to its own coordinator.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubmitDelegationReviewResultRequest {
    pub(crate) schema_version: u32,
    pub(crate) status: DelegationStatus,
    pub(crate) summary: String,
    pub(crate) findings: Vec<SubmitDelegationReviewFinding>,
    pub(crate) commands_run: Vec<SubmitDelegationReviewCommand>,
    pub(crate) files_inspected: Vec<String>,
    pub(crate) notes: Vec<String>,
    pub(crate) suggested_tracker_updates: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubmitDelegationReviewFinding {
    pub(crate) severity: String,
    #[serde(default)]
    pub(crate) file: Option<String>,
    #[serde(default)]
    pub(crate) line: Option<u32>,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubmitDelegationReviewCommand {
    pub(crate) command: String,
    pub(crate) status: DelegationReviewCommandStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationReviewCommandStatus {
    Success,
    Error,
}

impl DelegationReviewCommandStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Summary-safe result metadata for parent-scoped APIs and lifecycle deltas.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResultSummary {
    pub(crate) delegation_id: String,
    pub(crate) child_session_id: String,
    pub(crate) status: DelegationStatus,
    pub(crate) summary: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationFinding {
    pub(crate) severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u32>,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationCommandResult {
    pub(crate) command: String,
    pub(crate) status: String,
}

/// Who may judge a task's acceptance criteria, in Engram's own words.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcceptanceEvaluationMode {
    SameSession,
    SubAgent,
    IndependentSession,
}

/// What an evaluator delegation judges. The bases are the revisions the host
/// read before spawning; the evaluator submits against exactly those, so a
/// task that moved on is refused by the tracker instead of silently covered.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    try_from = "PersistedDelegationAcceptanceEvaluation"
)]
pub(crate) struct DelegationAcceptanceEvaluation {
    pub(crate) work_ref: String,
    pub(crate) mode: AcceptanceEvaluationMode,
    /// Host-attested at child creation, never supplied by the evaluator.
    /// Legacy sub-agent targets without this pair cannot submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) execution_identity: Option<String>,
    pub(crate) acceptance_basis: i64,
    pub(crate) evidence_basis: i64,
    pub(crate) criteria_count: usize,
    /// The criteria bound to a typed host check when the brief was read: the
    /// submission refuses a pass on one that Engram could not admit. Empty on a
    /// record persisted before it was kept, which then submits as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) bindings: Vec<AcceptanceCriterionBinding>,
    /// The carried failing evaluation this one acknowledges: a failing
    /// evaluation whose criteria were revised on the run since. The
    /// submission names it to the tracker as `--supersedes`. Absent when none
    /// was carried, and on a record persisted before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) supersedes: Option<String>,
    /// One key per spawn (the delegation id): the tracker replays an identical
    /// resend and refuses different content once one is recorded.
    pub(crate) attempt_key: String,
    /// The tracker store the brief was read from; the bases mean nothing in
    /// another one. Absent only on a record persisted before it was kept,
    /// which can no longer submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) store: Option<EngramAuthorityStoreKey>,
    /// Historical declared source revision, kept without conversion. Fresh
    /// commands do not emit the retired fingerprint flag. An open historical
    /// write is retained in terminal attempt history instead of resent, with
    /// its earlier tracker outcome explicitly unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_fingerprint: Option<String>,
    /// Complete owner identity, private to the persisted host attempt.
    /// Historical attempts keep source_fingerprint without conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_id: Option<EngramSourceId>,
    /// The work's named source root the evaluation was requested on, when
    /// its claim had one (`engram_source_roots.rs`): the evaluator runs there
    /// and its fingerprint is taken there. The first submission is refused
    /// when the work's root has changed since. Absent when the evaluation
    /// measured the requesting session's workdir. Like `source_claim`, it is
    /// host bookkeeping that the stored record needs; the same serialization
    /// carries it to the UI, which does not read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_root: Option<AcceptanceEvaluationSourceRoot>,
    /// The claim the requesting session was bound to when no root was named
    /// (`engram_source_roots.rs`): the first submission is refused if a root
    /// is named for it on this work since, whatever that session is bound to
    /// by then. Absent with a root, or with no binding at the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_claim: Option<AcceptanceEvaluationSourceClaim>,
    /// Immutable request fence, independent of current selection and receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) naming_history: Option<EngramWorkNamingToken>,
    #[serde(
        default,
        skip_serializing_if = "AcceptanceEvaluationSubmission::is_none"
    )]
    pub(crate) submission: AcceptanceEvaluationSubmission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attempt_history: Option<AcceptanceEvaluationAttemptHistory>,
    /// Host-private requester discovery selections for automatic refresh.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) selected_evidence: Vec<AcceptanceSelectedEvidence>,
}

/// What the host knows about this evaluator's one tracker write. `pending` is
/// persisted before the tracker runs, so a crash or a lost response never
/// reads as "nothing was recorded".
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum AcceptanceEvaluationSubmission {
    /// No active host write. Terminal history may still hold an unknown legacy
    /// write retired locally; this state alone proves no tracker outcome.
    #[default]
    None,
    /// `payload_digest` names the arguments and any saved source object.
    /// A shared source's temporary file location is outside that identity.
    Pending {
        payload_digest: String,
        started_at: String,
        #[serde(flatten)]
        original: AcceptanceEvaluationOpenWrite,
    },
    Recorded {
        receipt: AcceptanceEvaluationReceiptExtract,
        recorded_at: String,
    },
    Unconfirmed {
        payload_digest: String,
        reason: String,
        at: String,
        #[serde(flatten)]
        original: AcceptanceEvaluationOpenWrite,
    },
}

/// What an open write was sent as, kept so that its resend is the original
/// whatever the host's settings have become since (a renamed developer,
/// another model): the host's part of the command may drift, the write it must
/// replay may not. Retired legacy writes stay in terminal history instead.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceEvaluationOpenWrite {
    /// Canonical source object, saved before invocation and hidden from clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_payload: Option<AcceptanceSourcePayload>,
    /// Names the evaluator's own part of the command, its normalized verdicts:
    /// what "the same verdicts" is decided on.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) verdicts_digest: String,
    /// The exact argument list, within the command-line guard. Host-private:
    /// it names the actor and is persisted only, never served.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) args: Vec<String>,
}

impl AcceptanceEvaluationSubmission {
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[cfg(test)]
    pub(crate) fn open_write(&self) -> Option<&AcceptanceEvaluationOpenWrite> {
        match self {
            Self::Pending { original, .. } | Self::Unconfirmed { original, .. } => Some(original),
            Self::None | Self::Recorded { .. } => None,
        }
    }
}

impl DelegationAcceptanceEvaluation {
    /// What a client may see: everything but the host-private argument list.
    pub(crate) fn client_view(&self) -> Self {
        let mut view = self.clone();
        view.source_id = None;
        view.selected_evidence.clear();
        if let AcceptanceEvaluationSubmission::Pending { original, .. }
        | AcceptanceEvaluationSubmission::Unconfirmed { original, .. } = &mut view.submission
        {
            original.args.clear();
            original.source_payload = None;
        }
        if let Some(history) = view.attempt_history.as_mut() {
            history.expose_unavailable_refused_submissions(self);
            if let Some(refusal) = history.refusal.as_mut() {
                refusal.original.args.clear();
                refusal.original.source_payload = None;
            }
            for previous in &mut history.previous {
                previous.source_id = None;
                if let Some(refusal) = previous.refusal.as_mut() {
                    refusal.original.args.clear();
                    refusal.original.source_payload = None;
                }
                if let AcceptanceEvaluationSubmission::Pending { original, .. }
                | AcceptanceEvaluationSubmission::Unconfirmed { original, .. } =
                    &mut previous.submission
                {
                    original.args.clear();
                    original.source_payload = None;
                }
            }
        }
        view
    }
}

/// The bounded part of the tracker's receipt a parent needs. The raw receipt
/// (up to a whole control frame) goes to the evaluator once and is not kept.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceEvaluationReceiptExtract {
    // Older persisted host snapshots used evaluationHash. Always emit the ID
    // name; this alias is not the live producer's temporary hash fallback.
    #[serde(
        default,
        alias = "evaluationHash",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) evaluation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verdicts_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) blocking: Option<AcceptanceEvaluationBlockingVerdict>,
    #[serde(default)]
    pub(crate) replayed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) work_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evaluated_cut: Option<i64>,
}

/// The first criterion that keeps the task from completing.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceEvaluationBlockingVerdict {
    pub(crate) position: u64,
    pub(crate) verdict: String,
}

/// The persisted shape, which still accepts the raw `outcome.receipt` the
/// first build stored and folds it into the bounded extract.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedDelegationAcceptanceEvaluation {
    pub(crate) work_ref: String,
    pub(crate) mode: AcceptanceEvaluationMode,
    #[serde(default)]
    pub(crate) parent_session: Option<String>,
    #[serde(default)]
    pub(crate) execution_identity: Option<String>,
    pub(crate) acceptance_basis: i64,
    pub(crate) evidence_basis: i64,
    pub(crate) criteria_count: usize,
    #[serde(default)]
    pub(crate) bindings: Vec<AcceptanceCriterionBinding>,
    #[serde(default)]
    pub(crate) supersedes: Option<String>,
    pub(crate) attempt_key: String,
    #[serde(default)]
    pub(crate) store: Option<EngramAuthorityStoreKey>,
    #[serde(default)]
    pub(crate) source_fingerprint: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_acceptance_optional_source_id"
    )]
    pub(crate) source_id: Option<EngramSourceId>,
    #[serde(default)]
    pub(crate) source_root: Option<AcceptanceEvaluationSourceRoot>,
    #[serde(default)]
    pub(crate) source_claim: Option<AcceptanceEvaluationSourceClaim>,
    #[serde(default)]
    pub(crate) naming_history: Option<EngramWorkNamingToken>,
    #[serde(default)]
    pub(crate) submission: AcceptanceEvaluationSubmission,
    #[serde(default)]
    pub(crate) attempt_history: Option<AcceptanceEvaluationAttemptHistory>,
    #[serde(default)]
    pub(crate) selected_evidence: Vec<AcceptanceSelectedEvidence>,
    #[serde(default)]
    pub(crate) outcome: Option<PersistedRawAcceptanceEvaluationOutcome>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedRawAcceptanceEvaluationOutcome {
    pub(crate) receipt: Value,
    pub(crate) recorded_at: String,
}

impl TryFrom<PersistedDelegationAcceptanceEvaluation> for DelegationAcceptanceEvaluation {
    type Error = &'static str;

    fn try_from(
        persisted: PersistedDelegationAcceptanceEvaluation,
    ) -> std::result::Result<Self, Self::Error> {
        if persisted.source_id.is_some() && persisted.source_fingerprint.is_some() {
            return Err("evaluation target has more than one source identity");
        }
        let submission = match (persisted.submission, persisted.outcome) {
            (AcceptanceEvaluationSubmission::None, Some(outcome)) => {
                AcceptanceEvaluationSubmission::Recorded {
                    receipt: acceptance_evaluation_receipt_extract(&outcome.receipt),
                    recorded_at: outcome.recorded_at,
                }
            }
            (submission, _) => submission,
        };
        Ok(Self {
            work_ref: persisted.work_ref,
            mode: persisted.mode,
            parent_session: persisted.parent_session,
            execution_identity: persisted.execution_identity,
            acceptance_basis: persisted.acceptance_basis,
            evidence_basis: persisted.evidence_basis,
            criteria_count: persisted.criteria_count,
            bindings: persisted.bindings,
            supersedes: persisted.supersedes,
            attempt_key: persisted.attempt_key,
            store: persisted.store,
            source_fingerprint: persisted.source_fingerprint,
            source_id: persisted.source_id,
            source_root: persisted.source_root,
            source_claim: persisted.source_claim,
            naming_history: persisted.naming_history,
            submission,
            attempt_history: persisted.attempt_history,
            selected_evidence: persisted.selected_evidence,
        })
    }
}

/// Persisted parent-child delegation metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationRecord {
    /// Distinguishes immutable no-project commissioning from a legacy record.
    #[serde(default)]
    pub(crate) project_basis_recorded: bool,
    /// Original host-recorded project at commissioning; never changed by follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) original_project_id: Option<String>,
    pub(crate) id: String,
    pub(crate) parent_session_id: String,
    pub(crate) child_session_id: String,
    pub(crate) mode: DelegationMode,
    pub(crate) status: DelegationStatus,
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) cwd: String,
    pub(crate) agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) write_policy: DelegationWritePolicy,
    /// Immutable commit used to create a worker workspace; absent on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_revision: Option<String>,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<DelegationResult>,
    /// Validated reviewer result submitted before the child finishes its
    /// current turn. Once accepted, the submission is authoritative; terminal
    /// child lifecycle observation only decides when it is promoted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) submitted_review_result: Option<DelegationResult>,
    /// A child transport failure observed after TermAl had already accepted a
    /// validated structured result. The submitted result remains authoritative;
    /// this field preserves the later transport diagnostic separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) post_submission_transport_error: Option<String>,
    /// Attempt for which durable recovery already proved that no usable
    /// structured envelope exists. This is an in-process probe only: a restart
    /// deliberately rechecks the coordination store once.
    #[serde(skip)]
    pub(crate) review_result_recovery_probe_attempt: Option<u32>,
    /// Diagnostic for a durable envelope that recovery quarantined instead of
    /// allowing it to break parent lifecycle APIs. It is serialized for the
    /// current process so operators can see the reason, but ignored on reload
    /// so recovery gets a fresh probe after restart.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub(crate) review_result_recovery_error: Option<String>,
    /// Prevents later prose-parser upgrades from overwriting a result that
    /// arrived through the strict mailbox contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review_result_schema_version: Option<u32>,
    /// First accepted follow-up prompt when admission queued rather than started
    /// it. While this exact id is queued and has no user-message boundary, old
    /// child output belongs to the previous attempt. Persisted with the queue;
    /// subsequent turns and their leftover queues must not inherit this fence.
    /// Cleared on terminal settlement; retained during promotion so a failed
    /// queue-promotion commit can restore the queue without losing its fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) queued_followup_prompt_id: Option<String>,
    /// Backend-owned turn number for idempotent structured review submission.
    /// Required reviewer delegations start at one; other delegations keep zero.
    /// The value advances whenever a completed review is rearmed.
    #[serde(default)]
    pub(crate) review_result_submission_attempt: u32,
    /// Evaluation target and recorded outcome; present only in evaluator mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) acceptance_evaluation: Option<DelegationAcceptanceEvaluation>,
    /// Whether the current attempt is held or has not started; maintained
    /// from the child by `refresh_delegation_from_child_locked`.
    #[serde(flatten)]
    pub(crate) attempt: DelegationAttemptState,
}

/// Determines when a delegation wait resumes its parent session.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationWaitMode {
    Any,
    #[default]
    All,
}

/// Durable pending resume request for one parent waiting on child delegations.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationWaitRecord {
    pub(crate) id: String,
    pub(crate) parent_session_id: String,
    pub(crate) delegation_ids: Vec<String>,
    pub(crate) mode: DelegationWaitMode,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
}

/// Summary-safe delegation metadata for parent-scoped APIs and lifecycle deltas.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationSummary {
    pub(crate) id: String,
    pub(crate) parent_session_id: String,
    pub(crate) child_session_id: String,
    pub(crate) mode: DelegationMode,
    pub(crate) status: DelegationStatus,
    pub(crate) title: String,
    pub(crate) agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) write_policy: DelegationWritePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_revision: Option<String>,
    pub(crate) created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<String>,
    /// Whether this child must publish a validated structured reviewer result.
    /// Parent-scoped delegation APIs, lifecycle deltas, and the minimal broad
    /// state capability summary expose this bit. Broad state still omits the
    /// delegation's lifecycle and result payloads.
    pub(crate) review_result_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) post_submission_transport_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review_result_recovery_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<DelegationResultSummary>,
    /// What an evaluator delegation judged and what the tracker recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) acceptance_evaluation: Option<DelegationAcceptanceEvaluation>,
    /// Present while `status` is `held`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) hold: Option<DelegationHold>,
}

/// Minimal delegation identity and MCP capability metadata carried by broad
/// state snapshots.
///
/// The frontend and peer-session discovery need the durable link to classify
/// child sessions. The delegation MCP bridge additionally needs `mode` and
/// `review_result_required` to advertise the structured review submission tool
/// only to eligible reviewer children. Full lifecycle/result metadata remains
/// available from the parent-scoped delegation endpoints and delegation SSE
/// deltas.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationStateSummary {
    pub(crate) id: String,
    pub(crate) child_session_id: String,
    pub(crate) mode: DelegationMode,
    pub(crate) review_result_required: bool,
    /// Static policy capability; the endpoint separately checks live authority.
    #[serde(default)]
    pub(crate) review_freeze_allowed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) review_identity_allowed: bool,
    /// Static policy capability for the evaluator-only submission tool; the
    /// endpoint separately checks live authority. Written only when true, so
    /// every other delegation's broad-state link keeps its shape.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) acceptance_evaluation_allowed: bool,
}

/// Request payload for creating a delegation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateDelegationRequest {
    pub(crate) prompt: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) agent: Option<Agent>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) mode: Option<DelegationMode>,
    #[serde(default)]
    pub(crate) write_policy: Option<DelegationWritePolicy>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResponse {
    pub(crate) revision: u64,
    #[serde(serialize_with = "serialize_delegation_record_for_api")]
    pub(crate) delegation: DelegationRecord,
    pub(crate) child_session: Session,
    /// How the turn this call started was left: on create the first turn, on
    /// a follow-up its turn. Absent on responses that started no turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) first_turn: Option<DelegationTurnDelivery>,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationStatusResponse {
    pub(crate) revision: u64,
    #[serde(serialize_with = "serialize_delegation_record_for_api")]
    pub(crate) delegation: DelegationRecord,
    /// On a follow-up: how the turn it started was left. Absent elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) turn: Option<DelegationTurnDelivery>,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Compact parent-scoped delegation inventory used to recover delegation ids.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationListResponse {
    pub(crate) revision: u64,
    pub(crate) delegations: Vec<DelegationSummary>,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Confirmed immutable publications, separate from the mutable current packet.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetainedReviewAttemptIdentity {
    pub(crate) submission_attempt: u32,
    pub(crate) result_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResultResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) retained_review_attempts: Vec<RetainedReviewAttemptIdentity>,
    /// Discovery failure does not hide the parent's available current result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retained_review_attempts_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review_attempt_state: Option<ReviewAttemptReadState>,
    pub(crate) revision: u64,
    pub(crate) result: DelegationResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) submission_attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review_attempt_provenance: Option<ReviewAttemptProvenance>,
    /// Evaluator delegations only: the target and the recorded outcome, so a
    /// completed child that recorded nothing is visible as exactly that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) acceptance_evaluation: Option<DelegationAcceptanceEvaluation>,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Byte-oriented page request for the authoritative final child output.
/// Offsets returned by the server are always UTF-8 boundaries and can be fed
/// back verbatim for the next page.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResultOutputQuery {
    pub(crate) submission_attempt: Option<u32>,
    pub(crate) result_id: Option<String>,
    #[serde(default)]
    pub(crate) offset_bytes: usize,
    #[serde(default = "default_delegation_result_output_page_bytes")]
    pub(crate) limit_bytes: usize,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResultSelection {
    pub(crate) submission_attempt: Option<u32>,
    pub(crate) result_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationResultOutputResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review_attempt_state: Option<ReviewAttemptReadState>,
    pub(crate) revision: u64,
    pub(crate) delegation_id: String,
    pub(crate) child_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) submission_attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_id: Option<String>,
    pub(crate) output: String,
    pub(crate) offset_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_offset_bytes: Option<usize>,
    pub(crate) total_bytes: usize,
    pub(crate) complete: bool,
    /// True when no current-attempt assistant output can be attributed and
    /// the compact result summary had to be used instead.
    pub(crate) summary_fallback: bool,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Request payload for scheduling a parent resume after delegations finish.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateDelegationWaitRequest {
    pub(crate) delegation_ids: Vec<String>,
    #[serde(default)]
    pub(crate) mode: DelegationWaitMode,
    #[serde(default)]
    pub(crate) title: Option<String>,
}

/// Response payload for scheduling a delegation wait.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DelegationWaitResponse {
    pub(crate) revision: u64,
    pub(crate) wait: DelegationWaitRecord,
    pub(crate) resume_prompt_queued: bool,
    pub(crate) resume_dispatch_requested: bool,
    pub(crate) server_instance_id: String,
}

/// Reason a delegation wait left the pending wait set.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DelegationWaitConsumedReason {
    Completed,
    /// A watched child is held: the wait ended so its parent can act, and
    /// the child has not completed. A new wait is needed for its result.
    AttentionRequired,
    ParentSessionStopped,
    ParentSessionUnavailable,
    ParentSessionRemoved,
}

/// Represents the create project request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateProjectRequest {
    pub(crate) name: Option<String>,
    pub(crate) root_path: String,
    #[serde(default = "default_local_remote_id")]
    pub(crate) remote_id: String,
}

/// Patches one project's Engram host-adapter configuration. The route is
/// intentionally project-scoped so Engram is never enabled globally by
/// accident, and a project can be disabled immediately without a restart.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateProjectEngramSettingsRequest {
    #[serde(default)]
    pub(crate) acceptance_evaluation: Option<AcceptanceEvaluatorDefaults>,
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Absent keeps the project's stored choice: an update that does not name
    /// it, such as Disable, never turns observations back on.
    #[serde(default)]
    pub(crate) execution_observability: Option<bool>,
    #[serde(default)]
    pub(crate) binary_path: Option<String>,
    #[serde(default)]
    pub(crate) home: Option<String>,
    #[serde(default)]
    pub(crate) deadline_ms: Option<u64>,
}

/// Redacted, non-mutating verification result for the project Engram settings
/// UI.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifyProjectEngramSettingsResponse {
    pub(crate) verified: bool,
    pub(crate) binary_path: String,
    pub(crate) home: String,
    pub(crate) project_id: String,
    pub(crate) database: String,
    pub(crate) ready: bool,
    pub(crate) full_audit: &'static str,
    pub(crate) host_path_status: String,
    pub(crate) elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) errors: Vec<String>,
}

/// Updates the machine-scoped Engram principal, executable, and home.
/// Repository declaration and per-project tier selection remain separate
/// concerns.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateEngramHostSettingsRequest {
    pub(crate) developer_name: String,
    pub(crate) binary_path: String,
    pub(crate) home: String,
}

impl UpdateProjectEngramSettingsRequest {
    /// Whether the request leaves Execution Observability as the project has
    /// it stored.
    pub(crate) fn keeps_stored_observability(&self) -> bool {
        self.execution_observability.is_none()
    }

    /// The settings the request names. An absent Execution Observability
    /// takes the stored record's default here; an update replaces it with the
    /// project's stored choice (`keeps_stored_observability`).
    pub(crate) fn into_settings(self) -> EngramProjectSettings {
        EngramProjectSettings {
            acceptance_evaluation: self.acceptance_evaluation,
            enabled: self.enabled,
            execution_observability: self
                .execution_observability
                .unwrap_or_else(default_engram_execution_observability),
            binary_path: self.binary_path,
            home: self.home,
            authority_store_key: None,
            deadline_ms: self.deadline_ms,
        }
    }
}

/// Represents the update app settings request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateAppSettingsRequest {
    #[serde(default)]
    pub(crate) default_kimi_approval_mode: Option<KimiApprovalMode>,
    /// `auto` (the CLI's current value) or an effort value; copied onto new
    /// Kimi sessions when they are created.
    #[serde(default)]
    pub(crate) default_kimi_effort: Option<String>,
    #[serde(rename = "defaultOpenCodeApprovalMode")]
    pub(crate) default_opencode_approval_mode: Option<OpenCodeApprovalMode>,
    pub(crate) default_codex_model: Option<String>,
    pub(crate) default_codex_sandbox_mode: Option<CodexSandboxMode>,
    pub(crate) default_codex_approval_policy: Option<CodexApprovalPolicy>,
    pub(crate) default_claude_model: Option<String>,
    pub(crate) default_cursor_model: Option<String>,
    pub(crate) default_gemini_model: Option<String>,
    pub(crate) default_kimi_model: Option<String>,
    #[serde(rename = "defaultOpenCodeModel")]
    pub(crate) default_opencode_model: Option<String>,
    pub(crate) default_codex_reasoning_effort: Option<CodexReasoningEffort>,
    pub(crate) default_claude_approval_mode: Option<ClaudeApprovalMode>,
    pub(crate) default_claude_effort: Option<ClaudeEffortLevel>,
    pub(crate) remotes: Option<Vec<RemoteConfig>>,
}

/// Request payload for registering an existing TermAl checkout on an SSH remote.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteRegisterRequest {
    pub(crate) source_path: String,
}

/// Response payload for one-shot SSH remote lifecycle actions.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteActionResponse {
    pub(crate) remote_id: String,
    pub(crate) action: String,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) stderr: String,
}

/// UI-owned Telegram relay configuration committed through app preferences.
///
/// App preferences are the sole configuration source. Runtime JSON contains
/// only relay state; tokens are accepted by the dedicated settings request
/// and stored only in the OS credential store.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TelegramUiConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) forward_assistant_replies: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) subscribed_project_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_session_id: Option<String>,
}

pub(crate) fn telegram_ui_config_is_default(config: &TelegramUiConfig) -> bool {
    config == &TelegramUiConfig::default()
}

/// UI-visible Telegram relay lifecycle.
///
/// The current relay always runs in the backend process.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TelegramLifecycle {
    InProcess,
}

/// Represents the Telegram settings response payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TelegramStatusResponse {
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
    pub(crate) forward_assistant_replies: bool,
    pub(crate) running: bool,
    pub(crate) lifecycle: TelegramLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) linked_chat_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bot_token_masked: Option<String>,
    #[serde(default)]
    pub(crate) subscribed_project_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_session_id: Option<String>,
}

/// Represents the update Telegram settings request payload.
///
/// String fields that use `deserialize_nullable_marker_field` follow the same
/// tri-state PATCH convention as marker fields elsewhere in the API: omitted
/// means "leave unchanged", `null` means "clear the saved value", and a string
/// means "replace with this value". `subscribed_project_ids` is a list field:
/// omitted or `null` leaves it unchanged, while an array replaces the list.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateTelegramConfigRequest {
    pub(crate) enabled: Option<bool>,
    pub(crate) forward_assistant_replies: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_nullable_marker_field")]
    pub(crate) bot_token: Option<Option<String>>,
    pub(crate) subscribed_project_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_marker_field")]
    pub(crate) default_project_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_marker_field")]
    pub(crate) default_session_id: Option<Option<String>>,
}

/// Represents the Telegram connection test request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TelegramTestRequest {
    #[serde(default, deserialize_with = "deserialize_nullable_marker_field")]
    pub(crate) bot_token: Option<Option<String>>,
    #[serde(default)]
    pub(crate) use_saved_token: bool,
}

/// Represents the Telegram connection test response payload.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TelegramTestResponse {
    pub(crate) bot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bot_username: Option<String>,
}

/// Represents file query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileQuery {
    pub(crate) path: String,
    pub(crate) project_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// Represents instruction search query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionSearchQuery {
    pub(crate) q: String,
    pub(crate) session_id: String,
}

/// Represents review query.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewQuery {
    pub(crate) project_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// Represents the Codex thread rollback request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexThreadRollbackRequest {
    #[serde(default = "default_codex_thread_rollback_turns")]
    pub(crate) num_turns: usize,
}

/// Returns the default Codex thread rollback turns.
fn default_codex_thread_rollback_turns() -> usize {
    1
}

/// Represents the write file request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WriteFileRequest {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) base_hash: Option<String>,
    #[serde(default)]
    pub(crate) overwrite: bool,
    pub(crate) project_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// Represents the file response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileResponse {
    pub(crate) path: String,
    pub(crate) content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mtime_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
}

/// Enumerates file system entry kinds.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FileSystemEntryKind {
    Directory,
    File,
}

/// Represents directory entry.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryEntry {
    pub(crate) kind: FileSystemEntryKind,
    pub(crate) name: String,
    pub(crate) path: String,
}

/// Represents the directory response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryResponse {
    pub(crate) entries: Vec<DirectoryEntry>,
    pub(crate) name: String,
    pub(crate) path: String,
}

/// Represents the error response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ErrorResponse {
    pub(crate) error: String,
}

/// Represents the health response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HealthResponse {
    pub(crate) ok: bool,
    /// UUID generated at `AppState::new_with_paths` boot. Stable for
    /// the lifetime of the process, changes on every restart. Clients
    /// use a mismatch between this and their last-seen id to detect a
    /// server restart deterministically — see `shouldAdoptSnapshotRevision`
    /// in the frontend.
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Serializes the persisted record for parent-scoped APIs while deriving the
/// current reviewer-result capability from its canonical source, `mode`.
fn serialize_delegation_record_for_api<S>(
    record: &DelegationRecord,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ApiDelegationRecord<'a> {
        #[serde(flatten)]
        record: &'a DelegationRecord,
        review_result_required: bool,
    }

    // Current and historical acceptance writes carry host-private argument
    // lists. Strip both even when the current submission is already settled.
    let mut client_record = record
        .acceptance_evaluation
        .as_ref()
        .map(|target| DelegationRecord {
            acceptance_evaluation: Some(target.client_view()),
            ..record.clone()
        });
    // A stored `running` attempt that is held or has not started is reported
    // as such; the stored status itself is host bookkeeping.
    let public_status = public_delegation_status(record);
    if public_status != record.status {
        client_record.get_or_insert_with(|| record.clone()).status = public_status;
    }
    // Clients get the hold; the pending-start flag and the hold-generation
    // counter are bookkeeping behind the status and `hold.generation`.
    if record.attempt.pending_start || record.attempt.hold_generation != 0 {
        let client = client_record.get_or_insert_with(|| record.clone());
        client.attempt.pending_start = false;
        client.attempt.hold_generation = 0;
    }
    if record.write_policy == DelegationWritePolicy::ReadOnly
        && [&record.result, &record.submitted_review_result]
            .into_iter()
            .flatten()
            .any(|result| {
                !result.changed_files.is_empty() || !result.observed_workspace_changes.is_empty()
            })
    {
        let client = client_record.get_or_insert_with(|| record.clone());
        for result in [&mut client.result, &mut client.submitted_review_result]
            .into_iter()
            .flatten()
        {
            *result = delegation_result_for_policy(result, &record.write_policy).into_owned();
        }
    }
    ApiDelegationRecord {
        record: client_record.as_ref().unwrap_or(record),
        review_result_required: record.mode == DelegationMode::Reviewer,
    }
    .serialize(serializer)
}

/// Origin declared on the explicit UI action, outside agent/mailbox payloads.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ExplicitPromptOrigin {
    Human,
}

/// The UI states its origin on the request that creates the envelope. Legacy
/// bodies remain ordinary/automated; missing attribution never grants this.
#[derive(Deserialize)]
pub(crate) struct SendMessageRouteRequest {
    #[serde(flatten)]
    pub(crate) prompt: SendMessageRequest,
    #[serde(default)]
    pub(crate) origin: Option<ExplicitPromptOrigin>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HumanPromptSelection {
    pub(crate) prompt: PendingPrompt,
    pub(crate) session_mutation_stamp: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumeQueueRequest {
    #[serde(default)]
    pub(crate) origin: Option<ExplicitPromptOrigin>,
    #[serde(default)]
    pub(crate) human_choice: Option<HumanPromptSelection>,
}

/// Represents the ordinary internal send message request payload.
#[derive(Deserialize, Serialize)]
pub(crate) struct SendMessageRequest {
    pub(crate) text: String,
    #[serde(default, rename = "expandedText")]
    pub(crate) expanded_text: Option<String>,
    #[serde(default)]
    pub(crate) attachments: Vec<SendMessageAttachmentRequest>,
    /// Peer sender session id, set only by `termal_send_to_session`. The
    /// backend resolves it to the sender's display name at dispatch time; a
    /// missing/unknown id yields no attribution (the message stays "You").
    #[serde(default, rename = "sourceSessionId")]
    pub(crate) source_session_id: Option<String>,
    /// Internal durable-mailbox provenance for the compact wake-up prompt
    /// created by the backend after committing a peer message. The ordinary
    /// `/messages` JSON surface must not be able to forge this field.
    #[serde(default, rename = "sourceMailbox", skip_deserializing)]
    pub(crate) source_mailbox: Option<MailboxMessageSource>,
}

/// Represents the send message attachment request payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendMessageAttachmentRequest {
    pub(crate) data: String,
    pub(crate) file_name: Option<String>,
    pub(crate) media_type: String,
}

/// Represents the delegation follow-up request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FollowupDelegationRequest {
    pub(crate) message: String,
}

/// Represents the update session settings request payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateSessionSettingsRequest {
    pub(crate) kimi_effort: Option<String>,
    #[serde(default)]
    pub(crate) kimi_approval_mode: Option<KimiApprovalMode>,
    #[serde(default)]
    pub(crate) kimi_mode: Option<KimiMode>,
    pub(crate) opencode_approval_mode: Option<OpenCodeApprovalMode>,
    pub(crate) name: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) approval_policy: Option<CodexApprovalPolicy>,
    pub(crate) reasoning_effort: Option<CodexReasoningEffort>,
    pub(crate) codex_fast_mode: Option<bool>,
    pub(crate) sandbox_mode: Option<CodexSandboxMode>,
    pub(crate) cursor_mode: Option<CursorMode>,
    pub(crate) claude_approval_mode: Option<ClaudeApprovalMode>,
    pub(crate) claude_effort: Option<ClaudeEffortLevel>,
    pub(crate) gemini_approval_mode: Option<GeminiApprovalMode>,
    pub(crate) opencode_effort: Option<String>,
    pub(crate) opencode_mode: Option<String>,
}

/// Represents a model-specific Codex service tier advertised by `model/list`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionModelServiceTier {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

/// Represents session model option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionModelOption {
    pub(crate) label: String,
    pub(crate) value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) badges: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) supported_claude_effort_levels: Vec<ClaudeEffortLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_reasoning_effort: Option<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) supported_reasoning_efforts: Vec<CodexReasoningEffort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) service_tiers: Vec<SessionModelServiceTier>,
}

impl SessionModelOption {
    /// Builds the plain response value.
    #[cfg(test)]
    pub(crate) fn plain(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
            badges: Vec::new(),
            supported_claude_effort_levels: Vec::new(),
            default_reasoning_effort: None,
            supported_reasoning_efforts: Vec::new(),
            service_tiers: Vec::new(),
        }
    }
}

/// Maximum number of Codex notices retained in state and sent over SSE.
pub(crate) const CODEX_NOTICE_CAP: usize = 5;

/// Tracks Codex state.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rate_limits: Option<CodexRateLimits>,
    /// Most-recent-first Codex notices, capped at [`CODEX_NOTICE_CAP`] by
    /// `AppState::note_codex_notice`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) notices: Vec<CodexNotice>,
}

impl CodexState {
    /// Returns whether empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.rate_limits.is_none() && self.notices.is_empty()
    }
}

/// Enumerates Codex notice kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CodexNoticeKind {
    ConfigWarning,
    DeprecationNotice,
    RuntimeNotice,
}

/// Defines the Codex notice level variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CodexNoticeLevel {
    Info,
    Warning,
}

/// Represents Codex notice.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexNotice {
    pub(crate) kind: CodexNoticeKind,
    pub(crate) level: CodexNoticeLevel,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<String>,
}

/// Represents Codex rate limits.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexRateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) primary: Option<CodexRateLimitWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) secondary: Option<CodexRateLimitWindow>,
}

/// Represents Codex rate limit window.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexRateLimitWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) used_percent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_duration_mins: Option<u64>,
}

/// Enumerates agent readiness states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AgentReadinessStatus {
    Ready,
    Missing,
    NeedsSetup,
}

/// Represents agent readiness.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentReadiness {
    pub(crate) agent: Agent,
    pub(crate) status: AgentReadinessStatus,
    pub(crate) blocking: bool,
    pub(crate) detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) warning_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command_path: Option<String>,
}

/// Stale remote-mirror status and its required recovery action.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteTransportStatus {
    pub(crate) remote_id: String,
    pub(crate) code: String,
    pub(crate) stale: bool,
    pub(crate) detail: String,
    pub(crate) recovery_action: String,
}

/// Represents the state response payload.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StateResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) remote_transport_statuses: Vec<RemoteTransportStatus>,
    pub(crate) revision: u64,
    /// UUID generated at `AppState::new_with_paths` boot; see
    /// `HealthResponse::server_instance_id` for semantics. Carried on
    /// every snapshot so clients can distinguish "revision decreased
    /// because the server restarted" from "revision decreased because
    /// this response is stale".
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
    #[serde(default)]
    pub(crate) codex: CodexState,
    #[serde(default)]
    pub(crate) agent_readiness: Vec<AgentReadiness>,
    pub(crate) preferences: AppPreferences,
    #[serde(
        default,
        serialize_with = "serialize_client_projects_with_engram_status"
    )]
    pub(crate) projects: Vec<Project>,
    #[serde(default)]
    pub(crate) orchestrators: Vec<OrchestratorInstance>,
    #[serde(default)]
    pub(crate) workspaces: Vec<WorkspaceLayoutSummary>,
    #[serde(default)]
    pub(crate) sessions: Vec<StateSessionSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) delegations: Vec<DelegationStateSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) delegation_waits: Vec<DelegationWaitRecord>,
    /// Revision-coherent counters, never a global catalogue of run rows.
    #[serde(default)]
    pub(crate) test_run_overview: TestRunOverviewSnapshot,
    /// Pending run waits only (docs/features/test-runs.md, run waits).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) test_run_waits: Vec<TestRunWaitRecord>,
    /// Sessions whose stale Engram MCP runtime will be revoked by the Stop
    /// operation that already owns their runtime fence. Normal snapshots and
    /// SSE state events preserve this durable-in-memory pending state so a
    /// mixed cleanup failure cannot hide the sessions still awaiting Stop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) pending_engram_mcp_revocation_session_ids: Vec<String>,
}

/// Test-only view of retained state. This is deliberately a different type
/// from `StateResponse`, so test introspection cannot accidentally weaken the
/// transcript-free production wire contract.
#[cfg(test)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FullStateSnapshot {
    pub(crate) revision: u64,
    pub(crate) server_instance_id: String,
    pub(crate) codex: CodexState,
    pub(crate) agent_readiness: Vec<AgentReadiness>,
    pub(crate) preferences: AppPreferences,
    #[serde(serialize_with = "serialize_client_projects_with_engram_status")]
    pub(crate) projects: Vec<Project>,
    pub(crate) orchestrators: Vec<OrchestratorInstance>,
    pub(crate) workspaces: Vec<WorkspaceLayoutSummary>,
    pub(crate) sessions: Vec<Session>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) delegations: Vec<DelegationStateSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) delegation_waits: Vec<DelegationWaitRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) pending_engram_mcp_revocation_session_ids: Vec<String>,
}

/// Represents one bounded session-tail response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionResponse {
    pub(crate) revision: u64,
    pub(crate) session: Session,
    /// See `StateResponse::server_instance_id` — same semantics. The
    /// frontend's `adoptFetchedSession` uses a mismatch against the
    /// last-seen id to accept a revision downgrade after a server
    /// restart. Without this field, a session hydration in flight
    /// across a restart could be silently rejected by the monotonic
    /// revision guard until the safety-net pollers re-fetch.
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// One bounded, ascending transcript page.
///
/// `next_before` is the first message id in `messages` when older history
/// remains. Supplying it as the next request's exclusive `before` cursor walks
/// toward the beginning without relying on mutation-prone array indexes.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionHistoryResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) body_seq_epoch: Option<String>,
    pub(crate) messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_before: Option<String>,
    pub(crate) has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_after: Option<String>,
    #[serde(default)]
    pub(crate) has_newer: bool,
    #[serde(default)]
    pub(crate) message_start_index: usize,
    pub(crate) message_count: u32,
    pub(crate) revision: u64,
    pub(crate) session_mutation_stamp: u64,
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Defines the workspace control panel side variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WorkspaceControlPanelSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WorkspaceThemeMode {
    Light,
    Dark,
    Auto,
}

/// Represents the workspace layout document.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceLayoutDocument {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    pub(crate) revision: u64,
    pub(crate) updated_at: String,
    pub(crate) control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) light_theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dark_theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) theme_mode: Option<WorkspaceThemeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) density_percent: Option<u32>,
    pub(crate) workspace: Value,
}

/// Represents the workspace layout response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceLayoutResponse {
    pub(crate) layout: WorkspaceLayoutDocument,
}

/// Summarizes workspace layout.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceLayoutSummary {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    pub(crate) revision: u64,
    pub(crate) updated_at: String,
    pub(crate) control_panel_side: WorkspaceControlPanelSide,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) light_theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dark_theme_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) theme_mode: Option<WorkspaceThemeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) style_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) editor_font_size_px: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) density_percent: Option<u32>,
}

/// Represents the workspace layouts response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceLayoutsResponse {
    pub(crate) workspaces: Vec<WorkspaceLayoutSummary>,
}

#[derive(Deserialize)]
pub(crate) struct PatchWorkspaceLabelRequest {
    pub(crate) label: String,
}

/// Represents the put workspace layout request payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PutWorkspaceLayoutRequest {
    pub(crate) control_panel_side: WorkspaceControlPanelSide,
    #[serde(default)]
    pub(crate) light_theme_id: Option<String>,
    #[serde(default)]
    pub(crate) dark_theme_id: Option<String>,
    #[serde(default)]
    pub(crate) theme_mode: Option<WorkspaceThemeMode>,
    #[serde(default)]
    pub(crate) style_id: Option<String>,
    #[serde(default)]
    pub(crate) font_size_px: Option<u32>,
    #[serde(default)]
    pub(crate) editor_font_size_px: Option<u32>,
    #[serde(default)]
    pub(crate) density_percent: Option<u32>,
    pub(crate) workspace: Value,
}

/// Represents the create session response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateSessionResponse {
    pub(crate) session_id: String,
    pub(crate) session: Session,
    pub(crate) revision: u64,
    /// See `StateResponse::server_instance_id` — same semantics. The
    /// frontend's `adoptCreatedSessionResponse` uses a mismatch
    /// against the last-seen id to accept a revision downgrade after
    /// a server restart, which is the common case for this response
    /// (POST sent from a stale browser tab against a freshly started
    /// server).
    #[serde(deserialize_with = "deserialize_nonempty_server_instance_id")]
    pub(crate) server_instance_id: String,
}

/// Represents the create project response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateProjectResponse {
    pub(crate) project_id: String,
    pub(crate) state: StateResponse,
}

/// Represents a agent command.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentCommand {
    #[serde(default)]
    pub(crate) kind: AgentCommandKind,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) content: String,
    pub(crate) source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) argument_hint: Option<String>,
    #[serde(skip)]
    pub(crate) resolver_frontmatter: Option<String>,
    #[serde(skip)]
    pub(crate) resolver_frontmatter_trusted: bool,
}

/// Represents the agent commands response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentCommandsResponse {
    pub(crate) commands: Vec<AgentCommand>,
}

/// Sanitized MCP tool metadata returned by Codex's app-server.
///
/// The app-server also reports each tool's input schema and opaque metadata,
/// but the composer only needs human-readable inventory information. Keeping
/// the public response narrow avoids turning `/mcp` into a raw protocol dump.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexMcpToolSummary {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
}

/// Sanitized status for one MCP server configured in the Codex app-server.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexMcpServerStatus {
    pub(crate) name: String,
    pub(crate) auth_status: String,
    pub(crate) tools: Vec<CodexMcpToolSummary>,
}

/// Response payload for the Codex `/mcp` composer command.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexMcpServersResponse {
    pub(crate) servers: Vec<CodexMcpServerStatus>,
}

/// Indicates why an agent command is being resolved.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AgentCommandResolveIntent {
    Send,
    Delegate,
}

impl Default for AgentCommandResolveIntent {
    /// Builds the default value.
    fn default() -> Self {
        Self::Send
    }
}

/// Request payload for resolving an agent command into a concrete prompt.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolveAgentCommandRequest {
    #[serde(default)]
    pub(crate) arguments: Option<String>,
    #[serde(default)]
    pub(crate) note: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) intent: AgentCommandResolveIntent,
}

/// Optional delegation defaults derived from a resolved command.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedAgentCommandDelegationDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<DelegationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) write_policy: Option<DelegationWritePolicy>,
}

/// Response payload for a resolved agent command.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolveAgentCommandResponse {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) kind: AgentCommandKind,
    pub(crate) visible_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expanded_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delegation: Option<ResolvedAgentCommandDelegationDefaults>,
}

/// Enumerates agent command kinds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AgentCommandKind {
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
pub(crate) enum InstructionDocumentKind {
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
pub(crate) enum InstructionRelation {
    MarkdownLink,
    FileReference,
    DirectoryDiscovery,
}

/// Represents the instruction search response payload.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionSearchResponse {
    pub(crate) matches: Vec<InstructionSearchMatch>,
    pub(crate) query: String,
    pub(crate) workdir: String,
}

/// Represents instruction search match.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionSearchMatch {
    pub(crate) line: usize,
    pub(crate) path: String,
    pub(crate) root_paths: Vec<InstructionRootPath>,
    pub(crate) text: String,
}

/// Represents instruction root path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionRootPath {
    pub(crate) root_kind: InstructionDocumentKind,
    pub(crate) root_path: String,
    pub(crate) steps: Vec<InstructionPathStep>,
}

/// Represents instruction path step.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstructionPathStep {
    pub(crate) excerpt: String,
    pub(crate) from_path: String,
    pub(crate) line: usize,
    pub(crate) relation: InstructionRelation,
    pub(crate) to_path: String,
}

/// Represents instruction document internal.
#[derive(Clone, Debug)]
pub(crate) struct InstructionDocumentInternal {
    pub(crate) kind: InstructionDocumentKind,
    pub(crate) lines: Vec<String>,
    pub(crate) path: PathBuf,
}

/// Represents instruction search graph.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstructionSearchGraph {
    pub(crate) documents: HashMap<String, InstructionDocumentInternal>,
    pub(crate) incoming: HashMap<String, Vec<InstructionPathStep>>,
}

/// Represents the pick project root response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PickProjectRootResponse {
    pub(crate) path: Option<String>,
}

/// Defines the delta event variants.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionQueueDelta {
    pub(crate) pending_prompts: Vec<PendingPrompt>,
    pub(crate) queue_paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) queue_projection_hash: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum DeltaEvent {
    SessionCreated {
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        session: StateSessionSummary,
    },
    MessageCreated {
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_queue: Option<SessionQueueDelta>,
        #[serde(
            rename = "sessionMutationStamp",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_mutation_stamp: Option<u64>,
    },
    MessageUpdated {
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
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
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
        revision: u64,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "messageIndex")]
        message_index: usize,
        #[serde(rename = "messageCount")]
        message_count: u32,
        #[serde(rename = "textStartByte")]
        text_start_byte: usize,
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
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
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
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
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
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
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
    /// A test-run card updated in place from the index. `run` replaces the
    /// snapshot whole; the message's other fields never change.
    TestRunCardUpdated {
        #[serde(
            rename = "sessionSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        session_seq: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body_seq_epoch: Option<String>,
        revision: u64,
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        preview: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_mutation_stamp: Option<u64>,
        run: TestRunCardSnapshot,
    },
    ConversationMarkerCreated {
        revision: u64,
        session_id: String,
        marker: ConversationMarker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_mutation_stamp: Option<u64>,
    },
    ConversationMarkerUpdated {
        revision: u64,
        session_id: String,
        marker: ConversationMarker,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_mutation_stamp: Option<u64>,
    },
    ConversationMarkerDeleted {
        revision: u64,
        session_id: String,
        marker_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_mutation_stamp: Option<u64>,
    },
    /// Process-global Codex runtime metadata. This event intentionally carries
    /// the full, currently-small CodexState snapshot (rate limits plus capped
    /// notices) so clients replace the whole subsystem atomically instead of
    /// merging partial patches. Split this into narrower delta variants before
    /// adding independently-changing fields or payloads larger than runtime
    /// chrome.
    CodexUpdated { revision: u64, codex: CodexState },
    OrchestratorsUpdated {
        revision: u64,
        orchestrators: Vec<OrchestratorInstance>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sessions: Vec<StateSessionSummary>,
    },
    DelegationCreated {
        revision: u64,
        delegation: DelegationSummary,
    },
    DelegationWaitCreated {
        revision: u64,
        #[serde(default)]
        server_instance_id: String,
        wait: DelegationWaitRecord,
    },
    DelegationWaitConsumed {
        revision: u64,
        #[serde(default)]
        server_instance_id: String,
        wait_id: String,
        parent_session_id: String,
        reason: DelegationWaitConsumedReason,
    },
    DelegationWaitResumeDispatchFailed {
        revision: u64,
        parent_session_id: String,
        error: String,
    },
    DelegationUpdated {
        revision: u64,
        delegation_id: String,
        status: DelegationStatus,
        updated_at: String,
        /// The hold while `status` is `held`; null otherwise, so a client
        /// clears a hold it showed. Sent on every update, never omitted.
        #[serde(default)]
        hold: Option<DelegationHold>,
    },
    DelegationCompleted {
        revision: u64,
        delegation_id: String,
        result: DelegationResultSummary,
        completed_at: String,
    },
    DelegationFailed {
        revision: u64,
        delegation_id: String,
        result: DelegationResultSummary,
        failed_at: String,
    },
    DelegationCanceled {
        revision: u64,
        delegation_id: String,
        canceled_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A test run's summary or detail changed, or the run is new.
    TestRunChanged { revision: u64, run: TestRunSummary },
    /// A test run left the index.
    TestRunRemoved { revision: u64, run_id: String },
    /// Public run updates invalidate on-demand views without accumulating
    /// run summaries in every client. Activity is a complete replacement of
    /// running-session counters; absent sessions have zero activity.
    TestRunsInvalidated {
        revision: u64,
        server_instance_id: String,
        overview: TestRunOverviewSnapshot,
        activity: TestRunActivitiesSnapshot,
        #[cfg(test)]
        #[serde(skip)]
        internal: Option<Box<DeltaEvent>>,
    },
    /// A run wait was registered (`test_run_waits.rs`).
    TestRunWaitCreated {
        revision: u64,
        #[serde(default)]
        server_instance_id: String,
        wait: TestRunWaitRecord,
    },
    /// A run wait left the pending set.
    TestRunWaitConsumed {
        revision: u64,
        #[serde(default)]
        server_instance_id: String,
        wait_id: String,
        session_id: String,
        reason: TestRunWaitConsumedReason,
    },
    /// A resume queued by a run wait could not be dispatched.
    TestRunWaitResumeDispatchFailed {
        revision: u64,
        session_id: String,
        error: String,
        /// The server instance that failed to dispatch. The UI can then keep
        /// a failure a restarted server sends before its first snapshot.
        /// Defaults to empty when decoding a remote on an older build that
        /// omits it, so remote delta decoding does not fail.
        #[serde(default)]
        server_instance_id: String,
    },
}

impl DeltaEvent {
    pub(crate) fn revision(&self) -> u64 {
        match self {
            Self::SessionCreated { revision, .. }
            | Self::MessageCreated { revision, .. }
            | Self::MessageUpdated { revision, .. }
            | Self::TextDelta { revision, .. }
            | Self::TextReplace { revision, .. }
            | Self::CommandUpdate { revision, .. }
            | Self::ParallelAgentsUpdate { revision, .. }
            | Self::TestRunCardUpdated { revision, .. }
            | Self::ConversationMarkerCreated { revision, .. }
            | Self::ConversationMarkerUpdated { revision, .. }
            | Self::ConversationMarkerDeleted { revision, .. }
            | Self::CodexUpdated { revision, .. }
            | Self::OrchestratorsUpdated { revision, .. }
            | Self::DelegationCreated { revision, .. }
            | Self::DelegationWaitCreated { revision, .. }
            | Self::DelegationWaitConsumed { revision, .. }
            | Self::DelegationWaitResumeDispatchFailed { revision, .. }
            | Self::DelegationUpdated { revision, .. }
            | Self::DelegationCompleted { revision, .. }
            | Self::DelegationFailed { revision, .. }
            | Self::DelegationCanceled { revision, .. }
            | Self::TestRunChanged { revision, .. }
            | Self::TestRunRemoved { revision, .. }
            | Self::TestRunsInvalidated { revision, .. }
            | Self::TestRunWaitCreated { revision, .. }
            | Self::TestRunWaitConsumed { revision, .. }
            | Self::TestRunWaitResumeDispatchFailed { revision, .. } => *revision,
        }
    }
}
