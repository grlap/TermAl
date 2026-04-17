/*
Turn execution and event recording
agent stdout/stdin
  -> runtime-specific parser
  -> TurnRecorder callbacks
  -> SessionRecorder
  -> AppState mutations
  -> persistence + SSE deltas
The REPL path and the server path share the same recorder vocabulary so each
agent integration only needs one normalization layer.
*/

/// Runs a blocking turn for the selected agent.
fn run_turn_blocking(config: TurnConfig, recorder: &mut dyn TurnRecorder) -> Result<String> {
    match config.agent {
        Agent::Codex => run_codex_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            config
                .codex_sandbox_mode
                .unwrap_or_else(default_codex_sandbox_mode),
            config
                .codex_approval_policy
                .unwrap_or_else(default_codex_approval_policy),
            config
                .codex_reasoning_effort
                .unwrap_or_else(default_codex_reasoning_effort),
            &config.prompt,
            recorder,
        ),
        Agent::Claude => run_claude_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            config
                .claude_approval_mode
                .unwrap_or_else(default_claude_approval_mode),
            config.claude_effort.unwrap_or_else(default_claude_effort),
            &config.prompt,
            recorder,
        ),
        Agent::Cursor => run_acp_turn(
            AcpAgent::Cursor,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            &config.prompt,
            recorder,
        ),
        Agent::Gemini => run_acp_turn(
            AcpAgent::Gemini,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            &config.prompt,
            recorder,
        ),
    }
}

/// Defines behavior for turn recorder.
trait TurnRecorder {
    /// Records external session.
    fn note_external_session(&mut self, session_id: &str) -> Result<()>;
    /// Pushes approval.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()>;
    /// Pushes text.
    fn push_text(&mut self, text: &str) -> Result<()>;
    /// Pushes subagent result.
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()>;
    /// Pushes thinking.
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()>;
    /// Pushes diff.
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()>;
    /// Handles text delta.
    fn text_delta(&mut self, delta: &str) -> Result<()>;
    /// Replaces streaming text.
    fn replace_streaming_text(&mut self, text: &str) -> Result<()>;
    /// Finishes streaming text.
    fn finish_streaming_text(&mut self) -> Result<()>;
    /// Resets per-turn recorder state.
    fn reset_turn_state(&mut self) -> Result<()> {
        self.finish_streaming_text()
    }
    /// Handles command started.
    fn command_started(&mut self, key: &str, command: &str) -> Result<()>;
    /// Handles command completed.
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()>;
    /// Upserts parallel agents.
    fn upsert_parallel_agents(&mut self, key: &str, agents: &[ParallelAgentProgress])
    -> Result<()>;
    /// Handles error.
    fn error(&mut self, detail: &str) -> Result<()>;
}

/// Defines behavior for Codex turn recorder.
trait CodexTurnRecorder: TurnRecorder {
    /// Pushes Codex approval.
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()>;

    /// Pushes Codex user input request.
    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()>;

    /// Pushes Codex MCP elicitation request.
    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()>;

    /// Pushes Codex app request.
    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()>;
}

/// Represents session recorder.
struct SessionRecorder {
    recorder_state: SessionRecorderState,
    session_id: String,
    state: AppState,
}

impl SessionRecorder {
    /// Creates a new instance.
    fn new(state: AppState, session_id: String) -> Self {
        Self {
            recorder_state: SessionRecorderState::default(),
            session_id,
            state,
        }
    }

    /// Pushes Claude approval.
    fn push_claude_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        recorder_push_pending_approval(
            self,
            title,
            command,
            detail,
            approval,
            |state, session_id, message_id, approval| {
                state.register_claude_pending_approval(session_id, message_id, approval)
            },
        )
    }

    /// Pushes ACP approval.
    fn push_acp_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        recorder_push_pending_approval(
            self,
            title,
            command,
            detail,
            approval,
            |state, session_id, message_id, approval| {
                state.register_acp_pending_approval(session_id, message_id, approval)
            },
        )
    }
}

/// Defines behavior for session recorder access.
trait SessionRecorderAccess {
    /// Handles state.
    fn state(&self) -> &AppState;
    /// Handles session ID.
    fn session_id(&self) -> &str;
    /// Handles recorder state mut.
    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState;
}

impl SessionRecorderAccess for SessionRecorder {
    /// Handles state.
    fn state(&self) -> &AppState {
        &self.state
    }

    /// Handles session ID.
    fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Handles recorder state mut.
    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState {
        &mut self.recorder_state
    }
}

/// Handles recorder push pending approval.
fn recorder_push_pending_approval<R, T, F>(
    recorder: &mut R,
    title: &str,
    command: &str,
    detail: &str,
    pending: T,
    register: F,
) -> Result<()>
where
    R: SessionRecorderAccess,
    F: FnOnce(&AppState, &str, String, T) -> Result<()>,
{
    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = state.allocate_message_id();
    state.push_message(
        &session_id,
        Message::Approval {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            command: command.to_owned(),
            command_language: Some(shell_language().to_owned()),
            detail: detail.to_owned(),
            decision: ApprovalDecision::Pending,
        },
    )?;
    register(&state, &session_id, message_id, pending)
}

/// Handles recorder push Codex user input request.
fn recorder_push_codex_user_input_request<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    detail: &str,
    questions: Vec<UserInputQuestion>,
    request: CodexPendingUserInput,
) -> Result<()> {
    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = state.allocate_message_id();
    state.push_message(
        &session_id,
        Message::UserInputRequest {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            detail: detail.to_owned(),
            questions,
            state: InteractionRequestState::Pending,
            submitted_answers: None,
        },
    )?;
    state.register_codex_pending_user_input(&session_id, message_id, request)
}

/// Handles recorder push Codex MCP elicitation request.
fn recorder_push_codex_mcp_elicitation_request<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    detail: &str,
    request: McpElicitationRequestPayload,
    pending: CodexPendingMcpElicitation,
) -> Result<()> {
    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = state.allocate_message_id();
    state.push_message(
        &session_id,
        Message::McpElicitationRequest {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            detail: detail.to_owned(),
            request,
            state: InteractionRequestState::Pending,
            submitted_action: None,
            submitted_content: None,
        },
    )?;
    state.register_codex_pending_mcp_elicitation(&session_id, message_id, pending)
}

/// Handles recorder push Codex app request.
fn recorder_push_codex_app_request<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    detail: &str,
    method: &str,
    params: Value,
    pending: CodexPendingAppRequest,
) -> Result<()> {
    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = state.allocate_message_id();
    state.push_message(
        &session_id,
        Message::CodexAppRequest {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            detail: detail.to_owned(),
            method: method.to_owned(),
            params,
            state: InteractionRequestState::Pending,
            submitted_result: None,
        },
    )?;
    state.register_codex_pending_app_request(&session_id, message_id, pending)
}

/// Handles recorder note external session.
fn recorder_note_external_session<R: SessionRecorderAccess>(
    recorder: &mut R,
    session_id: &str,
) -> Result<()> {
    let state = recorder.state().clone();
    let current_session_id = recorder.session_id().to_owned();
    state.set_external_session_id(&current_session_id, session_id.to_owned())
}

/// Handles recorder push approval.
fn recorder_push_approval<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    command: &str,
    detail: &str,
) -> Result<()> {
    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    state.push_message(
        &session_id,
        Message::Approval {
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            command: command.to_owned(),
            command_language: Some(shell_language().to_owned()),
            detail: detail.to_owned(),
            decision: ApprovalDecision::Pending,
        },
    )
}

