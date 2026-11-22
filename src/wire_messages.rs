// Session-scoped wire types: the `Message` enum + interaction
// DTOs + parallel-agent progress shapes.
//
// This is where the session transcript's actual payload shapes
// live. Every item that can appear in `SessionRecord.messages` is
// a variant of `Message`: user prompts, agent replies, streaming
// text placeholders, command + tool + diff cards, parallel-agent
// progress, approval requests, user-input questions, MCP
// elicitation requests, and errors.
//
// The sidebar / header UI picks up previews via the `impl Message`
// helpers (`preview_text`, `kind`, etc.) and `parallel_agents_preview_text`
// formats the collapsed "3 agents running, 1 done" summary for the
// parallel-agents variant.
//
// The approval / user-input / MCP-elicitation request payloads
// (`ApprovalRequest`, `UserInputSubmissionRequest`,
// `McpElicitationSubmissionRequest`) live here because they travel
// with the corresponding `Message` variants and share types like
// `UserInputQuestion` / `McpElicitationAction` /
// `InteractionRequestState`. The routes that consume them are in
// `api.rs`'s tail section (`submit_approval`, `submit_user_input`,
// `submit_mcp_elicitation`, `submit_codex_app_request`), but those
// routes are thin; all the vocabulary lives here.

use super::*;

/// Enumerates parallel agent states.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ParallelAgentStatus {
    Initializing,
    Running,
    Completed,
    Error,
}

/// Represents user input question option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserInputQuestionOption {
    /// Opaque protocol value when it differs from the displayed label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    pub(crate) description: String,
    pub(crate) label: String,
}

/// Represents user input question.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserInputQuestion {
    pub(crate) header: String,
    pub(crate) id: String,
    #[serde(default, rename = "isOther")]
    pub(crate) is_other: bool,
    #[serde(default, rename = "isSecret")]
    pub(crate) is_secret: bool,
    #[serde(default, rename = "multiSelect")]
    pub(crate) multi_select: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) options: Option<Vec<UserInputQuestionOption>>,
    pub(crate) question: String,
}

/// Enumerates MCP elicitation actions.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

/// Enumerates MCP elicitation request modes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub(crate) enum McpElicitationRequestMode {
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
pub(crate) struct McpElicitationRequestPayload {
    #[serde(rename = "threadId")]
    pub(crate) thread_id: String,
    #[serde(default, rename = "turnId", skip_serializing_if = "Option::is_none")]
    pub(crate) turn_id: Option<String>,
    pub(crate) server_name: String,
    #[serde(flatten)]
    pub(crate) mode: McpElicitationRequestMode,
}

/// Tracks interaction request state.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InteractionRequestState {
    Pending,
    Submitted,
    Interrupted,
    Canceled,
    /// The request was resolved without answers: either the user explicitly
    /// skipped a declinable card, or TermAl self-resolved an unattended
    /// question. Distinct from `Canceled`, which is an agent- or turn-side
    /// cancellation.
    Declined,
}

/// Represents parallel agent progress.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ParallelAgentProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    pub(crate) id: String,
    /// Discriminates which subsystem owns `id`. Delegation rows carry a
    /// TermAl delegation id that can be routed to delegation APIs; tool rows
    /// carry an opaque tool-use id and must not be used as delegation ids.
    pub(crate) source: ParallelAgentSource,
    pub(crate) status: ParallelAgentStatus,
    pub(crate) title: String,
}

/// Identifies the producer of a parallel-agent progress row.
///
/// Adding a variant is a wire-contract change: update all UI consumers and
/// routing gates so only delegation-owned rows expose delegation actions.
/// If this enum becomes public, restore `#[non_exhaustive]` for downstream
/// consumers.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ParallelAgentSource {
    /// TermAl delegation id, routable through delegation status/result/cancel commands.
    Delegation,
    /// Agent-runtime tool id, shown as progress only and not routable as a delegation.
    Tool,
}

/// Represents message image attachment.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageImageAttachment {
    pub(crate) byte_size: usize,
    pub(crate) file_name: String,
    pub(crate) media_type: String,
}

