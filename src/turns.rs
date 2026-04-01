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

trait TurnRecorder {
    fn note_external_session(&mut self, session_id: &str) -> Result<()>;
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()>;
    fn push_text(&mut self, text: &str) -> Result<()>;
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()>;
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()>;
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()>;
    fn text_delta(&mut self, delta: &str) -> Result<()>;
    fn replace_streaming_text(&mut self, text: &str) -> Result<()>;
    fn finish_streaming_text(&mut self) -> Result<()>;
    fn command_started(&mut self, key: &str, command: &str) -> Result<()>;
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()>;
    fn upsert_parallel_agents(&mut self, key: &str, agents: &[ParallelAgentProgress])
    -> Result<()>;
    fn error(&mut self, detail: &str) -> Result<()>;
}

trait CodexTurnRecorder: TurnRecorder {
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()>;

    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()>;

    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()>;

    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()>;
}

struct SessionRecorder {
    recorder_state: SessionRecorderState,
    session_id: String,
    state: AppState,
}

impl SessionRecorder {
    fn new(state: AppState, session_id: String) -> Self {
        Self {
            recorder_state: SessionRecorderState::default(),
            session_id,
            state,
        }
    }

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

trait SessionRecorderAccess {
    fn state(&self) -> &AppState;
    fn session_id(&self) -> &str;
    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState;
}

impl SessionRecorderAccess for SessionRecorder {
    fn state(&self) -> &AppState {
        &self.state
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState {
        &mut self.recorder_state
    }
}

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

fn recorder_note_external_session<R: SessionRecorderAccess>(
    recorder: &mut R,
    session_id: &str,
) -> Result<()> {
    let state = recorder.state().clone();
    let current_session_id = recorder.session_id().to_owned();
    state.set_external_session_id(&current_session_id, session_id.to_owned())
}

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

fn recorder_finish_streaming_text<R: SessionRecorderAccess>(recorder: &mut R) -> Result<()> {
    recorder.recorder_state_mut().streaming_text_message_id = None;
    Ok(())
}

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

    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        recorder_push_codex_user_input_request(self, title, detail, questions, request)
    }

    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        recorder_push_codex_mcp_elicitation_request(self, title, detail, request, pending)
    }

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

struct BorrowedSessionRecorder<'a> {
    recorder_state: &'a mut SessionRecorderState,
    session_id: &'a str,
    state: &'a AppState,
}

impl<'a> BorrowedSessionRecorder<'a> {
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
    fn state(&self) -> &AppState {
        self.state
    }

    fn session_id(&self) -> &str {
        self.session_id
    }

    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState {
        self.recorder_state
    }
}

impl CodexTurnRecorder for BorrowedSessionRecorder<'_> {
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

    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        recorder_push_codex_user_input_request(self, title, detail, questions, request)
    }

    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()> {
        recorder_push_codex_mcp_elicitation_request(self, title, detail, request, pending)
    }

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
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        recorder_note_external_session(self, session_id)
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        recorder_push_approval(self, title, command, detail)
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        recorder_push_text(self, text)
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        recorder_push_subagent_result(self, title, summary, conversation_id, turn_id)
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        recorder_text_delta(self, delta)
    }

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        recorder_replace_streaming_text(self, text)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        recorder_push_thinking(self, title, lines)
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        recorder_push_diff(self, file_path, summary, diff, change_type)
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        recorder_finish_streaming_text(self)
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        recorder_command_started(self, key, command)
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        recorder_command_completed(self, key, command, output, status)
    }

    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        recorder_upsert_parallel_agents(self, key, agents)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        recorder_error(self, detail)
    }
}

impl TurnRecorder for BorrowedSessionRecorder<'_> {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        recorder_note_external_session(self, session_id)
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        recorder_push_approval(self, title, command, detail)
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        recorder_push_text(self, text)
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        recorder_push_subagent_result(self, title, summary, conversation_id, turn_id)
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        recorder_text_delta(self, delta)
    }

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        recorder_replace_streaming_text(self, text)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        recorder_push_thinking(self, title, lines)
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        recorder_push_diff(self, file_path, summary, diff, change_type)
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        recorder_finish_streaming_text(self)
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        recorder_command_started(self, key, command)
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        recorder_command_completed(self, key, command, output, status)
    }

    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        recorder_upsert_parallel_agents(self, key, agents)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        recorder_error(self, detail)
    }
}

#[derive(Default)]
struct ReplPrinter {
    assistant_stream_open: bool,
}