/// Handles recorder push text.
fn recorder_push_text<R: SessionRecorderAccess>(recorder: &mut R, text: &str) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    state.push_message(
        &session_id,
        Message::Text {
            attachments: Vec::new(),
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: trimmed.to_owned(),
            expanded_text: None,
        },
    )
}

/// Handles recorder push subagent result.
fn recorder_push_subagent_result<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    summary: &str,
    conversation_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<()> {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    state.push_message(
        &session_id,
        Message::SubagentResult {
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            summary: trimmed.to_owned(),
            conversation_id: conversation_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
        },
    )
}

/// Handles recorder text delta.
fn recorder_text_delta<R: SessionRecorderAccess>(recorder: &mut R, delta: &str) -> Result<()> {
    if delta.is_empty() {
        return Ok(());
    }

    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = match recorder
        .recorder_state_mut()
        .streaming_text_message_id
        .clone()
    {
        Some(message_id) => message_id,
        None => {
            let message_id = state.allocate_message_id();
            state.push_message(
                &session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id.clone(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: String::new(),
                    expanded_text: None,
                },
            )?;
            recorder.recorder_state_mut().streaming_text_message_id = Some(message_id.clone());
            message_id
        }
    };

    state.append_text_delta(&session_id, &message_id, delta)
}

/// Handles recorder replace streaming text.
fn recorder_replace_streaming_text<R: SessionRecorderAccess>(
    recorder: &mut R,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = match recorder
        .recorder_state_mut()
        .streaming_text_message_id
        .clone()
    {
        Some(message_id) => message_id,
        None => {
            return state.push_message(
                &session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: state.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: trimmed.to_owned(),
                    expanded_text: None,
                },
            );
        }
    };

    state.replace_text_message(&session_id, &message_id, trimmed)
}

/// Handles recorder push thinking.
fn recorder_push_thinking<R: SessionRecorderAccess>(
    recorder: &mut R,
    title: &str,
    lines: Vec<String>,
) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    state.push_message(
        &session_id,
        Message::Thinking {
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: title.to_owned(),
            lines,
        },
    )
}

/// Handles recorder push diff.
fn recorder_push_diff<R: SessionRecorderAccess>(
    recorder: &mut R,
    file_path: &str,
    summary: &str,
    diff: &str,
    change_type: ChangeType,
) -> Result<()> {
    if diff.trim().is_empty() {
        return Ok(());
    }

    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let message_id = state.allocate_message_id();
    state.push_message(
        &session_id,
        Message::Diff {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            change_set_id: Some(diff_change_set_id(&message_id)),
            file_path: file_path.to_owned(),
            summary: summary.to_owned(),
            diff: diff.to_owned(),
            language: Some("diff".to_owned()),
            change_type,
        },
    )
}

/// Handles recorder finish streaming text.
fn recorder_finish_streaming_text<R: SessionRecorderAccess>(recorder: &mut R) -> Result<()> {
    recorder.recorder_state_mut().streaming_text_message_id = None;
    Ok(())
}

/// Resets per-turn recorder state.
fn recorder_reset_turn_state<R: SessionRecorderAccess>(recorder: &mut R) -> Result<()> {
    reset_recorder_state_fields(recorder.recorder_state_mut());
    Ok(())
}

/// Handles recorder command started.
fn recorder_command_started<R: SessionRecorderAccess>(
    recorder: &mut R,
    key: &str,
    command: &str,
) -> Result<()> {
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let state_for_entry = state.clone();
    let message_id = recorder
        .recorder_state_mut()
        .command_messages
        .entry(key.to_owned())
        .or_insert_with(|| state_for_entry.allocate_message_id())
        .clone();

    state.upsert_command_message(
        &session_id,
        &message_id,
        command,
        "",
        CommandStatus::Running,
    )
}

/// Handles recorder command completed.
fn recorder_command_completed<R: SessionRecorderAccess>(
    recorder: &mut R,
    key: &str,
    command: &str,
    output: &str,
    status: CommandStatus,
) -> Result<()> {
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let state_for_entry = state.clone();
    let message_id = recorder
        .recorder_state_mut()
        .command_messages
        .entry(key.to_owned())
        .or_insert_with(|| state_for_entry.allocate_message_id())
        .clone();

    state.upsert_command_message(&session_id, &message_id, command, output, status)
}

/// Handles recorder upsert parallel agents.
fn recorder_upsert_parallel_agents<R: SessionRecorderAccess>(
    recorder: &mut R,
    key: &str,
    agents: &[ParallelAgentProgress],
) -> Result<()> {
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    let state_for_entry = state.clone();
    let message_id = recorder
        .recorder_state_mut()
        .parallel_agents_messages
        .entry(key.to_owned())
        .or_insert_with(|| state_for_entry.allocate_message_id())
        .clone();

    state.upsert_parallel_agents_message(&session_id, &message_id, agents.to_vec())
}

/// Handles recorder error.
fn recorder_error<R: SessionRecorderAccess>(recorder: &mut R, detail: &str) -> Result<()> {
    let cleaned = detail.trim();
    if cleaned.is_empty() {
        return Ok(());
    }

    recorder_finish_streaming_text(recorder)?;
    let state = recorder.state().clone();
    let session_id = recorder.session_id().to_owned();
    state.push_message(
        &session_id,
        Message::Text {
            attachments: Vec::new(),
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: format!("Error: {cleaned}"),
            expanded_text: None,
        },
    )
}

impl CodexTurnRecorder for SessionRecorder {
    /// Pushes Codex approval.
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        recorder_push_pending_approval(
            self,
            title,
            command,
            detail,
            approval,
            |state, session_id, message_id, approval| {
                state.register_codex_pending_approval(session_id, message_id, approval)
            },
        )
    }

    /// Pushes Codex user input request.
    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        recorder_push_codex_user_input_request(self, title, detail, questions, request)
    }

    /// Pushes Codex MCP elicitation request.
    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        recorder_push_codex_mcp_elicitation_request(self, title, detail, request, pending)
    }

    /// Pushes Codex app request.
    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()> {
        recorder_push_codex_app_request(self, title, detail, method, params, pending)
    }
}

/// Represents borrowed session recorder.
struct BorrowedSessionRecorder<'a> {
    recorder_state: &'a mut SessionRecorderState,
    session_id: &'a str,
    state: &'a AppState,
}

impl<'a> BorrowedSessionRecorder<'a> {
    /// Creates a new instance.
    fn new(
        state: &'a AppState,
        session_id: &'a str,
        recorder_state: &'a mut SessionRecorderState,
    ) -> Self {
        Self {
            recorder_state,
            session_id,
            state,
        }
    }
}

impl SessionRecorderAccess for BorrowedSessionRecorder<'_> {
    /// Handles state.
    fn state(&self) -> &AppState {
        self.state
    }

    /// Handles session ID.
    fn session_id(&self) -> &str {
        self.session_id
    }

    /// Handles recorder state mut.
    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState {
        self.recorder_state
    }
}

impl CodexTurnRecorder for BorrowedSessionRecorder<'_> {
    /// Pushes Codex approval.
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        recorder_push_pending_approval(
            self,
            title,
            command,
            detail,
            approval,
            |state, session_id, message_id, approval| {
                state.register_codex_pending_approval(session_id, message_id, approval)
            },
        )
    }

    /// Pushes Codex user input request.
    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        recorder_push_codex_user_input_request(self, title, detail, questions, request)
    }

    /// Pushes Codex MCP elicitation request.
    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        recorder_push_codex_mcp_elicitation_request(self, title, detail, request, pending)
    }

    /// Pushes Codex app request.
    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()> {
        recorder_push_codex_app_request(self, title, detail, method, params, pending)
    }
}

