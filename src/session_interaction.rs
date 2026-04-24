// Per-SessionRecord mutation helpers — approvals, user input, MCP
// elicitation, Codex app requests, queued prompts, and the preview-text
// projections that show the user what's happening in each session.
//
// These helpers all take `&mut SessionRecord` (or `&SessionRecord` for
// read-only projections) and encapsulate the invariants for a single
// session's in-memory state: how a pending approval moves through
// Pending/Accepted/Rejected, how queued prompts get FIFO'd with user
// prompts prioritized over orchestrator work, how the latest interaction
// preview string is computed for the sidebar, and how records get reset
// for Claude spare-pool reuse.
//
// Covers:
// - Claude spare-pool reset: `reset_hidden_claude_spare_record`,
//   `claude_spare_profile`
// - Pending-request housekeeping: `has_pending_requests`,
//   `clear_all_pending_requests`
// - Agent command merge/dedupe: `merge_agent_commands`, `dedupe_agent_commands`
// - Codex thread state: `normalized_codex_thread_state`,
//   `sync_codex_thread_state`, `set_record_codex_thread_state`,
//   `set_record_external_session_id`, `record_has_archived_codex_thread`
// - Prompt-queue sync: `sync_pending_prompts`
// - Interaction state transitions: `set_approval_decision_on_record`,
//   `set_user_input_request_state_on_record`,
//   `set_mcp_elicitation_request_state_on_record`,
//   `set_codex_app_request_state_on_record`
// - Preview-text projections: `latest_pending_interaction_preview`,
//   `approval_preview_text`, `user_input_request_preview_text`,
//   `mcp_elicitation_request_preview_text`, `codex_app_request_preview_text`,
//   `sync_session_interaction_state`
// - Queue mutations: `queue_prompt_on_record`,
//   `queue_orchestrator_prompt_on_record`,
//   `queue_prompt_on_record_with_source`,
//   `prioritize_user_queued_prompts`,
//   `clear_queued_prompts_by_source`,
//   `clear_stopped_orchestrator_queued_prompts`
//
// Extracted from state.rs so state.rs can stay focused on `StateInner`
// + commit_locked() + SSE broadcasting.

/// Handles reset hidden Claude spare record.
fn reset_hidden_claude_spare_record(record: &mut SessionRecord) {
    if record.session.agent != Agent::Claude {
        return;
    }

    record.session.messages.clear();
    record.session.pending_prompts.clear();
    record.session.status = SessionStatus::Idle;
    record.session.preview = "Ready for a prompt.".to_owned();
    clear_all_pending_requests(record);
    record.queued_prompts.clear();
    record.message_positions.clear();
    record.runtime_reset_required = false;
    record.orchestrator_auto_dispatch_blocked = false;
    record.runtime_stop_in_progress = false;
    record.deferred_stop_callbacks.clear();
    clear_active_turn_file_change_tracking(record);
}

/// Returns whether pending requests.
fn has_pending_requests(record: &SessionRecord) -> bool {
    !record.pending_claude_approvals.is_empty()
        || !record.pending_codex_approvals.is_empty()
        || !record.pending_codex_user_inputs.is_empty()
        || !record.pending_codex_mcp_elicitations.is_empty()
        || !record.pending_codex_app_requests.is_empty()
        || !record.pending_acp_approvals.is_empty()
}

/// Clears all pending requests.
fn clear_all_pending_requests(record: &mut SessionRecord) {
    record.pending_claude_approvals.clear();
    record.pending_codex_approvals.clear();
    record.pending_codex_user_inputs.clear();
    record.pending_codex_mcp_elicitations.clear();
    record.pending_codex_app_requests.clear();
    record.pending_acp_approvals.clear();
}

/// Merges agent commands.
fn merge_agent_commands(
    preferred: &[AgentCommand],
    fallback: &[AgentCommand],
) -> Vec<AgentCommand> {
    if preferred.is_empty() {
        return dedupe_agent_commands(fallback.to_vec());
    }
    if fallback.is_empty() {
        return dedupe_agent_commands(preferred.to_vec());
    }

    let mut commands = preferred.to_vec();
    commands.extend(fallback.iter().cloned());
    dedupe_agent_commands(commands)
}

/// Deduplicates agent commands.
fn dedupe_agent_commands(commands: Vec<AgentCommand>) -> Vec<AgentCommand> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for command in commands {
        let key = command.name.trim().to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(command);
    }
    deduped.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    deduped
}

/// Handles Claude spare profile.
fn claude_spare_profile(
    record: &SessionRecord,
) -> (
    String,
    Option<String>,
    String,
    ClaudeApprovalMode,
    ClaudeEffortLevel,
) {
    (
        record.session.workdir.clone(),
        record.session.project_id.clone(),
        record.session.model.clone(),
        record
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode),
        record
            .session
            .claude_effort
            .unwrap_or_else(default_claude_effort),
    )
}

