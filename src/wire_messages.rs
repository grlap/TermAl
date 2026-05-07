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
    source: ParallelAgentSource,
    status: ParallelAgentStatus,
    title: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum ParallelAgentSource {
    Delegation,
    Tool,
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

    /// Returns a short single-line preview of the message for
    /// sidebar/header rendering. Different variants distill down
    /// differently: user/agent text falls back to a truncated
    /// excerpt, command/diff cards surface their command or file
    /// path, and parallel-agent messages call through to
    /// `parallel_agents_preview_text`. `None` means the variant
    /// has no meaningful preview text (placeholders, internal
    /// sync markers).
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