impl TurnRecorder for SessionRecorder {
    /// Records external session.
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        recorder_note_external_session(self, session_id)
    }

    /// Pushes approval.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        recorder_push_approval(self, title, command, detail)
    }

    /// Pushes text.
    fn push_text(&mut self, text: &str) -> Result<()> {
        recorder_push_text(self, text)
    }

    /// Pushes subagent result.
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        recorder_push_subagent_result(self, title, summary, conversation_id, turn_id)
    }

    /// Handles text delta.
    fn text_delta(&mut self, delta: &str) -> Result<()> {
        recorder_text_delta(self, delta)
    }

    /// Replaces streaming text.
    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        recorder_replace_streaming_text(self, text)
    }

    /// Pushes thinking.
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        recorder_push_thinking(self, title, lines)
    }

    /// Pushes diff.
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        recorder_push_diff(self, file_path, summary, diff, change_type)
    }

    /// Finishes streaming text.
    fn finish_streaming_text(&mut self) -> Result<()> {
        recorder_finish_streaming_text(self)
    }

    /// Resets turn state.
    fn reset_turn_state(&mut self) -> Result<()> {
        recorder_reset_turn_state(self)
    }

    /// Handles command started.
    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        recorder_command_started(self, key, command)
    }

    /// Handles command completed.
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        recorder_command_completed(self, key, command, output, status)
    }

    /// Upserts parallel agents.
    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        recorder_upsert_parallel_agents(self, key, agents)
    }

    /// Handles error.
    fn error(&mut self, detail: &str) -> Result<()> {
        recorder_error(self, detail)
    }
}

impl TurnRecorder for BorrowedSessionRecorder<'_> {
    /// Records external session.
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        recorder_note_external_session(self, session_id)
    }

    /// Pushes approval.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        recorder_push_approval(self, title, command, detail)
    }

    /// Pushes text.
    fn push_text(&mut self, text: &str) -> Result<()> {
        recorder_push_text(self, text)
    }

    /// Pushes subagent result.
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        recorder_push_subagent_result(self, title, summary, conversation_id, turn_id)
    }

    /// Handles text delta.
    fn text_delta(&mut self, delta: &str) -> Result<()> {
        recorder_text_delta(self, delta)
    }

    /// Replaces streaming text.
    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        recorder_replace_streaming_text(self, text)
    }

    /// Pushes thinking.
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        recorder_push_thinking(self, title, lines)
    }

    /// Pushes diff.
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        recorder_push_diff(self, file_path, summary, diff, change_type)
    }

    /// Finishes streaming text.
    fn finish_streaming_text(&mut self) -> Result<()> {
        recorder_finish_streaming_text(self)
    }

    /// Resets turn state.
    fn reset_turn_state(&mut self) -> Result<()> {
        recorder_reset_turn_state(self)
    }

    /// Handles command started.
    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        recorder_command_started(self, key, command)
    }

    /// Handles command completed.
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        recorder_command_completed(self, key, command, output, status)
    }

    /// Upserts parallel agents.
    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        recorder_upsert_parallel_agents(self, key, agents)
    }

    /// Handles error.
    fn error(&mut self, detail: &str) -> Result<()> {
        recorder_error(self, detail)
    }
}

/// Represents REPL printer.
#[derive(Default)]
struct ReplPrinter {
    assistant_stream_open: bool,
}

impl TurnRecorder for ReplPrinter {
    /// Records external session.
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        println!("session> {session_id}");
        Ok(())
    }

    /// Pushes approval.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        println!("approval> {title}");
        println!("approval> {command}");
        println!("approval> {detail}");
        Ok(())
    }

    /// Pushes text.
    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            println!("assistant> {trimmed}");
        }
        Ok(())
    }

    /// Pushes subagent result.
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        _conversation_id: Option<&str>,
        _turn_id: Option<&str>,
    ) -> Result<()> {
        println!("subagent> {title}");
        println!("{summary}");
        Ok(())
    }

    /// Upserts parallel agents.
    fn upsert_parallel_agents(
        &mut self,
        _key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        let count = agents.len();
        let label = if count == 1 { "agent" } else { "agents" };
        println!("parallel> Running {count} {label}");
        for agent in agents {
            println!("- {} ({:?})", agent.title, agent.status);
            if let Some(detail) = agent.detail.as_deref() {
                println!("  {detail}");
            }
        }
        Ok(())
    }

    /// Handles text delta.
    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        if !self.assistant_stream_open {
            print!("assistant> ");
            self.assistant_stream_open = true;
        }
        print!("{delta}");
        io::stdout().flush().context("failed to flush stdout")?;
        Ok(())
    }

    /// Replaces streaming text.
    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        self.finish_streaming_text()?;
        self.push_text(text)
    }

    /// Pushes thinking.
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        println!("thinking> {title}");
        for line in lines {
            println!("- {line}");
        }
        Ok(())
    }

    /// Pushes diff.
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        let label = match change_type {
            ChangeType::Edit => "edit",
            ChangeType::Create => "create",
        };
        println!("diff> {label} {file_path}");
        println!("{summary}");
        println!("{diff}");
        Ok(())
    }

    /// Finishes streaming text.
    fn finish_streaming_text(&mut self) -> Result<()> {
        if self.assistant_stream_open {
            println!();
            self.assistant_stream_open = false;
        }
        Ok(())
    }

    /// Handles command started.
    fn command_started(&mut self, _key: &str, command: &str) -> Result<()> {
        println!("cmd> {command}");
        Ok(())
    }

    /// Handles command completed.
    fn command_completed(
        &mut self,
        _key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        println!("cmd> completed `{command}` ({})", status.label());
        if !output.trim().is_empty() {
            println!("{output}");
        }
        Ok(())
    }

    /// Handles error.
    fn error(&mut self, detail: &str) -> Result<()> {
        println!("error> {detail}");
        Ok(())
    }
}

impl CodexTurnRecorder for ReplPrinter {
    /// Pushes Codex approval.
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        _approval: CodexPendingApproval,
    ) -> Result<()> {
        self.push_approval(title, command, detail)
    }

    /// Pushes Codex user input request.
    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        _request: CodexPendingUserInput,
    ) -> Result<()> {
        println!("input> {title}");
        println!("input> {detail}");
        for question in questions {
            println!("- {}: {}", question.header, question.question);
        }
        Ok(())
    }

    /// Pushes Codex MCP elicitation request.
    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        _pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        println!("mcp> {title}");
        println!("mcp> {detail}");
        println!(
            "{}",
            serde_json::to_string_pretty(&request).unwrap_or_else(|_| request.thread_id)
        );
        Ok(())
    }

    /// Pushes Codex app request.
    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        _pending: CodexPendingAppRequest,
    ) -> Result<()> {
        println!("codex-request> {title}");
        println!("codex-request> {detail}");
        println!("codex-request> method: {method}");
        println!(
            "{}",
            serde_json::to_string_pretty(&params).unwrap_or_else(|_| params.to_string())
        );
        Ok(())
    }
}

/// Runs ACP turn.
fn run_acp_turn(
    agent: AcpAgent,
    _cwd: &str,
    _external_session_id: Option<&str>,
    _model: &str,
    _prompt: &str,
    _recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    bail!("{} REPL mode is not supported yet", agent.label())
}