impl TurnRecorder for ReplPrinter {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        println!("session> {session_id}");
        Ok(())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        println!("approval> {title}");
        println!("approval> {command}");
        println!("approval> {detail}");
        Ok(())
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            println!("assistant> {trimmed}");
        }
        Ok(())
    }

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

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        self.finish_streaming_text()?;
        self.push_text(text)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        println!("thinking> {title}");
        for line in lines {
            println!("- {line}");
        }
        Ok(())
    }

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

    fn finish_streaming_text(&mut self) -> Result<()> {
        if self.assistant_stream_open {
            println!();
            self.assistant_stream_open = false;
        }
        Ok(())
    }

    fn command_started(&mut self, _key: &str, command: &str) -> Result<()> {
        println!("cmd> {command}");
        Ok(())
    }

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

    fn error(&mut self, detail: &str) -> Result<()> {
        println!("error> {detail}");
        Ok(())
    }
}

impl CodexTurnRecorder for ReplPrinter {
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        _approval: CodexPendingApproval,
    ) -> Result<()> {
        self.push_approval(title, command, detail)
    }

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
    command.arg("app-server").env("CODEX_HOME", &codex_home);

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
        write_codex_json_rpc_message(&mut child_stdin, &json!({ "method": "initialized" }))?;

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

#[derive(Default)]
struct ReplCodexSessionState {
    resolved_session_id: Option<String>,
    current_turn_id: Option<String>,
    turn_state: CodexTurnState,
    turn_completed: bool,
    turn_failed: Option<String>,
}

struct DynTurnRecorderRef<'a> {
    inner: &'a mut dyn TurnRecorder,
}

impl<'a> DynTurnRecorderRef<'a> {
    fn new(inner: &'a mut dyn TurnRecorder) -> Self {
        Self { inner }
    }
}

impl TurnRecorder for DynTurnRecorderRef<'_> {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        self.inner.note_external_session(session_id)
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.inner.push_approval(title, command, detail)
    }

    fn push_text(&mut self, text: &str) -> Result<()> {
        self.inner.push_text(text)
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        self.inner
            .push_subagent_result(title, summary, conversation_id, turn_id)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        self.inner.push_thinking(title, lines)
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        self.inner.push_diff(file_path, summary, diff, change_type)
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        self.inner.text_delta(delta)
    }

    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        self.inner.replace_streaming_text(text)
    }

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.inner.finish_streaming_text()
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        self.inner.command_started(key, command)
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        self.inner.command_completed(key, command, output, status)
    }

    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        self.inner.upsert_parallel_agents(key, agents)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        self.inner.error(detail)
    }
}

fn spawn_repl_codex_stdout_reader(
    stdout: impl io::Read + Send + 'static,
) -> mpsc::Receiver<std::result::Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut raw_line = String::new();
        loop {
            raw_line.clear();
            let bytes_read = match reader.read_line(&mut raw_line) {
                Ok(bytes_read) => bytes_read,
                Err(err) => {
                    let _ = tx.send(Err(format!(
                        "failed to read stdout from Codex app-server: {err}"
                    )));
                    break;
                }
            };

            if bytes_read == 0 {
                let _ = tx.send(Err("Codex app-server closed stdout".to_owned()));
                break;
            }

            let message = match serde_json::from_str(raw_line.trim_end()) {
                Ok(message) => message,
                Err(err) => {
                    let _ = tx.send(Err(format!(
                        "failed to parse Codex app-server JSON line: {err}"
                    )));
                    break;
                }
            };

            if tx.send(Ok(message)).is_err() {
                break;
            }
        }
    });
    rx
}

