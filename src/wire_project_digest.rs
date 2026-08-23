// Project digest types + project-status/progress text formatters.
//
// The `projects` dashboard UI renders a compact "digest" summary for
// each project: active action tiles, a pending-approval badge, and a
// done-summary hint. This file owns the DTOs plus the formatters that
// build their text from a compact SessionRecord projection / GitStatusResponse.

/// Temporary operational kill switch. Digest requests previously amplified
/// resource pressure into process-wide state-lock stalls, so both HTTP handlers
/// return 404 before touching `StateInner` while this remains false.
const PROJECT_DIGESTS_ENABLED: bool = false;

// DTOs
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
    /// Session id used by digest actions and deep links. For live approval,
    /// interaction, and active states this points at the session that produced
    /// the digest status. For error, dirty-worktree, and idle states it points
    /// at the latest non-delegation prompt target so follow-up actions continue
    /// in the parent session even when the summary came from another session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    primary_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    proposed_actions: Vec<ProjectDigestAction>,
    /// Project URL, optionally focused on `primarySessionId` when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deep_link: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_message_ids: Vec<String>,
}

/// Represents project digest inputs.
#[derive(Clone)]
struct ProjectDigestInputs {
    project: Project,
    sessions: Vec<ProjectDigestSession>,
}

/// Compact per-session facts needed by the project digest.
///
/// Keep this projection deliberately free of transcript bodies, runtime
/// handles, and pending-request maps: `project_digest_inputs` builds it while
/// holding the process-wide state mutex, and callers must inspect only a
/// bounded recent tail rather than deep-cloning or fully scanning a session
/// merely to render a small digest card.
#[derive(Clone)]
struct ProjectDigestSession {
    id: String,
    parent_delegation_id: Option<String>,
    status: SessionStatus,
    preview: String,
    pending_prompt_count: usize,
    has_messages: bool,
    latest_progress_summary: Option<(String, String)>,
    pending_approval_message_id: Option<String>,
    pending_interaction_message_id: Option<String>,
}

/// Maximum recent messages inspected while the process-wide state mutex is
/// held. Persisted idle sessions retain the same-sized tail; active sessions
/// may temporarily be much larger, so project polling must impose its own
/// fixed upper bound too.
const PROJECT_DIGEST_MESSAGE_SCAN_LIMIT: usize = SESSION_IN_MEMORY_MESSAGE_LIMIT;

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
    /// Parses a digest-action id from the URL path. Returns `None`
    /// for unknown ids so the route can 404 instead of panicking.
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

    /// Returns the prompt text that should be sent to the agent
    /// when the user triggers this digest action (or `None` for
    /// actions like Reject that don't send a prompt).
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

// Project text/status helpers
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
fn active_project_status_text(record: &ProjectDigestSession) -> String {
    let queued_count = record.pending_prompt_count;
    match queued_count {
        0 => "Agent is working.".to_owned(),
        1 => "Agent is working with 1 queued follow-up.".to_owned(),
        count => format!("Agent is working with {count} queued follow-ups."),
    }
}

/// Handles select project done summary.
fn select_project_done_summary(
    primary_session: Option<&ProjectDigestSession>,
    git_status: Option<&GitStatusResponse>,
    prefer_git: bool,
) -> (String, Vec<String>) {
    let message_summary = primary_session.and_then(|record| record.latest_progress_summary.clone());
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
fn default_project_done_summary(record: &ProjectDigestSession) -> String {
    if !record.has_messages {
        return "Ready for the next prompt.".to_owned();
    }
    let preview = record.preview.trim();
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
    sessions: &'a [ProjectDigestSession],
) -> Option<(&'a ProjectDigestSession, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .pending_approval_message_id
            .as_ref()
            .map(|message_id| (record, message_id.clone()))
    })
}

/// Returns whether live pending approval.
fn has_live_pending_approval(record: &SessionRecord, message_id: &str) -> bool {
    record.pending_claude_approvals.contains_key(message_id)
        || record.pending_codex_approvals.contains_key(message_id)
        || record.pending_acp_approvals.contains_key(message_id)
}

/// Selects the latest registered message without scanning transcript bodies.
/// Live local interactions are registered by message id and `message_positions`
/// is maintained with every transcript mutation, so this work is bounded by
/// the small number of simultaneous pending requests rather than transcript
/// length.
fn latest_registered_message<'a>(
    record: &SessionRecord,
    message_ids: impl Iterator<Item = &'a String>,
) -> Option<(usize, String)> {
    message_ids
        .filter_map(|message_id| {
            record
                .message_positions
                .get(message_id)
                .copied()
                .map(|position| (position, message_id))
        })
        .max_by_key(|(position, _)| *position)
        .map(|(position, message_id)| (position, message_id.clone()))
}