/// Runs Codex turn.
fn run_codex_turn(
    cwd: &str,
    external_session_id: Option<&str>,
    model: &str,
    sandbox_mode: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    reasoning_effort: CodexReasoningEffort,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let cwd = normalize_local_user_facing_path(cwd);
    let codex_home = prepare_termal_codex_home(&cwd, "repl")?;
    let mut command = codex_command()?;
    command
        .arg("app-server")
        .args(["--listen", "stdio://"])
        .env("CODEX_HOME", &codex_home);

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Codex app-server")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture Codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture Codex app-server stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture Codex app-server stderr")?;
    let process = Arc::new(SharedChild::new(child).context("failed to share Codex child")?);
    let stdout_rx = spawn_repl_codex_stdout_reader(stdout);

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut repl_state = ReplCodexSessionState {
        resolved_session_id: external_session_id.map(str::to_owned),
        ..ReplCodexSessionState::default()
    };
    let run_result = (|| -> Result<String> {
        send_repl_codex_json_rpc_request(
            &mut child_stdin,
            &stdout_rx,
            &mut repl_state,
            recorder,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "termal",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
            Duration::from_secs(15),
        )?;
        write_codex_json_rpc_message(
            &mut child_stdin,
            &json_rpc_notification_message("initialized"),
        )?;

        let thread_result = match external_session_id {
            Some(thread_id) => send_repl_codex_json_rpc_request(
                &mut child_stdin,
                &stdout_rx,
                &mut repl_state,
                recorder,
                "thread/resume",
                json!({
                    "threadId": thread_id,
                    "cwd": cwd.as_str(),
                    "model": model,
                    "sandbox": sandbox_mode.as_cli_value(),
                    "approvalPolicy": approval_policy.as_cli_value(),
                }),
                Duration::from_secs(30),
            )?,
            None => send_repl_codex_json_rpc_request(
                &mut child_stdin,
                &stdout_rx,
                &mut repl_state,
                recorder,
                "thread/start",
                json!({
                    "cwd": cwd.as_str(),
                    "model": model,
                    "sandbox": sandbox_mode.as_cli_value(),
                    "approvalPolicy": approval_policy.as_cli_value(),
                    "personality": "pragmatic",
                }),
                Duration::from_secs(30),
            )?,
        };
        if let Some(thread_id) = thread_result.pointer("/thread/id").and_then(Value::as_str) {
            remember_repl_codex_thread_id(&mut repl_state, recorder, thread_id)?;
        }

        let resolved_thread_id = repl_state
            .resolved_session_id
            .clone()
            .ok_or_else(|| anyhow!("Codex did not return a thread id"))?;

        repl_state.turn_completed = false;
        repl_state.turn_failed = None;
        let turn_result = send_repl_codex_json_rpc_request(
            &mut child_stdin,
            &stdout_rx,
            &mut repl_state,
            recorder,
            "turn/start",
            json!({
                "threadId": resolved_thread_id,
                "cwd": cwd.as_str(),
                "approvalPolicy": approval_policy.as_cli_value(),
                "effort": reasoning_effort.as_api_value(),
                "model": model,
                "sandboxPolicy": codex_sandbox_policy_value(sandbox_mode),
                "input": codex_user_input_items(prompt, &[]),
            }),
            Duration::from_secs(30),
        )?;
        repl_state.current_turn_id = turn_result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(repl_state.current_turn_id.clone());

        pump_repl_codex_turn(&stdout_rx, &mut child_stdin, &mut repl_state, recorder)?;
        recorder.finish_streaming_text()?;
        if let Some(detail) = repl_state.turn_failed.as_deref() {
            bail!(detail.to_owned());
        }

        Ok(resolved_thread_id)
    })();

    let _ = recorder.finish_streaming_text();
    drop(child_stdin);
    let (status, forced_shutdown) = shutdown_repl_codex_process(&process)?;
    let stderr_lines = stderr_thread.join().unwrap_or_default();
    let stderr_output = stderr_lines.join("\n");

    match run_result {
        Ok(session_id) => {
            if !status.success() && !forced_shutdown {
                if stderr_output.trim().is_empty() {
                    bail!("Codex app-server exited with status {status}");
                } else {
                    bail!("Codex app-server exited with status {status}: {stderr_output}");
                }
            }
            Ok(session_id)
        }
        Err(err) => {
            if !status.success() && !forced_shutdown && !stderr_output.trim().is_empty() {
                Err(err.context(format!(
                    "Codex app-server exited with status {status}: {stderr_output}"
                )))
            } else {
                Err(err)
            }
        }
    }
}


/// Returns the default Codex sandbox mode.
fn default_codex_sandbox_mode() -> CodexSandboxMode {
    match std::env::var("TERMAL_CODEX_SANDBOX").ok().as_deref() {
        Some("read-only") => CodexSandboxMode::ReadOnly,
        Some("danger-full-access") => CodexSandboxMode::DangerFullAccess,
        _ => CodexSandboxMode::WorkspaceWrite,
    }
}

/// Returns the default Codex approval policy.
fn default_codex_approval_policy() -> CodexApprovalPolicy {
    match std::env::var("TERMAL_CODEX_APPROVAL").ok().as_deref() {
        Some("untrusted") => CodexApprovalPolicy::Untrusted,
        Some("on-request") => CodexApprovalPolicy::OnRequest,
        Some("on-failure") => CodexApprovalPolicy::OnFailure,
        _ => CodexApprovalPolicy::Never,
    }
}

/// Returns the default Codex reasoning effort.
fn default_codex_reasoning_effort() -> CodexReasoningEffort {
    match std::env::var("TERMAL_CODEX_REASONING_EFFORT")
        .ok()
        .as_deref()
    {
        Some("none") => CodexReasoningEffort::None,
        Some("minimal") => CodexReasoningEffort::Minimal,
        Some("low") => CodexReasoningEffort::Low,
        Some("high") => CodexReasoningEffort::High,
        Some("xhigh") => CodexReasoningEffort::XHigh,
        _ => CodexReasoningEffort::Medium,
    }
}

/// Returns the default Claude approval mode.
fn default_claude_approval_mode() -> ClaudeApprovalMode {
    ClaudeApprovalMode::Ask
}

/// Returns the default Claude effort.
fn default_claude_effort() -> ClaudeEffortLevel {
    ClaudeEffortLevel::Default
}

const LOCAL_REMOTE_ID: &str = "local";
const LOCAL_REMOTE_NAME: &str = "Local";
const DEFAULT_SSH_REMOTE_PORT: u16 = 22;

/// Returns the default local remote ID.
fn default_local_remote_id() -> String {
    LOCAL_REMOTE_ID.to_owned()
}

/// Returns the default remote enabled.
fn default_remote_enabled() -> bool {
    true
}

/// Returns the default remote configs.
fn default_remote_configs() -> Vec<RemoteConfig> {
    vec![RemoteConfig::local()]
}

/// Defines the remote transport variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum RemoteTransport {
    Local,
    Ssh,
}

/// Holds remote configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConfig {
    id: String,
    name: String,
    transport: RemoteTransport,
    #[serde(default = "default_remote_enabled")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

impl RemoteConfig {
    /// Builds the local default value.
    fn local() -> Self {
        Self {
            id: default_local_remote_id(),
            name: LOCAL_REMOTE_NAME.to_owned(),
            transport: RemoteTransport::Local,
            enabled: true,
            host: None,
            port: None,
            user: None,
        }
    }
}

/// Represents app preferences.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppPreferences {
    #[serde(default = "default_codex_reasoning_effort")]
    default_codex_reasoning_effort: CodexReasoningEffort,
    #[serde(default = "default_claude_approval_mode")]
    default_claude_approval_mode: ClaudeApprovalMode,
    #[serde(default = "default_claude_effort")]
    default_claude_effort: ClaudeEffortLevel,
    #[serde(default = "default_remote_configs")]
    remotes: Vec<RemoteConfig>,
}