/// Returns the normalized Codex thread state.
fn normalized_codex_thread_state(
    agent: Agent,
    external_session_id: Option<&str>,
    current_state: Option<CodexThreadState>,
) -> Option<CodexThreadState> {
    if !agent.supports_codex_prompt_settings() || external_session_id.is_none() {
        return None;
    }

    Some(current_state.unwrap_or(CodexThreadState::Active))
}

/// Syncs Codex thread state.
fn sync_codex_thread_state(record: &mut SessionRecord) {
    record.session.codex_thread_state = normalized_codex_thread_state(
        record.session.agent,
        record.external_session_id.as_deref(),
        record.session.codex_thread_state,
    );
}

/// Sets record external session ID.
fn set_record_external_session_id(record: &mut SessionRecord, external_session_id: Option<String>) {
    record.external_session_id = external_session_id.clone();
    record.session.external_session_id = external_session_id;
    sync_codex_thread_state(record);
}

/// Sets record Codex thread state.
fn set_record_codex_thread_state(record: &mut SessionRecord, thread_state: CodexThreadState) {
    record.session.codex_thread_state = normalized_codex_thread_state(
        record.session.agent,
        record.external_session_id.as_deref(),
        Some(thread_state),
    );
}

/// Records has archived Codex thread.
fn record_has_archived_codex_thread(record: &SessionRecord) -> bool {
    record.session.codex_thread_state == Some(CodexThreadState::Archived)
}

/// Defines the queued prompt source variants.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum QueuedPromptSource {
    #[default]
    User,
    Orchestrator,
}

/// Represents a queued prompt record.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct QueuedPromptRecord {
    source: QueuedPromptSource,
    attachments: Vec<PromptImageAttachment>,
    pending_prompt: PendingPrompt,
}

/// Syncs pending prompts.
fn sync_pending_prompts(record: &mut SessionRecord) {
    if record.is_remote_proxy() {
        return;
    }
    record.session.pending_prompts = record
        .queued_prompts
        .iter()
        .map(|queued| queued.pending_prompt.clone())
        .collect();
}

/// Sets approval decision on record.
fn set_approval_decision_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    decision: ApprovalDecision,
) -> Result<usize> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("approval message `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("approval message `{message_id}` not found"));
    };
    match message {
        Message::Approval {
            id,
            decision: current,
            ..
        } if id == message_id => {
            *current = decision;
            Ok(message_index)
        }
        _ => Err(anyhow!("approval message `{message_id}` not found")),
    }
}

/// Sets user input request state on record.
fn set_user_input_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_answers: Option<BTreeMap<String, Vec<String>>>,
) -> Result<usize> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("user input request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("user input request `{message_id}` not found"));
    };
    match message {
        Message::UserInputRequest {
            id,
            state: current_state,
            submitted_answers: current_answers,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_answers = submitted_answers;
            Ok(message_index)
        }
        _ => Err(anyhow!("user input request `{message_id}` not found")),
    }
}

/// Sets MCP elicitation request state on record.
fn set_mcp_elicitation_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_action: Option<McpElicitationAction>,
    submitted_content: Option<Value>,
) -> Result<usize> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("MCP elicitation request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("MCP elicitation request `{message_id}` not found"));
    };
    match message {
        Message::McpElicitationRequest {
            id,
            state: current_state,
            submitted_action: current_action,
            submitted_content: current_content,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_action = submitted_action;
            *current_content = submitted_content;
            Ok(message_index)
        }
        _ => Err(anyhow!("MCP elicitation request `{message_id}` not found")),
    }
}

/// Sets Codex app request state on record.
fn set_codex_app_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_result: Option<Value>,
) -> Result<usize> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("Codex app request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("Codex app request `{message_id}` not found"));
    };
    match message {
        Message::CodexAppRequest {
            id,
            state: current_state,
            submitted_result: current_result,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_result = submitted_result;
            Ok(message_index)
        }
        _ => Err(anyhow!("Codex app request `{message_id}` not found")),
    }
}

/// Returns the latest pending interaction preview.
fn latest_pending_interaction_preview(record: &SessionRecord) -> Option<String> {
    for message in record.session.messages.iter().rev() {
        match message {
            Message::Approval {
                decision: ApprovalDecision::Pending,
                ..
            } => {
                return Some(approval_preview_text(
                    record.session.agent.name(),
                    ApprovalDecision::Pending,
                ));
            }
            Message::UserInputRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(user_input_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                ));
            }
            Message::McpElicitationRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(mcp_elicitation_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                    None,
                ));
            }
            Message::CodexAppRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(codex_app_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                ));
            }
            _ => {}
        }
    }

    None
}

