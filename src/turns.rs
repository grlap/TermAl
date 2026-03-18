fn run_turn_blocking(config: TurnConfig, recorder: &mut dyn TurnRecorder) -> Result<String> {
    match config.agent {
        Agent::Codex => run_codex_turn(
            None,
            None,
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
    fn finish_streaming_text(&mut self) -> Result<()>;
    fn command_started(&mut self, key: &str, command: &str) -> Result<()>;
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()>;
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
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
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
        self.state
            .register_claude_pending_approval(&self.session_id, message_id, approval)
    }

    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
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
        self.state
            .register_codex_pending_approval(&self.session_id, message_id, approval)
    }

    fn push_acp_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
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
        self.state
            .register_acp_pending_approval(&self.session_id, message_id, approval)
    }
}

impl CodexTurnRecorder for SessionRecorder {
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        SessionRecorder::push_codex_approval(self, title, command, detail, approval)
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

    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            self.session_id,
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
        self.state
            .register_codex_pending_approval(self.session_id, message_id, approval)
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
        BorrowedSessionRecorder::push_codex_approval(self, title, command, detail, approval)
    }
}

impl TurnRecorder for SessionRecorder {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        self.state
            .set_external_session_id(&self.session_id, session_id.to_owned())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Approval {
                id: self.state.allocate_message_id(),
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

    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: trimmed.to_owned(),
                expanded_text: None,
            },
        )
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::SubagentResult {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                summary: trimmed.to_owned(),
                conversation_id: conversation_id.map(str::to_owned),
                turn_id: turn_id.map(str::to_owned),
            },
        )
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        let message_id = match &self.recorder_state.streaming_text_message_id {
            Some(message_id) => message_id.clone(),
            None => {
                let message_id = self.state.allocate_message_id();
                self.state.push_message(
                    &self.session_id,
                    Message::Text {
                        attachments: Vec::new(),
                        id: message_id.clone(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: String::new(),
                        expanded_text: None,
                    },
                )?;
                self.recorder_state.streaming_text_message_id = Some(message_id.clone());
                message_id
            }
        };

        self.state
            .append_text_delta(&self.session_id, &message_id, delta)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Thinking {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                lines,
            },
        )
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        if diff.trim().is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            &self.session_id,
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

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.recorder_state.streaming_text_message_id = None;
        Ok(())
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        let message_id = self
            .recorder_state
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state.upsert_command_message(
            &self.session_id,
            &message_id,
            command,
            "",
            CommandStatus::Running,
        )
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let message_id = self
            .recorder_state
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state
            .upsert_command_message(&self.session_id, &message_id, command, output, status)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            &self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("Error: {cleaned}"),
                expanded_text: None,
            },
        )
    }
}