impl Default for AppPreferences {
    /// Builds the default value.
    fn default() -> Self {
        Self {
            default_codex_reasoning_effort: default_codex_reasoning_effort(),
            default_claude_approval_mode: default_claude_approval_mode(),
            default_claude_effort: default_claude_effort(),
            remotes: default_remote_configs(),
        }
    }
}

/// Returns the default cursor mode.
fn default_cursor_mode() -> CursorMode {
    CursorMode::Agent
}

/// Returns the default Gemini approval mode.
fn default_gemini_approval_mode() -> GeminiApprovalMode {
    GeminiApprovalMode::Default
}

/// Handles log unhandled Codex event.
fn log_unhandled_codex_event(context: &str, message: &Value) {
    eprintln!("codex diagnostic> {context}: {message}");
}

/// Runs Claude turn.
fn run_claude_turn(
    cwd: &str,
    session_id: Option<&str>,
    model: &str,
    approval_mode: ClaudeApprovalMode,
    effort: ClaudeEffortLevel,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let cwd = normalize_local_user_facing_path(cwd);
    let mut command = Command::new("claude");
    command.current_dir(&cwd);
    let expected_session_id = session_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let session_arg = session_id
        .map(ClaudeCliSessionArg::Resume)
        .unwrap_or(ClaudeCliSessionArg::SessionId(&expected_session_id));
    command.args(claude_cli_oneshot_args(
        model,
        approval_mode,
        effort,
        session_arg,
    ));

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Claude")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture child stdin")?;
    writeln!(child_stdin, "{prompt}").context("failed to write prompt to Claude stdin")?;
    drop(child_stdin);

    let stdout = child
        .stdout
        .take()
        .context("failed to capture child stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture child stderr")?;

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut reader = BufReader::new(stdout);
    let mut resolved_session_id = Some(expected_session_id);
    let mut raw_line = String::new();
    let mut state = ClaudeTurnState::default();

    loop {
        raw_line.clear();
        let bytes_read = reader
            .read_line(&mut raw_line)
            .context("failed to read stdout from Claude")?;

        if bytes_read == 0 {
            break;
        }

        let message: Value = serde_json::from_str(raw_line.trim_end()).with_context(|| {
            format!("failed to parse Claude JSON line: {}", raw_line.trim_end())
        })?;

        handle_claude_event(&message, &mut resolved_session_id, &mut state, recorder)?;
    }

    recorder.finish_streaming_text()?;

    let status = child.wait().context("failed waiting for Claude process")?;
    let stderr_lines = stderr_thread.join().unwrap_or_default();

    if !status.success() {
        let stderr_output = stderr_lines.join("\n");
        if stderr_output.trim().is_empty() {
            bail!("Claude exited with status {status}");
        } else {
            bail!("Claude exited with status {status}: {stderr_output}");
        }
    }

    resolved_session_id.ok_or_else(|| anyhow!("Claude completed without emitting a session id"))
}

/// Tracks Claude turn state.
#[derive(Default)]
struct ClaudeTurnState {
    approval_keys_this_turn: HashSet<String>,
    parallel_agent_group_key: Option<String>,
    parallel_agent_order: Vec<String>,
    parallel_agents: HashMap<String, ParallelAgentProgress>,
    permission_denied_this_turn: bool,
    pending_tools: HashMap<String, ClaudeToolUse>,
    streamed_assistant_text: String,
    saw_text_delta: bool,
}

/// Represents Claude tool use.
struct ClaudeToolUse {
    command: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    name: String,
    subagent_type: Option<String>,
}

/// Represents the Claude tool permission request payload.
struct ClaudeToolPermissionRequest {
    detail: String,
    permission_mode_for_session: Option<String>,
    request_id: String,
    title: String,
    tool_name: String,
    tool_input: Value,
}

/// Classifies Claude control request.
fn classify_claude_control_request(
    message: &Value,
    state: &mut ClaudeTurnState,
    approval_mode: ClaudeApprovalMode,
) -> Result<Option<ClaudeControlRequestAction>> {
    let Some(request) = parse_claude_tool_permission_request(message) else {
        return Ok(None);
    };

    let command = describe_claude_tool_request(&request);
    let key = format!("{}\n{}\n{}", request.request_id, request.title, command);
    if !state.approval_keys_this_turn.insert(key) {
        return Ok(None);
    }

    Ok(Some(match approval_mode {
        ClaudeApprovalMode::Ask => ClaudeControlRequestAction::QueueApproval {
            title: request.title,
            command,
            detail: request.detail,
            approval: ClaudePendingApproval {
                permission_mode_for_session: request.permission_mode_for_session,
                request_id: request.request_id,
                tool_input: request.tool_input,
            },
        },
        ClaudeApprovalMode::AutoApprove => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
                request_id: request.request_id,
                updated_input: request.tool_input,
            })
        }
        ClaudeApprovalMode::Plan => {
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
                request_id: request.request_id,
                message: "TermAl denied this tool request because Claude is in plan mode."
                    .to_owned(),
            })
        }
    }))
}

/// Parses Claude tool permission request.
fn parse_claude_tool_permission_request(message: &Value) -> Option<ClaudeToolPermissionRequest> {
    if message.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }

    let request = message.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }

    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = request.get("tool_name").and_then(Value::as_str)?;
    let tool_input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let permission_mode_for_session = request
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .and_then(|suggestions| {
            suggestions.iter().find_map(|suggestion| {
                (suggestion.get("type").and_then(Value::as_str) == Some("setMode")
                    && suggestion.get("destination").and_then(Value::as_str) == Some("session"))
                .then(|| suggestion.get("mode").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned)
            })
        });

    let detail = describe_claude_permission_detail(
        tool_name,
        &tool_input,
        request.get("decision_reason").and_then(Value::as_str),
    );

    Some(ClaudeToolPermissionRequest {
        detail,
        permission_mode_for_session,
        request_id,
        title: "Claude needs approval".to_owned(),
        tool_name: tool_name.to_owned(),
        tool_input,
    })
}

/// Records Claude assistant text delta.
fn record_claude_assistant_text_delta(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let delta = if state.saw_text_delta {
        text
    } else {
        text.trim_start_matches('\n')
    };
    if delta.is_empty() {
        return Ok(());
    }

    recorder.text_delta(delta)?;
    state.saw_text_delta = true;
    state.streamed_assistant_text.push_str(delta);
    Ok(())
}

/// Records Claude completed assistant text.
fn record_claude_completed_assistant_text(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if !state.saw_text_delta {
        state.streamed_assistant_text.clear();
        state.streamed_assistant_text.push_str(trimmed);
        return recorder.push_text(trimmed);
    }

    match next_completed_codex_text_update(&mut state.streamed_assistant_text, trimmed) {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }
}

/// Finishes Claude assistant text stream.
fn finish_claude_assistant_text_stream<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    recorder.finish_streaming_text()?;
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
    Ok(())
}

/// Clears Claude turn-local state.
fn clear_claude_turn_state(state: &mut ClaudeTurnState) {
    state.approval_keys_this_turn.clear();
    state.parallel_agent_group_key = None;
    state.parallel_agent_order.clear();
    state.parallel_agents.clear();
    state.permission_denied_this_turn = false;
    state.pending_tools.clear();
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
}

/// Resets Claude turn-local parser and recorder state.
fn reset_claude_turn_state<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    finish_claude_assistant_text_stream(state, recorder)?;
    clear_claude_turn_state(state);
    recorder.reset_turn_state()
}