fn latest_registered_message_id<'a>(
    record: &SessionRecord,
    message_ids: impl Iterator<Item = &'a String>,
) -> Option<String> {
    latest_registered_message(record, message_ids).map(|(_, message_id)| message_id)
}

/// Resolves the latest renderable live local approval target from routing
/// registries. ACP uses FIFO resolution, so its queue head wins over later ACP
/// cards while it remains resident; the resulting ACP candidate is still
/// compared with newer Claude/Codex approvals.
fn registered_pending_approval_message_id(record: &SessionRecord) -> Option<String> {
    let acp_head = record
        .pending_acp_approval_order
        .front()
        .filter(|message_id| record.pending_acp_approvals.contains_key(*message_id))
        .and_then(|message_id| {
            record
                .message_positions
                .get(message_id)
                .copied()
                .map(|position| (position, message_id.clone()))
        });
    let acp_candidate = acp_head.or_else(|| {
        latest_registered_message(record, record.pending_acp_approvals.keys())
    });
    let other_candidate = latest_registered_message(
        record,
        record
            .pending_claude_approvals
            .keys()
            .chain(record.pending_codex_approvals.keys()),
    );

    [acp_candidate, other_candidate]
        .into_iter()
        .flatten()
        .max_by_key(|(position, _)| *position)
        .map(|(_, message_id)| message_id)
}

/// Resolves the latest live local nonapproval interaction from its routing
/// registries, independent of how deep the backing card is in the transcript.
fn registered_pending_interaction_message_id(record: &SessionRecord) -> Option<String> {
    latest_registered_message_id(
        record,
        record
            .pending_codex_user_inputs
            .keys()
            .chain(record.pending_claude_user_inputs.keys())
            .chain(record.pending_codex_mcp_elicitations.keys())
            .chain(record.pending_codex_app_requests.keys()),
    )
}

/// Finds latest project pending nonapproval interaction.
fn find_latest_project_pending_nonapproval_interaction<'a>(
    sessions: &'a [ProjectDigestSession],
) -> Option<(&'a ProjectDigestSession, String)> {
    sessions.iter().rev().find_map(|record| {
        record
            .pending_interaction_message_id
            .as_ref()
            .map(|message_id| (record, message_id.clone()))
    })
}

/// Projects one session into the bounded metadata needed by project digests.
fn project_digest_session_from_record(record: &SessionRecord) -> ProjectDigestSession {
    let mut latest_progress_summary = None;
    let mut pending_approval_message_id = registered_pending_approval_message_id(record);
    let mut pending_interaction_message_id = registered_pending_interaction_message_id(record);

    for message in record
        .session
        .messages
        .iter()
        .rev()
        .take(PROJECT_DIGEST_MESSAGE_SCAN_LIMIT)
    {
        if latest_progress_summary.is_none() {
            latest_progress_summary = project_progress_summary_for_message(message)
                .map(|summary| (message.id().to_owned(), summary));
        }
        if pending_approval_message_id.is_none() {
            if let Message::Approval { id, decision, .. } = message {
                if *decision == ApprovalDecision::Pending
                    && has_live_pending_approval(record, id)
                {
                    pending_approval_message_id = Some(id.clone());
                }
            }
        }
        if pending_interaction_message_id.is_none() {
            pending_interaction_message_id = match message {
                Message::UserInputRequest { id, state, .. }
                | Message::McpElicitationRequest { id, state, .. }
                | Message::CodexAppRequest { id, state, .. }
                    if *state == InteractionRequestState::Pending => Some(id.clone()),
                _ => None,
            };
        }
        if latest_progress_summary.is_some()
            && pending_approval_message_id.is_some()
            && pending_interaction_message_id.is_some()
        {
            break;
        }
    }

    ProjectDigestSession {
        id: record.session.id.clone(),
        parent_delegation_id: record.session.parent_delegation_id.clone(),
        status: record.session.status,
        preview: record.session.preview.clone(),
        pending_prompt_count: record.session.pending_prompts.len(),
        has_messages: !record.session.messages.is_empty(),
        latest_progress_summary,
        pending_approval_message_id,
        pending_interaction_message_id,
    }
}