impl TurnRecorder for BorrowedSessionRecorder<'_> {
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        self.state
            .set_external_session_id(self.session_id, session_id.to_owned())
    }

    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.finish_streaming_text()?;
        self.state.push_message(
            self.session_id,
            Message::Approval {
                id: self.state.allocate_message_id(),
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

    fn push_text(&mut self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: trimmed.to_owned(),
                expanded_text: None,
            },
        )
    }

    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()> {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            self.session_id,
            Message::SubagentResult {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                summary: trimmed.to_owned(),
                conversation_id: conversation_id.map(str::to_owned),
                turn_id: turn_id.map(str::to_owned),
            },
        )
    }

    fn text_delta(&mut self, delta: &str) -> Result<()> {
        if delta.is_empty() {
            return Ok(());
        }

        let message_id = match &self.recorder_state.streaming_text_message_id {
            Some(message_id) => message_id.clone(),
            None => {
                let message_id = self.state.allocate_message_id();
                self.state.push_message(
                    self.session_id,
                    Message::Text {
                        attachments: Vec::new(),
                        id: message_id.clone(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: String::new(),
                        expanded_text: None,
                    },
                )?;
                self.recorder_state.streaming_text_message_id = Some(message_id.clone());
                message_id
            }
        };

        self.state
            .append_text_delta(self.session_id, &message_id, delta)
    }

    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        if lines.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            self.session_id,
            Message::Thinking {
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: title.to_owned(),
                lines,
            },
        )
    }

    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        if diff.trim().is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        let message_id = self.state.allocate_message_id();
        self.state.push_message(
            self.session_id,
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

    fn finish_streaming_text(&mut self) -> Result<()> {
        self.recorder_state.streaming_text_message_id = None;
        Ok(())
    }

    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        let message_id = self
            .recorder_state
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state.upsert_command_message(
            self.session_id,
            &message_id,
            command,
            "",
            CommandStatus::Running,
        )
    }

    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let message_id = self
            .recorder_state
            .command_messages
            .entry(key.to_owned())
            .or_insert_with(|| self.state.allocate_message_id())
            .clone();

        self.state
            .upsert_command_message(self.session_id, &message_id, command, output, status)
    }

    fn error(&mut self, detail: &str) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        self.finish_streaming_text()?;
        self.state.push_message(
            self.session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: format!("Error: {cleaned}"),
                expanded_text: None,
            },
        )
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
    state: Option<&AppState>,
    runtime_session_id: Option<&str>,
    cwd: &str,
    external_session_id: Option<&str>,
    model: &str,
    sandbox_mode: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    reasoning_effort: CodexReasoningEffort,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let codex_home = prepare_termal_codex_home(cwd, runtime_session_id.unwrap_or("repl"))?;
    let mut command = codex_command()?;
    command
        .env("CODEX_HOME", &codex_home)
        .args(["-m", model, "-c"])
        .arg(format!(
            "model_reasoning_effort=\"{}\"",
            reasoning_effort.as_api_value()
        ));

    match external_session_id {
        Some(session_id) => {
            command.args([
                "-a",
                approval_policy.as_cli_value(),
                "exec",
                "resume",
                "--json",
                session_id,
                "-",
            ]);
        }
        None => {
            command.args([
                "-a",
                approval_policy.as_cli_value(),
                "exec",
                "-s",
                sandbox_mode.as_cli_value(),
                "--json",
                "-C",
                cwd,
                "-",
            ]);
        }
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Codex")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture child stdin")?;
    writeln!(child_stdin, "{prompt}").context("failed to write prompt to Codex stdin")?;
    drop(child_stdin);

    let stdout = child
        .stdout
        .take()
        .context("failed to capture child stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture child stderr")?;
    let process = Arc::new(Mutex::new(child));
    if let (Some(state), Some(runtime_session_id)) = (state, runtime_session_id) {
        let (input_tx, _input_rx) = mpsc::channel();
        let runtime = CodexRuntimeHandle {
            runtime_id: Uuid::new_v4().to_string(),
            input_tx,
            process: process.clone(),
            shared_session: None,
        };

        if let Err(err) = state.set_codex_runtime(runtime_session_id, runtime) {
            let _ = kill_child_process(&process, "Codex");
            return Err(err).context("failed to register active Codex runtime");
        }
    }
    let mut rollout_streamer = match (state, runtime_session_id, external_session_id) {
        (Some(state), Some(runtime_session_id), Some(thread_id)) => {
            let path = wait_for_codex_rollout_path(&codex_home, thread_id)?;
            path.map(|path| {
                let start_offset = fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                spawn_codex_rollout_streamer(
                    state.clone(),
                    runtime_session_id.to_owned(),
                    path,
                    start_offset,
                )
            })
        }
        _ => None,
    };

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut reader = BufReader::new(stdout);
    let mut resolved_session_id = external_session_id.map(str::to_owned);
    let mut deferred_stdout_agent_message: Option<String> = None;
    let mut raw_line = String::new();

    loop {
        raw_line.clear();
        let bytes_read = reader
            .read_line(&mut raw_line)
            .context("failed to read stdout from Codex")?;

        if bytes_read == 0 {
            break;
        }

        let message: Value = serde_json::from_str(raw_line.trim_end())
            .with_context(|| format!("failed to parse Codex JSON line: {}", raw_line.trim_end()))?;

        if rollout_streamer.is_none() {
            if let (Some(state), Some(runtime_session_id)) = (state, runtime_session_id) {
                if message.get("type").and_then(Value::as_str) == Some("thread.started") {
                    if let Some(thread_id) = message.get("thread_id").and_then(Value::as_str) {
                        if let Some(path) = wait_for_codex_rollout_path(&codex_home, thread_id)? {
                            rollout_streamer = Some(spawn_codex_rollout_streamer(
                                state.clone(),
                                runtime_session_id.to_owned(),
                                path,
                                0,
                            ));
                        }
                    }
                }
            }
        }

        handle_codex_event(
            &message,
            &mut resolved_session_id,
            recorder,
            if rollout_streamer.is_some() {
                Some(&mut deferred_stdout_agent_message)
            } else {
                None
            },
        )?;
    }

    let status = {
        let mut child = process.lock().expect("Codex process mutex poisoned");
        child.wait().context("failed waiting for Codex process")?
    };
    let mut rollout_saw_final_answer = false;
    if let Some(streamer) = rollout_streamer {
        streamer.stop.store(true, Ordering::SeqCst);
        let _ = streamer.join.join();
        rollout_saw_final_answer = streamer.saw_final_answer.load(Ordering::SeqCst);
    }
    if !rollout_saw_final_answer {
        if let Some(text) = deferred_stdout_agent_message.take() {
            recorder.push_text(&text)?;
        }
    }
    let stderr_lines = stderr_thread.join().unwrap_or_default();

    if !status.success() {
        let stderr_output = stderr_lines.join("\n");
        if stderr_output.trim().is_empty() {
            bail!("Codex exited with status {status}");
        } else {
            bail!("Codex exited with status {status}: {stderr_output}");
        }
    }

    resolved_session_id.ok_or_else(|| anyhow!("Codex completed without emitting a thread id"))
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppPreferences {
    #[serde(default = "default_codex_reasoning_effort")]
    default_codex_reasoning_effort: CodexReasoningEffort,
    #[serde(default = "default_claude_effort")]
    default_claude_effort: ClaudeEffortLevel,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            default_codex_reasoning_effort: default_codex_reasoning_effort(),
            default_claude_effort: default_claude_effort(),
        }
    }
}