/// Handles Claude event.
fn handle_claude_event(
    message: &Value,
    session_id: &mut Option<String>,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        return Ok(());
    };

    match event_type {
        "system" => {
            if message.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(found_session_id) = message.get("session_id").and_then(Value::as_str) {
                    *session_id = Some(found_session_id.to_owned());
                    recorder.note_external_session(found_session_id)?;
                }
            }
        }
        "stream_event" => {
            let Some(stream_type) = message.pointer("/event/type").and_then(Value::as_str) else {
                return Ok(());
            };

            match stream_type {
                "content_block_delta" => {
                    if !state.permission_denied_this_turn {
                        if let Some(text) = message
                            .pointer("/event/delta/text")
                            .or_else(|| message.pointer("/event/delta/text_delta"))
                            .and_then(Value::as_str)
                        {
                            record_claude_assistant_text_delta(state, recorder, text)?;
                        }
                    }
                }
                "message_stop" => {
                    // Claude can emit the final assistant payload after `message_stop`.
                    // Keep the current text bubble open so any unseen suffix lands in it.
                }
                _ => {}
            }
        }
        "assistant" => {
            if let Some(contents) = message
                .pointer("/message/content")
                .and_then(Value::as_array)
            {
                for content in contents {
                    let Some(content_type) = content.get("type").and_then(Value::as_str) else {
                        continue;
                    };

                    match content_type {
                        "text" => {
                            if let Some(text) = content.get("text").and_then(Value::as_str) {
                                if state.permission_denied_this_turn {
                                    continue;
                                }
                                record_claude_completed_assistant_text(state, recorder, text)?;
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = content.get("thinking").and_then(Value::as_str)
                            {
                                finish_claude_assistant_text_stream(state, recorder)?;
                                let lines = split_thinking_lines(thinking);
                                recorder.push_thinking("Thinking", lines)?;
                            }
                        }
                        "tool_use" => {
                            finish_claude_assistant_text_stream(state, recorder)?;
                            register_claude_tool_use(content, state, recorder)?;
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            handle_claude_tool_result(message, state, recorder)?;
        }
        "result" => {
            reset_claude_turn_state(state, recorder)?;

            if message
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                recorder.error(&summarize_error(message))?;
            }
        }
        _ => {}
    }

    Ok(())
}

/// Registers Claude tool use.
fn register_claude_tool_use(
    content: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(tool_id) = content.get("id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(name) = content.get("name").and_then(Value::as_str) else {
        return Ok(());
    };

    let input = content.get("input");
    let command = input
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let description = input
        .and_then(|value| value.get("description"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let file_path = input
        .and_then(|value| value.get("file_path").or_else(|| value.get("filePath")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let subagent_type = input
        .and_then(|value| {
            value
                .get("subagent_type")
                .or_else(|| value.get("subagentType"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned);

    state.pending_tools.insert(
        tool_id.to_owned(),
        ClaudeToolUse {
            command: command.clone(),
            description: description.clone(),
            file_path,
            name: name.to_owned(),
            subagent_type: subagent_type.clone(),
        },
    );

    match name {
        "Bash" => {
            let command_label = command
                .as_deref()
                .or(description.as_deref())
                .unwrap_or("Bash");
            recorder.command_started(tool_id, command_label)?;
        }
        "Task" => {
            if state.parallel_agent_group_key.is_none() {
                state.parallel_agent_group_key = Some(format!("claude-task-group-{tool_id}"));
            }
            if !state.parallel_agents.contains_key(tool_id) {
                state.parallel_agent_order.push(tool_id.to_owned());
            }
            state.parallel_agents.insert(
                tool_id.to_owned(),
                ParallelAgentProgress {
                    detail: Some("Initializing...".to_owned()),
                    id: tool_id.to_owned(),
                    status: ParallelAgentStatus::Initializing,
                    title: describe_claude_task_tool(
                        description.as_deref(),
                        subagent_type.as_deref(),
                    ),
                },
            );
            sync_claude_parallel_agents(state, recorder)?;
        }
        _ => {}
    }

    Ok(())
}
/// Handles Claude tool result.
fn handle_claude_tool_result(
    message: &Value,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(contents) = message
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for content in contents {
        if content.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let Some(tool_use_id) = content.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(tool_use) = state.pending_tools.remove(tool_use_id) else {
            continue;
        };

        let is_error = content
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let detail = extract_claude_tool_result_text(message, content);

        match tool_use.name.as_str() {
            "Bash" => handle_claude_bash_result(
                tool_use_id,
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Task" => handle_claude_task_result(
                tool_use_id,
                &tool_use,
                &detail,
                is_error,
                state,
                recorder,
            )?,
            "Write" | "Edit" => handle_claude_file_result(
                &tool_use,
                message.get("tool_use_result"),
                &detail,
                is_error,
                state,
                recorder,
            )?,
            _ => {
                if is_error {
                    recorder.error(&detail)?;
                }
            }
        }
    }

    Ok(())
}
/// Handles Claude task result.
fn handle_claude_task_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let title = describe_claude_task_tool(
        tool_use.description.as_deref(),
        tool_use.subagent_type.as_deref(),
    );
    let summarized_detail = summarize_claude_task_detail(detail, is_error);
    let status = if is_error {
        ParallelAgentStatus::Error
    } else {
        ParallelAgentStatus::Completed
    };

    if let Some(agent) = state.parallel_agents.get_mut(tool_use_id) {
        agent.detail = Some(summarized_detail.clone());
        agent.status = status;
        if agent.title.trim().is_empty() {
            agent.title = title.clone();
        }
    } else {
        state.parallel_agent_order.push(tool_use_id.to_owned());
        state.parallel_agents.insert(
            tool_use_id.to_owned(),
            ParallelAgentProgress {
                detail: Some(summarized_detail.clone()),
                id: tool_use_id.to_owned(),
                status,
                title: title.clone(),
            },
        );
    }

    sync_claude_parallel_agents(state, recorder)?;

    let trimmed = detail.trim();
    let result_summary = if trimmed.is_empty() {
        if is_error {
            Some(summarized_detail.as_str())
        } else {
            None
        }
    } else {
        Some(trimmed)
    };
    if let Some(summary) = result_summary {
        recorder.push_subagent_result(&title, summary, None, None)?;
    }

    Ok(())
}

/// Syncs Claude parallel agents.
fn sync_claude_parallel_agents(
    state: &ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(key) = state.parallel_agent_group_key.as_deref() else {
        return Ok(());
    };

    let agents = state
        .parallel_agent_order
        .iter()
        .filter_map(|agent_id| state.parallel_agents.get(agent_id).cloned())
        .collect::<Vec<_>>();
    if agents.is_empty() {
        return Ok(());
    }

    recorder.upsert_parallel_agents(key, &agents)
}

/// Describes Claude task tool.
fn describe_claude_task_tool(description: Option<&str>, subagent_type: Option<&str>) -> String {
    let trimmed_description = description.unwrap_or("").trim();
    if !trimmed_description.is_empty() {
        return trimmed_description.to_owned();
    }

    let trimmed_subagent_type = subagent_type.unwrap_or("").trim();
    if !trimmed_subagent_type.is_empty() {
        return format!("{} agent", trimmed_subagent_type.replace('-', " "));
    }

    "Task agent".to_owned()
}

/// Summarizes Claude task detail.
fn summarize_claude_task_detail(detail: &str, is_error: bool) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return if is_error {
            "Task failed.".to_owned()
        } else {
            "Completed.".to_owned()
        };
    }

    make_preview(trimmed)
}
/// Handles Claude bash result.
fn handle_claude_bash_result(
    tool_use_id: &str,
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error && is_permission_denial(detail) {
        state.permission_denied_this_turn = true;
        record_claude_approval(
            state,
            recorder,
            "Claude needs approval",
            tool_use.command.as_deref().unwrap_or("Bash"),
            detail,
        )?;
        return Ok(());
    }

    let stdout = tool_use_result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stderr = tool_use_result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let interrupted = tool_use_result
        .and_then(|value| value.get("interrupted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(stderr);
    }
    if output.trim().is_empty() && !detail.is_empty() {
        output.push_str(detail);
    }

    let status = if is_error || interrupted {
        CommandStatus::Error
    } else {
        CommandStatus::Success
    };
    let command = tool_use.command.as_deref().unwrap_or("Bash");
    recorder.command_completed(tool_use_id, command, output.trim_end(), status)
}

/// Handles Claude file result.
fn handle_claude_file_result(
    tool_use: &ClaudeToolUse,
    tool_use_result: Option<&Value>,
    detail: &str,
    is_error: bool,
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if is_error {
        if is_permission_denial(detail) {
            state.permission_denied_this_turn = true;
            record_claude_approval(
                state,
                recorder,
                "Claude needs approval",
                &describe_claude_tool_action(tool_use),
                detail,
            )?;
        } else {
            recorder.error(detail)?;
        }
        return Ok(());
    }

    let Some(tool_use_result) = tool_use_result else {
        return Ok(());
    };

    let tool_kind = tool_use_result
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(file_path) = tool_use_result
        .get("filePath")
        .and_then(Value::as_str)
        .or(tool_use.file_path.as_deref())
    else {
        return Ok(());
    };

    match tool_kind {
        "create" => {
            let content = tool_use_result
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let diff = content
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            recorder.push_diff(
                file_path,
                &format!("Created {}", short_file_name(file_path)),
                &diff,
                ChangeType::Create,
            )?;
        }
        "update" => {
            let diff = tool_use_result
                .get("structuredPatch")
                .and_then(Value::as_array)
                .map(|patches| flatten_structured_patch(patches.as_slice()))
                .filter(|diff| !diff.trim().is_empty())
                .unwrap_or_else(|| {
                    fallback_file_diff(
                        tool_use_result
                            .get("originalFile")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        tool_use_result
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                });
            recorder.push_diff(
                file_path,
                &format!("Updated {}", short_file_name(file_path)),
                &diff,
                ChangeType::Edit,
            )?;
        }
        _ => {}
    }

    Ok(())
}

/// Extracts Claude tool result text.
fn extract_claude_tool_result_text(message: &Value, content: &Value) -> String {
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(parts) = content.get("content").and_then(Value::as_array) {
        let combined = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !combined.trim().is_empty() {
            return combined;
        }
    }
    if let Some(text) = message.get("tool_use_result").and_then(Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = message
        .get("tool_use_result")
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }

    "Claude tool call failed.".to_owned()
}
/// Returns whether permission denial.
fn is_permission_denial(detail: &str) -> bool {
    detail.contains("requested permissions")
}

/// Records Claude approval.
fn record_claude_approval(
    state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
    title: &str,
    command: &str,
    detail: &str,
) -> Result<()> {
    let key = format!("{title}\n{command}\n{detail}");
    if state.approval_keys_this_turn.insert(key) {
        recorder.push_approval(title, command, detail)?;
    }

    Ok(())
}

/// Describes Claude tool request.
fn describe_claude_tool_request(request: &ClaudeToolPermissionRequest) -> String {
    describe_claude_tool_action_from_parts(&request.tool_name, &request.tool_input)
}

/// Describes Claude tool action.
fn describe_claude_tool_action(tool_use: &ClaudeToolUse) -> String {
    match (
        tool_use.name.as_str(),
        tool_use.file_path.as_deref(),
        tool_use.command.as_deref(),
    ) {
        ("Write" | "Edit", Some(file_path), _) => format!("{} {}", tool_use.name, file_path),
        (_, _, Some(command)) => command.to_owned(),
        _ => tool_use.name.clone(),
    }
}

/// Describes Claude tool action from parts.
fn describe_claude_tool_action_from_parts(tool_name: &str, tool_input: &Value) -> String {
    match tool_name {
        "Write" | "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("{tool_name} {file_path}"))
            .unwrap_or_else(|| tool_name.to_owned()),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| tool_name.to_owned()),
        _ => tool_name.to_owned(),
    }
}

/// Describes Claude permission detail.
fn describe_claude_permission_detail(
    tool_name: &str,
    tool_input: &Value,
    decision_reason: Option<&str>,
) -> String {
    let specific = match tool_name {
        "Write" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to write to {file_path}.")),
        "Edit" => tool_input
            .get("file_path")
            .or_else(|| tool_input.get("filePath"))
            .and_then(Value::as_str)
            .map(|file_path| format!("Claude requested permission to edit {file_path}.")),
        "Bash" => tool_input
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("Claude requested permission to run `{command}`.")),
        _ => None,
    };

    match (
        specific,
        decision_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty()),
    ) {
        (Some(specific), Some(reason)) => format!("{specific} Reason: {reason}."),
        (Some(specific), None) => specific,
        (None, Some(reason)) => format!("Claude requested approval. Reason: {reason}."),
        (None, None) => "Claude requested approval.".to_owned(),
    }
}

/// Handles split thinking lines.
fn split_thinking_lines(thinking: &str) -> Vec<String> {
    let lines = thinking
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if lines.is_empty() && !thinking.trim().is_empty() {
        vec![thinking.trim().to_owned()]
    } else {
        lines
    }
}

/// Handles flatten structured patch.
fn flatten_structured_patch(patches: &[Value]) -> String {
    patches
        .iter()
        .filter_map(|patch| patch.get("lines").and_then(Value::as_array))
        .flat_map(|lines| lines.iter())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Handles fallback file diff.
fn fallback_file_diff(original: &str, updated: &str) -> String {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }
    lines.join("\n")
}

/// Handles short file name.
fn short_file_name(file_path: &str) -> &str {
    file_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(file_path)
}

/// Summarizes error.
fn summarize_error(value: &Value) -> String {
    summarize_structured_error(value).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    })
}

/// Summarizes structured error.
fn summarize_structured_error(value: &Value) -> Option<String> {
    summarize_retryable_connectivity_error(value)
        .or_else(|| summarize_error_fields(value))
        .or_else(|| value.get("error").and_then(summarize_error_fields))
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|result| !result.is_empty())
                .map(str::to_owned)
        })
}