fn send_repl_codex_json_rpc_request(
    writer: &mut impl Write,
    stdout_rx: &mpsc::Receiver<std::result::Result<Value, String>>,
    repl_state: &mut ReplCodexSessionState,
    recorder: &mut dyn TurnRecorder,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value> {
    let request_id = Uuid::new_v4().to_string();
    write_codex_json_rpc_message(
        writer,
        &json!({
            "id": request_id,
            "method": method,
            "params": params,
        }),
    )?;

    loop {
        let message = recv_repl_codex_stdout(stdout_rx, Some(timeout), method)?;
        if let Some(response_id) = message.get("id") {
            if message.get("result").is_some() || message.get("error").is_some() {
                if codex_request_id_key(response_id) == request_id {
                    return if let Some(result) = message.get("result") {
                        Ok(result.clone())
                    } else {
                        Err(anyhow!(summarize_codex_json_rpc_error(
                            message.get("error").unwrap_or(&Value::Null)
                        )))
                    };
                }
            }
        }

        handle_repl_codex_app_server_message(&message, writer, repl_state, recorder)?;
    }
}

fn recv_repl_codex_stdout(
    stdout_rx: &mpsc::Receiver<std::result::Result<Value, String>>,
    timeout: Option<Duration>,
    context: &str,
) -> Result<Value> {
    let message = match timeout {
        Some(timeout) => stdout_rx
            .recv_timeout(timeout)
            .map_err(|err| anyhow!("timed out waiting for Codex app-server `{context}`: {err}"))?,
        None => stdout_rx.recv().map_err(|err| anyhow!("{err}"))?,
    };

    message.map_err(anyhow::Error::msg)
}

fn pump_repl_codex_turn(
    stdout_rx: &mpsc::Receiver<std::result::Result<Value, String>>,
    writer: &mut impl Write,
    repl_state: &mut ReplCodexSessionState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    while !repl_state.turn_completed && repl_state.turn_failed.is_none() {
        let message = recv_repl_codex_stdout(stdout_rx, None, "turn output")?;
        handle_repl_codex_app_server_message(&message, writer, repl_state, recorder)?;
    }
    Ok(())
}

fn handle_repl_codex_app_server_message(
    message: &Value,
    writer: &mut impl Write,
    repl_state: &mut ReplCodexSessionState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(());
    };

    if message.get("id").is_some() {
        return handle_repl_codex_app_server_request(method, message, writer, recorder);
    }

    if handle_repl_codex_global_notice(method, message, recorder)? {
        return Ok(());
    }

    handle_repl_codex_app_server_notification(method, message, repl_state, recorder)
}

fn handle_repl_codex_global_notice(
    method: &str,
    message: &Value,
    recorder: &mut dyn TurnRecorder,
) -> Result<bool> {
    let notice = match method {
        "configWarning" => build_shared_codex_global_notice(
            CodexNoticeKind::ConfigWarning,
            CodexNoticeLevel::Warning,
            "Config warning",
            message,
        ),
        "deprecationNotice" => build_shared_codex_global_notice(
            CodexNoticeKind::DeprecationNotice,
            CodexNoticeLevel::Info,
            "Deprecation notice",
            message,
        ),
        _ => return Ok(false),
    };

    let Some(notice) = notice else {
        return Ok(true);
    };

    recorder.finish_streaming_text()?;
    let detail = if notice.title == "Config warning" || notice.title == "Deprecation notice" {
        format!("Codex notice: {}", notice.detail)
    } else {
        format!("Codex notice: {}. {}", notice.title, notice.detail)
    };
    recorder.push_text(&detail)?;
    Ok(true)
}