fn default_cursor_mode() -> CursorMode {
    CursorMode::Agent
}

fn default_gemini_approval_mode() -> GeminiApprovalMode {
    GeminiApprovalMode::Default
}

fn handle_codex_event(
    message: &Value,
    session_id: &mut Option<String>,
    recorder: &mut dyn TurnRecorder,
    deferred_stdout_agent_message: Option<&mut Option<String>>,
) -> Result<()> {
    let Some(event_type) = message.get("type").and_then(Value::as_str) else {
        log_unhandled_codex_event("missing top-level event type", message);
        return Ok(());
    };

    match event_type {
        "turn.started" | "turn.completed" => {}
        "thread.started" => {
            let thread_id = get_string(message, &["thread_id"])?;
            *session_id = Some(thread_id.to_owned());
            recorder.note_external_session(thread_id)?;
        }
        "item.started" => match message.pointer("/item/type").and_then(Value::as_str) {
            Some("command_execution") => {
                if let Some(command) = message.pointer("/item/command").and_then(Value::as_str) {
                    let key = codex_item_key(message, command);
                    recorder.command_started(&key, command)?;
                }
            }
            Some("web_search") => {
                let key = codex_item_key(message, "web_search");
                let command = describe_codex_web_search_command(message);
                recorder.command_started(&key, &command)?;
            }
            Some(item_type) => {
                log_unhandled_codex_event(
                    &format!("unhandled Codex item.started type `{item_type}`"),
                    message,
                );
            }
            None => {
                log_unhandled_codex_event("Codex item.started missing item.type", message);
            }
        },
        "item.completed" => {
            let Some(item_type) = message.pointer("/item/type").and_then(Value::as_str) else {
                log_unhandled_codex_event("Codex item.completed missing item.type", message);
                return Ok(());
            };

            match item_type {
                "agent_message" => {
                    if let Some(text) = message.pointer("/item/text").and_then(Value::as_str) {
                        if let Some(slot) = deferred_stdout_agent_message {
                            *slot = Some(text.to_owned());
                        } else {
                            recorder.push_text(text)?;
                        }
                    }
                }
                "command_execution" => {
                    if let Some(command) = message.pointer("/item/command").and_then(Value::as_str)
                    {
                        let key = codex_item_key(message, command);
                        let output = message
                            .pointer("/item/aggregated_output")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let exit_code = message.pointer("/item/exit_code").and_then(Value::as_i64);
                        let status = if exit_code.unwrap_or(-1) == 0 {
                            CommandStatus::Success
                        } else {
                            CommandStatus::Error
                        };
                        recorder.command_completed(&key, command, output, status)?;
                    }
                }
                "web_search" => {
                    let key = codex_item_key(message, "web_search");
                    let command = describe_codex_web_search_command(message);
                    let output = summarize_codex_web_search_output(message);
                    recorder.command_completed(&key, &command, &output, CommandStatus::Success)?;
                }
                _ => {
                    log_unhandled_codex_event(
                        &format!("unhandled Codex item.completed type `{item_type}`"),
                        message,
                    );
                }
            }
        }
        "error" => {
            recorder.error(&summarize_error(message))?;
        }
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled Codex event type `{event_type}`"),
                message,
            );
        }
    }

    Ok(())
}