/// Summarizes error fields.
fn summarize_error_fields(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message");
    let detail = trimmed_string_field(value, "additionalDetails")
        .or_else(|| trimmed_string_field(value, "detail"))
        .or_else(|| trimmed_string_field(value, "details"));

    match (message, detail) {
        (Some(message), Some(detail)) if contains_ignore_ascii_case(message, detail) => {
            Some(message.to_owned())
        }
        (Some(message), Some(detail)) if contains_ignore_ascii_case(detail, message) => {
            Some(detail.to_owned())
        }
        (Some(message), Some(detail)) => Some(format!("{message} {detail}")),
        (Some(message), None) => Some(message.to_owned()),
        (None, Some(detail)) => Some(detail.to_owned()),
        (None, None) => None,
    }
}

/// Summarizes retryable connectivity error.
fn summarize_retryable_connectivity_error(value: &Value) -> Option<String> {
    if !is_retryable_connectivity_error(value) {
        return None;
    }

    let mut summary = "Connection dropped before the response finished.".to_owned();
    if let Some(retry_status) = summarize_retry_status(value) {
        summary.push(' ');
        summary.push_str(&retry_status);
    } else {
        summary.push_str(" Retrying automatically.");
    }

    Some(summary)
}

/// Returns whether retryable connectivity error.
fn is_retryable_connectivity_error(value: &Value) -> bool {
    codex_error_will_retry(value) && has_connectivity_marker(value)
}