fn handle_repl_codex_app_server_request(
    method: &str,
    message: &Value,
    writer: &mut impl Write,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("Codex app-server request missing id"))?;
    let params = message
        .get("params")
        .ok_or_else(|| anyhow!("Codex app-server request missing params"))?;

    recorder.finish_streaming_text()?;
    let result = match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command execution");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if cwd.is_empty() && reason.is_empty() {
                "Codex requested approval to execute a command.".to_owned()
            } else if reason.is_empty() {
                format!("Codex requested approval to execute this command in {cwd}.")
            } else if cwd.is_empty() {
                format!("Codex requested approval to execute this command. Reason: {reason}")
            } else {
                format!(
                    "Codex requested approval to execute this command in {cwd}. Reason: {reason}"
                )
            };
            recorder.push_approval("Codex needs approval", command, &detail)?;
            codex_approval_result(
                &CodexApprovalKind::CommandExecution,
                prompt_repl_codex_approval_decision()?,
            )
        }
        "item/fileChange/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if reason.is_empty() {
                "Codex requested approval to apply file changes.".to_owned()
            } else {
                format!("Codex requested approval to apply file changes. Reason: {reason}")
            };
            recorder.push_approval("Codex needs approval", "Apply file changes", &detail)?;
            codex_approval_result(
                &CodexApprovalKind::FileChange,
                prompt_repl_codex_approval_decision()?,
            )
        }
        "item/permissions/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let permissions_summary = describe_codex_permission_request(
                params.get("permissions").unwrap_or(&Value::Null),
            );
            let detail = match (
                reason.trim().is_empty(),
                permissions_summary
                    .as_deref()
                    .filter(|value| !value.is_empty()),
            ) {
                (true, Some(summary)) => {
                    format!("Codex requested approval to grant additional permissions: {summary}.")
                }
                (false, Some(summary)) => format!(
                    "Codex requested approval to grant additional permissions: {summary}. Reason: {reason}"
                ),
                (true, None) => {
                    "Codex requested approval to grant additional permissions.".to_owned()
                }
                (false, None) => format!(
                    "Codex requested approval to grant additional permissions. Reason: {reason}"
                ),
            };
            recorder.push_approval(
                "Codex needs approval",
                "Grant additional permissions",
                &detail,
            )?;
            codex_approval_result(
                &CodexApprovalKind::Permissions {
                    requested_permissions: params
                        .get("permissions")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                },
                prompt_repl_codex_approval_decision()?,
            )
        }
        "item/tool/requestUserInput" => {
            let questions: Vec<UserInputQuestion> = serde_json::from_value(
                params
                    .get("questions")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .context("failed to parse Codex request_user_input questions")?;
            let detail = describe_codex_user_input_request(&questions);
            recorder.push_text(&format!("input> Codex needs input\ninput> {detail}"))?;
            for question in &questions {
                println!("input> {}: {}", question.header, question.question);
                if let Some(options) = question
                    .options
                    .as_ref()
                    .filter(|options| !options.is_empty())
                {
                    let labels = options
                        .iter()
                        .map(|option| option.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    println!("input> options: {labels}");
                }
            }
            let answers = prompt_repl_codex_user_input_answers(&questions)?;
            let (response_answers, _display_answers) =
                validate_codex_user_input_answers(&questions, answers)
                    .map_err(|err| anyhow!(err.message.clone()))?;
            json!({ "answers": response_answers })
        }
        "mcpServer/elicitation/request" => {
            let request: McpElicitationRequestPayload = serde_json::from_value(params.clone())
                .context("failed to parse Codex MCP elicitation request")?;
            let detail = describe_codex_mcp_elicitation_request(&request);
            recorder.push_text(&format!("mcp> Codex needs MCP input\nmcp> {detail}"))?;
            let (action, content) = prompt_repl_codex_mcp_submission(&request)?;
            let content = validate_codex_mcp_elicitation_submission(&request, action, content)
                .map_err(|err| anyhow!(err.message.clone()))?;
            json!({
                "action": match action {
                    McpElicitationAction::Accept => "accept",
                    McpElicitationAction::Decline => "decline",
                    McpElicitationAction::Cancel => "cancel",
                },
                "content": content,
            })
        }
        _ => {
            let (title, detail) = describe_codex_app_server_request(method, params);
            recorder.push_text(&format!("codex-request> {title}\ncodex-request> {detail}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(params).unwrap_or_else(|_| params.to_string())
            );
            prompt_repl_codex_app_request_result()?
        }
    };

    write_codex_json_rpc_message(
        writer,
        &json!({
            "id": request_id,
            "result": result,
        }),
    )
}

fn prompt_repl_codex_approval_decision() -> Result<ApprovalDecision> {
    loop {
        let choice = prompt_repl_line("approval> [a]ccept, [s]ession, [r]eject: ", false)?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "a" | "accept" => return Ok(ApprovalDecision::Accepted),
            "s" | "session" => return Ok(ApprovalDecision::AcceptedForSession),
            "r" | "reject" | "decline" => return Ok(ApprovalDecision::Rejected),
            _ => eprintln!("approval> enter `a`, `s`, or `r`"),
        }
    }
}

fn prompt_repl_codex_user_input_answers(
    questions: &[UserInputQuestion],
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut answers = BTreeMap::new();
    for question in questions {
        let answer = prompt_repl_line(&format!("input> {}: ", question.header.trim()), false)?;
        answers.insert(question.id.clone(), vec![answer]);
    }
    Ok(answers)
}

fn prompt_repl_codex_mcp_submission(
    request: &McpElicitationRequestPayload,
) -> Result<(McpElicitationAction, Option<Value>)> {
    let action = loop {
        let choice = prompt_repl_line("mcp> [a]ccept, [d]ecline, [c]ancel: ", false)?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "a" | "accept" => break McpElicitationAction::Accept,
            "d" | "decline" => break McpElicitationAction::Decline,
            "c" | "cancel" => break McpElicitationAction::Cancel,
            _ => eprintln!("mcp> enter `a`, `d`, or `c`"),
        }
    };

    let content = match (&request.mode, action) {
        (McpElicitationRequestMode::Form { .. }, McpElicitationAction::Accept) => Some(
            prompt_repl_json_block("mcp> enter JSON content, then an empty line:", None)?,
        ),
        _ => None,
    };
    Ok((action, content))
}

fn prompt_repl_codex_app_request_result() -> Result<Value> {
    prompt_repl_json_block(
        "codex-request> enter JSON result, then an empty line (blank for {}):",
        Some(json!({})),
    )
}