fn approval_preview_text(agent_name: &str, decision: ApprovalDecision) -> String {
    match decision {
        ApprovalDecision::Pending => "Approval pending.".to_owned(),
        ApprovalDecision::Interrupted => "Approval expired after TermAl restarted.".to_owned(),
        ApprovalDecision::Canceled => {
            format!("Approval canceled. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::Accepted => {
            format!("Approval granted. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::AcceptedForSession => {
            format!("Approval granted for this session. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::Rejected => {
            format!("Approval rejected. {agent_name} is continuing\u{2026}")
        }
    }
}

fn user_input_request_preview_text(agent_name: &str, state: InteractionRequestState) -> String {
    match state {
        InteractionRequestState::Pending => "Input requested.".to_owned(),
        InteractionRequestState::Submitted => {
            format!("Input submitted. {agent_name} is continuing\u{2026}")
        }
        InteractionRequestState::Interrupted => {
            "Input request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("Input request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

/// Handles MCP elicitation request preview text.
fn mcp_elicitation_request_preview_text(
    agent_name: &str,
    state: InteractionRequestState,
    action: Option<McpElicitationAction>,
) -> String {
    match state {
        InteractionRequestState::Pending => "MCP input requested.".to_owned(),
        InteractionRequestState::Submitted => {
            match action.unwrap_or(McpElicitationAction::Accept) {
                McpElicitationAction::Accept => {
                    format!("MCP input submitted. {agent_name} is continuing\u{2026}")
                }
                McpElicitationAction::Decline => {
                    format!("MCP request declined. {agent_name} is continuing\u{2026}")
                }
                McpElicitationAction::Cancel => {
                    format!("MCP request canceled. {agent_name} is continuing\u{2026}")
                }
            }
        }
        InteractionRequestState::Interrupted => {
            "MCP input request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("MCP input request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

/// Handles Codex app request preview text.
fn codex_app_request_preview_text(agent_name: &str, state: InteractionRequestState) -> String {
    match state {
        InteractionRequestState::Pending => "Codex response requested.".to_owned(),
        InteractionRequestState::Submitted => {
            format!("Codex response submitted. {agent_name} is continuing\u{2026}")
        }
        InteractionRequestState::Interrupted => {
            "Codex request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("Codex request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

/// Syncs session interaction state.
fn sync_session_interaction_state(record: &mut SessionRecord, resolved_preview: String) {
    if let Some(preview) = latest_pending_interaction_preview(record) {
        record.session.status = SessionStatus::Approval;
        record.session.preview = preview;
        return;
    }

    if matches!(
        record.session.status,
        SessionStatus::Approval | SessionStatus::Active
    ) {
        record.session.status = SessionStatus::Active;
        record.session.preview = resolved_preview;
    }
}


/// Queues prompt on record.
fn queue_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) {
    queue_prompt_on_record_with_source(
        record,
        pending_prompt,
        attachments,
        QueuedPromptSource::User,
    );
}

/// Queues orchestrator prompt on record.
fn queue_orchestrator_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) {
    queue_prompt_on_record_with_source(
        record,
        pending_prompt,
        attachments,
        QueuedPromptSource::Orchestrator,
    );
}

/// Queues prompt on record with source.
fn queue_prompt_on_record_with_source(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
    source: QueuedPromptSource,
) {
    record.queued_prompts.push_back(QueuedPromptRecord {
        source,
        attachments,
        pending_prompt,
    });
    sync_pending_prompts(record);
}

/// Handles prioritize user queued prompts.
fn prioritize_user_queued_prompts(record: &mut SessionRecord) {
    let mut user_prompts = VecDeque::new();
    let mut deferred_orchestrator_prompts = VecDeque::new();

    while let Some(queued) = record.queued_prompts.pop_front() {
        if queued.source == QueuedPromptSource::User {
            user_prompts.push_back(queued);
        } else {
            deferred_orchestrator_prompts.push_back(queued);
        }
    }

    user_prompts.append(&mut deferred_orchestrator_prompts);
    record.queued_prompts = user_prompts;
    sync_pending_prompts(record);
}

/// Clears queued prompts by source.
fn clear_queued_prompts_by_source(record: &mut SessionRecord, source: QueuedPromptSource) {
    let original_len = record.queued_prompts.len();
    record
        .queued_prompts
        .retain(|queued| queued.source != source);
    if record.queued_prompts.len() != original_len {
        sync_pending_prompts(record);
    }
}

/// Clears stopped orchestrator queued prompts.
fn clear_stopped_orchestrator_queued_prompts(record: &mut SessionRecord) {
    clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
}