/// Handles Codex error will retry.
fn codex_error_will_retry(value: &Value) -> bool {
    value
        .get("willRetry")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("error")
            .and_then(|error| error.get("willRetry"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Returns whether connectivity marker.
fn has_connectivity_marker(value: &Value) -> bool {
    value
        .pointer("/error/codexErrorInfo/responseStreamDisconnected")
        .is_some_and(|marker| !marker.is_null())
        || [
            trimmed_string_field(value, "message"),
            trimmed_string_field(value, "additionalDetails"),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "message")),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "additionalDetails")),
        ]
        .into_iter()
        .flatten()
        .any(is_connectivity_text)
}

/// Summarizes retry status.
fn summarize_retry_status(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message").or_else(|| {
        value
            .get("error")
            .and_then(|error| trimmed_string_field(error, "message"))
    })?;

    let counts = message
        .strip_prefix("Reconnecting...")
        .or_else(|| message.strip_prefix("Reconnecting…"))
        .map(str::trim);

    let Some(counts) = counts else {
        return Some("Retrying automatically.".to_owned());
    };

    let Some((current, total)) = counts.split_once('/') else {
        return Some("Retrying automatically.".to_owned());
    };

    let current = current.trim().parse::<usize>().ok()?;
    let total = total.trim().parse::<usize>().ok()?;
    Some(format!(
        "Retrying automatically (attempt {current} of {total})."
    ))
}

/// Handles trimmed string field.
fn trimmed_string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

/// Returns whether ignore ascii case.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Returns whether connectivity text.
fn is_connectivity_text(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("stream disconnected before completion")
        || normalized.contains("websocket closed by server before response.completed")
        || normalized.contains("response stream disconnected")
        || normalized.contains("connection dropped")
        || normalized.contains("reconnecting")
}

/// Builds preview.
fn make_preview(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let compact = first_line.replace('\t', " ");
    let compact = compact.trim();
    if compact.is_empty() {
        return "Waiting for activity.".to_owned();
    }

    const LIMIT: usize = 88;
    let mut preview = compact.chars().take(LIMIT).collect::<String>();
    if compact.chars().count() > LIMIT {
        preview.push_str("...");
    }
    preview
}

/// Handles image attachment summary.
fn image_attachment_summary(count: usize) -> String {
    match count {
        0 => "Waiting for activity.".to_owned(),
        1 => "1 image attached".to_owned(),
        count => format!("{count} images attached"),
    }
}

/// Handles prompt preview text.
fn prompt_preview_text(text: &str, attachments: &[MessageImageAttachment]) -> String {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        return make_preview(trimmed);
    }

    make_preview(&image_attachment_summary(attachments.len()))
}

/// Handles shell language.
fn shell_language() -> &'static str {
    "bash"
}

/// Infers language from path.
fn infer_language_from_path(path: &FsPath) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match file_name.as_str() {
        "dockerfile" => return Some("dockerfile"),
        "makefile" => return Some("makefile"),
        _ => {}
    }

    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "bash" | "sh" | "zsh" => Some("bash"),
        "cjs" | "js" | "jsx" | "mjs" => Some("javascript"),
        "css" => Some("css"),
        "dart" => Some("dart"),
        "go" => Some("go"),
        "htm" | "html" | "svg" | "xml" => Some("xml"),
        "ini" | "toml" => Some("ini"),
        "json" => Some("json"),
        "md" | "mdx" => Some("markdown"),
        "mts" | "ts" | "tsx" => Some("typescript"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "sql" => Some("sql"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

/// Infers command output language.
fn infer_command_output_language(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("git diff")
        || normalized
            .split(command_token_separator)
            .any(|token| token == "diff" || token == "patch")
    {
        return Some("diff");
    }

    if !normalized
        .split(command_token_separator)
        .any(is_file_viewer_command)
    {
        return None;
    }

    command
        .split(command_token_separator)
        .map(clean_command_path_hint)
        .rev()
        .find_map(|candidate| infer_language_from_path(FsPath::new(candidate)))
}

/// Handles command token separator.
fn command_token_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '|' | '&' | ';')
}

/// Returns whether file viewer command.
fn is_file_viewer_command(token: &str) -> bool {
    matches!(
        token,
        "bat" | "cat" | "head" | "less" | "more" | "sed" | "tail"
    )
}

/// Cleans command path hint.
fn clean_command_path_hint(token: &str) -> &str {
    let trimmed = token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
        )
    });

    trimmed
        .rsplit_once('=')
        .map(|(_, value)| value)
        .unwrap_or(trimmed)
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
            )
        })
}

/// Handles Codex user input items.
fn codex_user_input_items(prompt: &str, attachments: &[PromptImageAttachment]) -> Vec<Value> {
    let mut input = Vec::with_capacity(attachments.len() + usize::from(!prompt.is_empty()));

    if !prompt.is_empty() {
        input.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }

    input.extend(attachments.iter().map(|attachment| {
        json!({
            "type": "image",
            "url": codex_image_data_url(attachment),
        })
    }));

    input
}

/// Handles Codex image data URL.
fn codex_image_data_url(attachment: &PromptImageAttachment) -> String {
    format!(
        "data:{};base64,{}",
        attachment.metadata.media_type, attachment.data
    )
}

/// Parses prompt image attachments.
fn parse_prompt_image_attachments(
    requests: &[SendMessageAttachmentRequest],
) -> std::result::Result<Vec<PromptImageAttachment>, ApiError> {
    requests
        .iter()
        .enumerate()
        .map(|(index, request)| parse_prompt_image_attachment(index, request))
        .collect()
}

/// Parses prompt image attachment.
fn parse_prompt_image_attachment(
    index: usize,
    request: &SendMessageAttachmentRequest,
) -> std::result::Result<PromptImageAttachment, ApiError> {
    let media_type = request.media_type.trim().to_ascii_lowercase();
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(ApiError::bad_request(format!(
            "unsupported image attachment type `{media_type}`"
        )));
    }

    let data = request.data.trim();
    if data.is_empty() {
        return Err(ApiError::bad_request(
            "image attachment data cannot be empty",
        ));
    }

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ApiError::bad_request("image attachment data is not valid base64"))?;
    if decoded.len() > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(ApiError::bad_request(format!(
            "image attachment exceeds the {} MB limit",
            MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let file_name = request
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_attachment_file_name)
        .unwrap_or_else(|| default_attachment_file_name(index, &media_type));

    Ok(PromptImageAttachment {
        data: data.to_owned(),
        metadata: MessageImageAttachment {
            byte_size: decoded.len(),
            file_name,
            media_type,
        },
    })
}

/// Returns the default attachment file name.
fn default_attachment_file_name(index: usize, media_type: &str) -> String {
    let extension = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "img",
    };

    format!("pasted-image-{}.{}", index + 1, extension)
}

/// Sanitizes attachment file name.
fn sanitize_attachment_file_name(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| match character {
            '/' | '\\' | '\0' => '-',
            other => other,
        })
        .collect::<String>();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "pasted-image".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Stamps now.
fn stamp_now() -> String {
    Local::now().format("%H:%M:%S").to_string()
}