fn describe_codex_web_search_command(message: &Value) -> String {
    let query = message
        .pointer("/item/query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match message.pointer("/item/action/type").and_then(Value::as_str) {
        Some("open_page") => message
            .pointer("/item/action/url")
            .and_then(Value::as_str)
            .map(|url| format!("Open page: {url}"))
            .unwrap_or_else(|| "Open page".to_owned()),
        Some("find_in_page") => message
            .pointer("/item/action/pattern")
            .and_then(Value::as_str)
            .map(|pattern| format!("Find in page: {pattern}"))
            .unwrap_or_else(|| "Find in page".to_owned()),
        Some("search") | Some("other") | None | Some(_) => query
            .map(|value| format!("Web search: {value}"))
            .unwrap_or_else(|| "Web search".to_owned()),
    }
}

fn summarize_codex_web_search_output(message: &Value) -> String {
    match message.pointer("/item/action/type").and_then(Value::as_str) {
        Some("search") => {
            let queries = message
                .pointer("/item/action/queries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            if !queries.is_empty() {
                return queries.join("\n");
            }
        }
        Some("open_page") => {
            if let Some(url) = message.pointer("/item/action/url").and_then(Value::as_str) {
                return format!("Opened {url}");
            }
        }
        Some("find_in_page") => {
            let pattern = message
                .pointer("/item/action/pattern")
                .and_then(Value::as_str);
            let url = message.pointer("/item/action/url").and_then(Value::as_str);
            return match (pattern, url) {
                (Some(pattern), Some(url)) => format!("Searched for `{pattern}` in {url}"),
                (Some(pattern), None) => format!("Searched for `{pattern}`"),
                (None, Some(url)) => format!("Searched within {url}"),
                (None, None) => "Find in page completed".to_owned(),
            };
        }
        _ => {}
    }

    message
        .pointer("/item/query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Web search completed")
        .to_owned()
}

fn extract_codex_rollout_agent_message(message: &Value) -> Option<(String, String)> {
    let payload = message.get("payload")?;
    if message.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }

    if payload.get("type").and_then(Value::as_str) != Some("agent_message") {
        return None;
    }

    let text = payload.get("message").and_then(Value::as_str)?.trim();
    if text.is_empty() {
        return None;
    }

    let phase = payload
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("message");

    Some((phase.to_owned(), text.to_owned()))
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
    let mut command = Command::new("claude");
    command.current_dir(cwd).args([
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
    permission_denied_this_turn: bool,
    pending_tools: HashMap<String, ClaudeToolUse>,
    saw_text_delta: bool,
}

struct ClaudeToolUse {
    command: Option<String>,
    file_path: Option<String>,
    name: String,
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
                            let text = if state.saw_text_delta {
                                text
                            } else {
                                text.trim_start_matches('\n')
                            };
                            if !text.is_empty() {
                                recorder.text_delta(text)?;
                                state.saw_text_delta = true;
                            }
                        }
                    }
                }
                "message_stop" => {
                    recorder.finish_streaming_text()?;
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
                        "text" if !state.saw_text_delta => {
                            if let Some(text) = content.get("text").and_then(Value::as_str) {
                                if state.permission_denied_this_turn {
                                    continue;
                                }
                                recorder.push_text(text)?;
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = content.get("thinking").and_then(Value::as_str)
                            {
                                let lines = split_thinking_lines(thinking);
                                recorder.push_thinking("Thinking", lines)?;
                            }
                        }
                        "tool_use" => {
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
            recorder.finish_streaming_text()?;
            state.saw_text_delta = false;
            state.approval_keys_this_turn.clear();
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
    let file_path = input
        .and_then(|value| value.get("file_path").or_else(|| value.get("filePath")))
        .and_then(Value::as_str)
        .map(str::to_owned);

    state.pending_tools.insert(
        tool_id.to_owned(),
        ClaudeToolUse {
            command: command.clone(),
            file_path,
            name: name.to_owned(),
        },
    );

    if name == "Bash" {
        let description = input
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str);
        let command_label = command.as_deref().or(description).unwrap_or("Bash");
        recorder.command_started(tool_id, command_label)?;
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

fn codex_item_key(message: &Value, command: &str) -> String {
    message
        .pointer("/item/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("command:{command}"))
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