/// Structured provenance for text delivered through
/// `termal_send_to_session`. Ordinary peer messages identify one backend-
/// resolved sender; a peer batch is marked explicitly and may omit
/// `session_id` when it contains multiple senders. Absent (`None`) for
/// ordinary human/agent messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "MessageSourceKind::is_peer")]
    pub(crate) kind: MessageSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mailbox: Option<MailboxMessageSource>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MessageSourceKind {
    #[default]
    Peer,
    PeerBatch,
    Mailbox,
}

impl MessageSourceKind {
    pub(crate) fn is_peer(&self) -> bool {
        *self == Self::Peer
    }
}

impl MessageSource {
    pub(crate) fn peer(session_id: String, name: String) -> Self {
        Self {
            session_id: Some(session_id),
            name,
            kind: MessageSourceKind::Peer,
            mailbox: None,
        }
    }

    pub(crate) fn mailbox(session_id: String, name: String, mailbox: MailboxMessageSource) -> Self {
        Self {
            session_id: Some(session_id),
            name,
            kind: MessageSourceKind::Mailbox,
            mailbox: Some(mailbox),
        }
    }

    pub(crate) fn peer_batch(common_source: Option<&Self>) -> Self {
        Self {
            session_id: common_source.and_then(|source| source.session_id.clone()),
            name: "Peer queue".to_owned(),
            kind: MessageSourceKind::PeerBatch,
            mailbox: None,
        }
    }

    pub(crate) fn is_peer(&self) -> bool {
        self.kind == MessageSourceKind::Peer
    }

    pub(crate) fn is_peer_batch(&self) -> bool {
        self.kind == MessageSourceKind::PeerBatch
    }

    pub(crate) fn is_mailbox(&self) -> bool {
        self.kind == MessageSourceKind::Mailbox
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MailboxMessageSource {
    pub(crate) mailbox_id: String,
    pub(crate) message_id: String,
    pub(crate) sequence: u64,
    pub(crate) unread_count: u64,
}

/// Represents pending prompt.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingPrompt {
    /// Derived queue recovery disposition, never part of the authorization input.
    #[serde(default, skip_serializing_if = "session_flag_is_false")]
    pub(crate) engram_interrupted: bool,
    /// Exact derived retained-authorization fact. Queue pausing alone does not
    /// imply Engram ownership of this prompt.
    #[serde(default, skip_serializing_if = "session_flag_is_false")]
    pub(crate) is_engram_retained: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) attachments: Vec<MessageImageAttachment>,
    pub(crate) id: String,
    pub(crate) timestamp: String,
    pub(crate) text: String,
    #[serde(
        default,
        rename = "expandedText",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) expanded_text: Option<String>,
    /// Peer sender when this queued prompt arrived via `termal_send_to_session`;
    /// carried so a prompt queued while the target was busy keeps its
    /// attribution when it later becomes a `Message::Text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<MessageSource>,
}