fn prompt_repl_line(prompt: &str, allow_empty: bool) -> Result<String> {
    loop {
        print!("{prompt}");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let bytes_read = io::stdin()
            .read_line(&mut line)
            .context("failed to read stdin")?;
        if bytes_read == 0 {
            bail!("stdin closed while waiting for Codex input");
        }

        let trimmed = line.trim().to_owned();
        if allow_empty || !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
}

fn prompt_repl_json_block(prompt: &str, empty_default: Option<Value>) -> Result<Value> {
    println!("{prompt}");
    let mut lines = Vec::new();
    loop {
        print!("json> ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        let bytes_read = io::stdin()
            .read_line(&mut line)
            .context("failed to read stdin")?;
        if bytes_read == 0 {
            bail!("stdin closed while waiting for Codex JSON input");
        }

        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.trim().is_empty() {
            if lines.is_empty() {
                if let Some(default) = empty_default.clone() {
                    return Ok(default);
                }
                eprintln!("json> enter JSON content before submitting");
                continue;
            }
            break;
        }

        lines.push(trimmed.to_owned());
    }

    serde_json::from_str(&lines.join("\n")).context("failed to parse JSON input")
}

fn remember_repl_codex_thread_id(
    repl_state: &mut ReplCodexSessionState,
    recorder: &mut dyn TurnRecorder,
    thread_id: &str,
) -> Result<()> {
    if repl_state.resolved_session_id.as_deref() != Some(thread_id) {
        recorder.note_external_session(thread_id)?;
    }
    repl_state.resolved_session_id = Some(thread_id.to_owned());
    Ok(())
}

fn handle_repl_codex_app_server_notification(
    method: &str,
    message: &Value,
    repl_state: &mut ReplCodexSessionState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = message.pointer("/params/thread/id").and_then(Value::as_str) {
                remember_repl_codex_thread_id(repl_state, recorder, thread_id)?;
            }
        }
        "turn/started" => {
            clear_codex_turn_state(&mut repl_state.turn_state);
            repl_state.current_turn_id = message
                .pointer("/params/turn/id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            repl_state.turn_completed = false;
            recorder.finish_streaming_text()?;
        }
        "turn/completed" => {
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    repl_state.current_turn_id = None;
                    clear_codex_turn_state(&mut repl_state.turn_state);
                    recorder.finish_streaming_text()?;
                    repl_state.turn_failed = Some(summarize_error(error));
                    return Ok(());
                }
            }

            repl_state.current_turn_id = None;
            {
                let mut recorder_ref = DynTurnRecorderRef::new(recorder);
                flush_pending_codex_subagent_results(
                    &mut repl_state.turn_state,
                    &mut recorder_ref,
                )?;
            }
            clear_codex_turn_state(&mut repl_state.turn_state);
            recorder.finish_streaming_text()?;
            repl_state.turn_completed = true;
        }
        "item/started" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                let mut recorder_ref = DynTurnRecorderRef::new(recorder);
                handle_codex_app_server_item_started(item, &mut recorder_ref)?;
            }
        }
        "item/completed" => {
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_repl_codex_app_server_item_completed(
                    item,
                    &mut repl_state.turn_state,
                    recorder,
                )?;
            }
        }
        "item/agentMessage/delta" => {
            let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(item_id) = message.pointer("/params/itemId").and_then(Value::as_str) else {
                return Ok(());
            };
            record_repl_codex_agent_message_delta(
                &mut repl_state.turn_state,
                recorder,
                item_id,
                delta,
            )?;
        }
        "model/rerouted" => {
            handle_repl_codex_model_rerouted(
                message,
                repl_state.current_turn_id.as_deref(),
                recorder,
            )?;
        }
        "thread/compacted" => {
            handle_repl_codex_thread_compacted(
                message,
                repl_state.current_turn_id.as_deref(),
                recorder,
            )?;
        }
        "error" => {
            repl_state.current_turn_id = None;
            clear_codex_turn_state(&mut repl_state.turn_state);
            repl_state.turn_failed =
                Some(summarize_error(message.get("params").unwrap_or(message)));
        }
        "codex/event/item_completed" => {
            handle_repl_codex_event_item_completed(
                message,
                repl_state.current_turn_id.as_deref(),
                &mut repl_state.turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message_content_delta" => {
            handle_repl_codex_event_agent_message_content_delta(
                message,
                repl_state.current_turn_id.as_deref(),
                &mut repl_state.turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message" => {
            handle_repl_codex_event_agent_message(
                message,
                repl_state.current_turn_id.as_deref(),
                &mut repl_state.turn_state,
                recorder,
            )?;
        }
        "codex/event/task_complete" => {
            handle_repl_codex_task_complete(
                message,
                repl_state.current_turn_id.as_deref(),
                &mut repl_state.turn_state,
            )?;
        }
        "thread/archived"
        | "thread/unarchived"
        | "thread/status/changed"
        | "turn/diff/updated"
        | "turn/plan/updated"
        | "item/commandExecution/outputDelta"
        | "item/commandExecution/terminalInteraction"
        | "item/fileChange/outputDelta"
        | "item/plan/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta"
        | "thread/tokenUsage/updated"
        | "thread/name/updated"
        | "thread/closed"
        | "thread/realtime/started"
        | "thread/realtime/itemAdded"
        | "thread/realtime/outputAudio/delta"
        | "thread/realtime/error"
        | "thread/realtime/closed" => {}
        _ if method.starts_with("codex/event/") => {}
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled Codex REPL app-server notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

fn handle_repl_codex_task_complete(
    message: &Value,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
) -> Result<()> {
    let Some(summary) = message
        .pointer("/params/msg/last_agent_message")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let trimmed = summary.trim();
    if trimmed.is_empty()
        || !shared_codex_event_matches_active_turn(
            current_turn_id,
            shared_codex_event_turn_id(message),
        )
    {
        return Ok(());
    }

    buffer_codex_subagent_result(
        turn_state,
        "Subagent completed",
        trimmed,
        message
            .pointer("/params/conversationId")
            .and_then(Value::as_str),
        shared_codex_event_turn_id(message),
    );
    Ok(())
}

fn handle_repl_codex_model_rerouted(
    message: &Value,
    current_turn_id: Option<&str>,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !shared_codex_event_matches_active_turn(current_turn_id, shared_codex_event_turn_id(message))
    {
        return Ok(());
    }

    let Some(from_model) = message.pointer("/params/fromModel").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(to_model) = message.pointer("/params/toModel").and_then(Value::as_str) else {
        return Ok(());
    };
    if from_model == to_model {
        return Ok(());
    }

    let reason = match message.pointer("/params/reason").and_then(Value::as_str) {
        Some("highRiskCyberActivity") => " because it detected high-risk cyber activity",
        Some(_) | None => "",
    };
    recorder.finish_streaming_text()?;
    recorder.push_text(&format!(
        "Codex rerouted this turn from `{from_model}` to `{to_model}`{reason}."
    ))
}

fn handle_repl_codex_thread_compacted(
    message: &Value,
    current_turn_id: Option<&str>,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !shared_codex_event_matches_active_turn(current_turn_id, shared_codex_event_turn_id(message))
    {
        return Ok(());
    }

    recorder.finish_streaming_text()?;
    recorder.push_text("Codex compacted the thread context for this turn.")
}

fn record_repl_codex_completed_agent_message(
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
    item_id: &str,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if turn_state.current_agent_message_id.as_deref() != Some(item_id) {
        recorder.finish_streaming_text()?;
        turn_state.current_agent_message_id = Some(item_id.to_owned());
    }

    if !turn_state.streamed_agent_message_item_ids.contains(item_id) {
        let mut recorder_ref = DynTurnRecorderRef::new(recorder);
        begin_codex_assistant_output(turn_state, &mut recorder_ref)?;
        return recorder.push_text(trimmed);
    }

    let entry = turn_state
        .streamed_agent_message_text_by_item_id
        .entry(item_id.to_owned())
        .or_default();
    let update = next_completed_codex_text_update(entry, trimmed);
    if matches!(update, CompletedTextUpdate::NoChange) {
        return Ok(());
    }

    let mut recorder_ref = DynTurnRecorderRef::new(recorder);
    begin_codex_assistant_output(turn_state, &mut recorder_ref)?;
    match update {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }
}

fn handle_repl_codex_event_item_completed(
    message: &Value,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !shared_codex_event_matches_active_turn(current_turn_id, shared_codex_event_turn_id(message))
    {
        return Ok(());
    }

    let Some(item) = message.pointer("/params/msg/item") else {
        return Ok(());
    };
    match item.get("type").and_then(Value::as_str) {
        Some("AgentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let text = item
                .get("content")
                .and_then(Value::as_array)
                .and_then(|content| concatenate_codex_text_parts(content));
            if let Some(text) = text.as_deref() {
                record_repl_codex_completed_agent_message(turn_state, recorder, item_id, text)?;
            }
        }
        Some("CommandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn handle_repl_codex_event_agent_message_content_delta(
    message: &Value,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !shared_codex_event_matches_active_turn(current_turn_id, shared_codex_event_turn_id(message))
    {
        return Ok(());
    }

    let Some(delta) = message.pointer("/params/msg/delta").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(item_id) = message
        .pointer("/params/msg/item_id")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };

    record_repl_codex_agent_message_delta(turn_state, recorder, item_id, delta)
}

fn handle_repl_codex_event_agent_message(
    message: &Value,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !shared_codex_event_matches_active_turn(current_turn_id, shared_codex_event_turn_id(message))
    {
        return Ok(());
    }

    let Some(text) = message
        .pointer("/params/msg/message")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if let Some(item_id) = turn_state.current_agent_message_id.clone() {
        return record_repl_codex_completed_agent_message(turn_state, recorder, &item_id, trimmed);
    }

    let mut recorder_ref = DynTurnRecorderRef::new(recorder);
    begin_codex_assistant_output(turn_state, &mut recorder_ref)?;
    recorder.push_text(trimmed)
}

fn handle_repl_codex_app_server_item_completed(
    item: &Value,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                record_repl_codex_completed_agent_message(turn_state, recorder, item_id, text)?;
            }
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        Some("fileChange") => {
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return Ok(());
            }
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return Ok(());
            };
            for change in changes {
                let Some(file_path) = change.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
                if diff.trim().is_empty() {
                    continue;
                }
                let change_type = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => ChangeType::Create,
                    _ => ChangeType::Edit,
                };
                let summary = match change_type {
                    ChangeType::Create => format!("Created {}", short_file_name(file_path)),
                    ChangeType::Edit => format!("Updated {}", short_file_name(file_path)),
                };
                recorder.push_diff(file_path, &summary, diff, change_type)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            let output = summarize_codex_app_server_web_search_output(item);
            recorder.command_completed(key, &command, &output, CommandStatus::Success)?;
        }
        _ => {}
    }

    Ok(())
}