/// Defines the message variants.
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Message {
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
        /// Set when this text arrived from another session via
        /// `termal_send_to_session`; drives the UI's sender label instead of
        /// "You". `None` for ordinary human/agent text (backward-compatible:
        /// old transcripts deserialize to `None`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<MessageSource>,
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
    /// A test run as a specialized tool execution, created and updated in
    /// place by the host from the test-run index (`test_run_cards.rs`).
    #[serde(rename = "testRun")]
    TestRun {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
        run: TestRunCardSnapshot,
    },
    #[serde(rename = "fileChanges")]
    FileChanges {
        id: String,
        timestamp: String,
        author: Author,
        title: String,
        files: Vec<FileChangeSummaryEntry>,
    },
    #[serde(rename = "engramControl")]
    EngramControl {
        id: String,
        timestamp: String,
        author: Author,
        #[serde(flatten)]
        card: EngramControlCard,
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
        #[serde(
            default,
            rename = "supportedDecisions",
            skip_serializing_if = "Option::is_none"
        )]
        supported_decisions: Option<Vec<ApprovalDecision>>,
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
        /// Whether the browser may skip this request without answers. Pending
        /// Claude and Kimi permission questions are declinable — the owning
        /// protocol carries the skip back to the agent. Codex
        /// `request_user_input` and already self-resolved audit cards are not.
        /// Always serialized (the backend owns the flag; `default` only
        /// covers payloads persisted before it existed) so the UI treats it
        /// as a required boolean.
        #[serde(default)]
        declinable: bool,
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
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Text { id, .. }
            | Self::Thinking { id, .. }
            | Self::Command { id, .. }
            | Self::Diff { id, .. }
            | Self::Markdown { id, .. }
            | Self::SubagentResult { id, .. }
            | Self::ParallelAgents { id, .. }
            | Self::TestRun { id, .. }
            | Self::FileChanges { id, .. }
            | Self::EngramControl { id, .. }
            | Self::Approval { id, .. }
            | Self::UserInputRequest { id, .. }
            | Self::McpElicitationRequest { id, .. }
            | Self::CodexAppRequest { id, .. } => id,
        }
    }

    pub(crate) fn is_interaction_request(&self) -> bool {
        matches!(
            self,
            Self::Approval { .. }
                | Self::UserInputRequest { .. }
                | Self::McpElicitationRequest { .. }
                | Self::CodexAppRequest { .. }
        )
    }

    /// An interaction request that is still waiting for someone. A card
    /// recorded in an already-resolved state (for example an unattended
    /// AskUserQuestion TermAl self-resolved) is an interaction request but
    /// not a pending one, and must not park the session in Approval.
    pub(crate) fn is_pending_interaction_request(&self) -> bool {
        matches!(
            self,
            Self::Approval {
                decision: ApprovalDecision::Pending,
                ..
            } | Self::UserInputRequest {
                state: InteractionRequestState::Pending,
                ..
            } | Self::McpElicitationRequest {
                state: InteractionRequestState::Pending,
                ..
            } | Self::CodexAppRequest {
                state: InteractionRequestState::Pending,
                ..
            }
        )
    }

    pub(crate) fn user_prompt_text(&self) -> Option<&str> {
        match self {
            Self::Text {
                author: Author::You,
                text,
                ..
            } => Some(text),
            _ => None,
        }
    }

    pub(crate) fn pending_interaction_request(&self) -> Option<PendingInteractionRequest<'_>> {
        match self {
            Self::Approval {
                title,
                detail,
                decision: ApprovalDecision::Pending,
                ..
            } => Some(PendingInteractionRequest {
                child_waiting_prefix: "Child session is waiting for approval",
                title,
                detail,
            }),
            Self::UserInputRequest {
                title,
                detail,
                state: InteractionRequestState::Pending,
                ..
            } => Some(PendingInteractionRequest {
                child_waiting_prefix: "Child session is waiting for input",
                title,
                detail,
            }),
            Self::McpElicitationRequest {
                title,
                detail,
                state: InteractionRequestState::Pending,
                ..
            } => Some(PendingInteractionRequest {
                child_waiting_prefix: "Child session is waiting for MCP input",
                title,
                detail,
            }),
            Self::CodexAppRequest {
                title,
                detail,
                state: InteractionRequestState::Pending,
                ..
            } => Some(PendingInteractionRequest {
                child_waiting_prefix: "Child session is waiting for a Codex response",
                title,
                detail,
            }),
            _ => None,
        }
    }

    /// Returns a short single-line preview of the message for
    /// sidebar/header rendering. Different variants distill down
    /// differently: user/agent text falls back to a truncated
    /// excerpt, command/diff cards surface their command or file
    /// path, and parallel-agent messages call through to
    /// `parallel_agents_preview_text`. `None` means the variant
    /// has no meaningful preview text (placeholders, internal
    /// sync markers).
    pub(crate) fn preview_text(&self) -> Option<String> {
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
            Self::EngramControl { card, .. } => Some(make_preview(&format!(
                "Engram {:?}: {:?}",
                card.stage, card.decision
            ))),
            Self::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
            Self::TestRun { run, .. } => Some(test_run_card_preview_text(run)),
            Self::Command { .. } => None,
        }
    }
}

pub(crate) struct PendingInteractionRequest<'a> {
    pub(crate) child_waiting_prefix: &'static str,
    pub(crate) title: &'a str,
    pub(crate) detail: &'a str,
}

pub(crate) fn parallel_agents_preview_text(agents: &[ParallelAgentProgress]) -> String {
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