fn record_repl_codex_agent_message_delta(
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
    item_id: &str,
    delta: &str,
) -> Result<()> {
    if turn_state.current_agent_message_id.as_deref() != Some(item_id) {
        recorder.finish_streaming_text()?;
        turn_state.current_agent_message_id = Some(item_id.to_owned());
    }
    let entry = turn_state
        .streamed_agent_message_text_by_item_id
        .entry(item_id.to_owned())
        .or_default();
    let Some(unseen_suffix) = next_codex_delta_suffix(entry, delta) else {
        return Ok(());
    };

    {
        let mut recorder_ref = DynTurnRecorderRef::new(recorder);
        begin_codex_assistant_output(turn_state, &mut recorder_ref)?;
    }
    turn_state
        .streamed_agent_message_item_ids
        .insert(item_id.to_owned());
    recorder.text_delta(&unseen_suffix)
}

fn shutdown_repl_codex_process(
    process: &Arc<SharedChild>,
) -> Result<(std::process::ExitStatus, bool)> {
    if let Some(status) =
        wait_for_shared_child_exit_timeout(process, Duration::from_secs(1), "Codex app-server")?
    {
        return Ok((status, false));
    }

    kill_child_process(process, "Codex app-server")?;
    let status = process
        .wait()
        .context("failed waiting for Codex app-server process")?;
    Ok((status, true))
}

fn default_codex_sandbox_mode() -> CodexSandboxMode {
    match std::env::var("TERMAL_CODEX_SANDBOX").ok().as_deref() {
        Some("read-only") => CodexSandboxMode::ReadOnly,
        Some("danger-full-access") => CodexSandboxMode::DangerFullAccess,
        _ => CodexSandboxMode::WorkspaceWrite,
    }
}

fn default_codex_approval_policy() -> CodexApprovalPolicy {
    match std::env::var("TERMAL_CODEX_APPROVAL").ok().as_deref() {
        Some("untrusted") => CodexApprovalPolicy::Untrusted,
        Some("on-request") => CodexApprovalPolicy::OnRequest,
        Some("on-failure") => CodexApprovalPolicy::OnFailure,
        _ => CodexApprovalPolicy::Never,
    }
}

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

fn default_claude_approval_mode() -> ClaudeApprovalMode {
    ClaudeApprovalMode::Ask
}

fn default_claude_effort() -> ClaudeEffortLevel {
    ClaudeEffortLevel::Default
}

const LOCAL_REMOTE_ID: &str = "local";
const LOCAL_REMOTE_NAME: &str = "Local";
const DEFAULT_SSH_REMOTE_PORT: u16 = 22;

fn default_local_remote_id() -> String {
    LOCAL_REMOTE_ID.to_owned()
}

fn default_remote_enabled() -> bool {
    true
}

fn default_remote_configs() -> Vec<RemoteConfig> {
    vec![RemoteConfig::local()]
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum RemoteTransport {
    Local,
    Ssh,
}

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppPreferences {
    #[serde(default = "default_codex_reasoning_effort")]
    default_codex_reasoning_effort: CodexReasoningEffort,
    #[serde(default = "default_claude_effort")]
    default_claude_effort: ClaudeEffortLevel,
    #[serde(default = "default_remote_configs")]
    remotes: Vec<RemoteConfig>,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            default_codex_reasoning_effort: default_codex_reasoning_effort(),
            default_claude_effort: default_claude_effort(),
            remotes: default_remote_configs(),
        }
    }
}

fn default_cursor_mode() -> CursorMode {
    CursorMode::Agent
}

fn default_gemini_approval_mode() -> GeminiApprovalMode {
    GeminiApprovalMode::Default
}

fn log_unhandled_codex_event(context: &str, message: &Value) {
    eprintln!("codex diagnostic> {context}: {message}");
}

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
    command.current_dir(&cwd).args([
        "--model",
        model,
        "-p",
        "--verbose",
        "--output-format",
        "stream-json",
        "--include-partial-messages",
    ]);
    if let Some(permission_mode) = approval_mode.initial_cli_permission_mode() {
        command.args(["--permission-mode", permission_mode]);
    }
    if let Some(effort) = effort.as_cli_value() {
        command.args(["--effort", effort]);
    }

    let expected_session_id = match session_id {
        Some(session_id) => {
            command.args(["--resume", session_id]);
            session_id.to_owned()
        }
        None => {
            let session_id = Uuid::new_v4().to_string();
            command.args(["--session-id", &session_id]);
            session_id
        }
    };

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

struct ClaudeToolUse {
    command: Option<String>,
    description: Option<String>,
    file_path: Option<String>,
    name: String,
    subagent_type: Option<String>,
}

struct ClaudeToolPermissionRequest {
    detail: String,
    permission_mode_for_session: Option<String>,
    request_id: String,
    title: String,
    tool_name: String,
    tool_input: Value,
}

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

fn finish_claude_assistant_text_stream<R: TurnRecorder + ?Sized>(
    state: &mut ClaudeTurnState,
    recorder: &mut R,
) -> Result<()> {
    recorder.finish_streaming_text()?;
    state.streamed_assistant_text.clear();
    state.saw_text_delta = false;
    Ok(())
}

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
            finish_claude_assistant_text_stream(state, recorder)?;
            state.approval_keys_this_turn.clear();
            state.parallel_agent_group_key = None;
            state.parallel_agent_order.clear();
            state.parallel_agents.clear();
            state.permission_denied_this_turn = false;

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
fn is_permission_denial(detail: &str) -> bool {
    detail.contains("requested permissions")
}

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

fn describe_claude_tool_request(request: &ClaudeToolPermissionRequest) -> String {
    describe_claude_tool_action_from_parts(&request.tool_name, &request.tool_input)
}

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

fn short_file_name(file_path: &str) -> &str {
    file_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(file_path)
}

fn summarize_error(value: &Value) -> String {
    summarize_structured_error(value).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    })
}

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

fn is_retryable_connectivity_error(value: &Value) -> bool {
    codex_error_will_retry(value) && has_connectivity_marker(value)
}

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

fn trimmed_string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn is_connectivity_text(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("stream disconnected before completion")
        || normalized.contains("websocket closed by server before response.completed")
        || normalized.contains("response stream disconnected")
        || normalized.contains("connection dropped")
        || normalized.contains("reconnecting")
}

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

fn image_attachment_summary(count: usize) -> String {
    match count {
        0 => "Waiting for activity.".to_owned(),
        1 => "1 image attached".to_owned(),
        count => format!("{count} images attached"),
    }
}

fn prompt_preview_text(text: &str, attachments: &[MessageImageAttachment]) -> String {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        return make_preview(trimmed);
    }

    make_preview(&image_attachment_summary(attachments.len()))
}

fn shell_language() -> &'static str {
    "bash"
}

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

fn command_token_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '|' | '&' | ';')
}

fn is_file_viewer_command(token: &str) -> bool {
    matches!(
        token,
        "bat" | "cat" | "head" | "less" | "more" | "sed" | "tail"
    )
}

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

fn codex_image_data_url(attachment: &PromptImageAttachment) -> String {
    format!(
        "data:{};base64,{}",
        attachment.metadata.media_type, attachment.data
    )
}

fn parse_prompt_image_attachments(
    requests: &[SendMessageAttachmentRequest],
) -> std::result::Result<Vec<PromptImageAttachment>, ApiError> {
    requests
        .iter()
        .enumerate()
        .map(|(index, request)| parse_prompt_image_attachment(index, request))
        .collect()
}

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

fn stamp_now() -> String {
    Local::now().format("%H:%M").to_string()
}
