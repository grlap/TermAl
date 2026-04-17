// REPL (blocking interactive) Codex runtime.
//
// When TermAl is invoked as `termal repl codex` / `termal cli codex` /
// plain `termal codex`, this module runs one turn at a time against a
// per-session Codex app-server subprocess and prints transcript events
// to stdout. It is the single-shot counterpart to the shared-Codex
// runtime in src/codex.rs (which multiplexes N concurrent sessions over
// one long-lived helper for server mode).
//
// Covers: `ReplCodexSessionState`, `DynTurnRecorderRef`, the stdout-reader
// thread, JSON-RPC request plumbing, the main pump loop, message + global
// notice + app-server request handlers, interactive approval / user-input /
// MCP-elicitation / app-request prompts, thread-id remembering, event
// dispatch (task complete, agent message delta + final, item completed,
// model rerouted, thread compacted), and process shutdown.
//
// Extracted from turns.rs into its own `include!()` fragment so turns.rs
// stays focused on the TurnRecorder abstraction + Claude turn handling.

/// Tracks REPL Codex session state.
#[derive(Default)]
struct ReplCodexSessionState {
    resolved_session_id: Option<String>,
    current_turn_id: Option<String>,
    completed_turn_id: Option<String>,
    turn_state: CodexTurnState,
    turn_completed: bool,
    turn_failed: Option<String>,
}

/// Represents dyn turn recorder ref.
struct DynTurnRecorderRef<'a> {
    inner: &'a mut dyn TurnRecorder,
}

impl<'a> DynTurnRecorderRef<'a> {
    /// Creates a new instance.
    fn new(inner: &'a mut dyn TurnRecorder) -> Self {
        Self { inner }
    }
}

impl TurnRecorder for DynTurnRecorderRef<'_> {
    /// Records external session.
    fn note_external_session(&mut self, session_id: &str) -> Result<()> {
        self.inner.note_external_session(session_id)
    }

    /// Pushes approval.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()> {
        self.inner.push_approval(title, command, detail)
    }

    /// Pushes text.
    fn push_text(&mut self, text: &str) -> Result<()> {
        self.inner.push_text(text)
    }

    /// Pushes subagent result.
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

    /// Pushes thinking.
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()> {
        self.inner.push_thinking(title, lines)
    }

    /// Pushes diff.
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()> {
        self.inner.push_diff(file_path, summary, diff, change_type)
    }

    /// Handles text delta.
    fn text_delta(&mut self, delta: &str) -> Result<()> {
        self.inner.text_delta(delta)
    }

    /// Replaces streaming text.
    fn replace_streaming_text(&mut self, text: &str) -> Result<()> {
        self.inner.replace_streaming_text(text)
    }

    /// Finishes streaming text.
    fn finish_streaming_text(&mut self) -> Result<()> {
        self.inner.finish_streaming_text()
    }

    /// Resets turn state.
    fn reset_turn_state(&mut self) -> Result<()> {
        self.inner.reset_turn_state()
    }

    /// Handles command started.
    fn command_started(&mut self, key: &str, command: &str) -> Result<()> {
        self.inner.command_started(key, command)
    }

    /// Handles command completed.
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        self.inner.command_completed(key, command, output, status)
    }

    /// Upserts parallel agents.
    fn upsert_parallel_agents(
        &mut self,
        key: &str,
        agents: &[ParallelAgentProgress],
    ) -> Result<()> {
        self.inner.upsert_parallel_agents(key, agents)
    }

    /// Handles error.
    fn error(&mut self, detail: &str) -> Result<()> {
        self.inner.error(detail)
    }
}

/// Spawns REPL Codex stdout reader.
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

/// Handles send REPL Codex JSON RPC request.
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
        &json_rpc_request_message(request_id.clone(), method, params),
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

/// Handles recv REPL Codex stdout.
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

/// Handles pump REPL Codex turn.
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

/// Handles REPL Codex app server message.
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

/// Handles REPL Codex global notice.
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

/// Handles REPL Codex app server request.
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
        &codex_json_rpc_response_message(&CodexJsonRpcResponseCommand {
            request_id,
            payload: CodexJsonRpcResponsePayload::Result(result),
        }),
    )
}

/// Handles prompt REPL Codex approval decision.
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

/// Handles prompt REPL Codex user input answers.
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

/// Handles prompt REPL Codex MCP submission.
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

/// Handles prompt REPL Codex app request result.
fn prompt_repl_codex_app_request_result() -> Result<Value> {
    prompt_repl_json_block(
        "codex-request> enter JSON result, then an empty line (blank for {}):",
        Some(json!({})),
    )
}

/// Handles prompt REPL line.
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

/// Handles prompt REPL JSON block.
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

/// Remembers REPL Codex thread ID.
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

/// Handles REPL Codex app server notification.
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
            repl_state.completed_turn_id = None;
            repl_state.turn_completed = false;
            recorder.finish_streaming_text()?;
        }
        "turn/completed" => {
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    repl_state.current_turn_id = None;
                    repl_state.completed_turn_id = None;
                    clear_codex_turn_state(&mut repl_state.turn_state);
                    recorder.finish_streaming_text()?;
                    repl_state.turn_failed = Some(summarize_error(error));
                    return Ok(());
                }
            }

            repl_state.completed_turn_id = repl_state.current_turn_id.take();
            {
                let mut recorder_ref = DynTurnRecorderRef::new(recorder);
                flush_pending_codex_subagent_results(
                    &mut repl_state.turn_state,
                    &mut recorder_ref,
                )?;
            }
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
                let allow_late_agent_message = repl_state.current_turn_id.is_none()
                    && repl_state.completed_turn_id.is_some()
                    && matches!(item.get("type").and_then(Value::as_str), Some("agentMessage"));
                if repl_state.current_turn_id.is_some() || allow_late_agent_message {
                    handle_repl_codex_app_server_item_completed(
                        item,
                        &mut repl_state.turn_state,
                        recorder,
                    )?;
                }
            }
        }
        "item/agentMessage/delta" => {
            if repl_state.current_turn_id.is_none() && repl_state.completed_turn_id.is_none() {
                return Ok(());
            }
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
            repl_state.completed_turn_id = None;
            clear_codex_turn_state(&mut repl_state.turn_state);
            repl_state.turn_failed =
                Some(summarize_error(message.get("params").unwrap_or(message)));
        }
        "codex/event/item_completed" => {
            handle_repl_codex_event_item_completed(
                message,
                repl_state.current_turn_id.as_deref(),
                repl_state.completed_turn_id.as_deref(),
                &mut repl_state.turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message_content_delta" => {
            handle_repl_codex_event_agent_message_content_delta(
                message,
                repl_state.current_turn_id.as_deref(),
                repl_state.completed_turn_id.as_deref(),
                &mut repl_state.turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message" => {
            handle_repl_codex_event_agent_message(
                message,
                repl_state.current_turn_id.as_deref(),
                repl_state.completed_turn_id.as_deref(),
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

/// Handles REPL Codex task complete.
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

/// Matches REPL Codex final-output events against either the active turn or
/// the most recently completed turn.
fn repl_codex_event_matches_visible_turn(
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    event_turn_id: Option<&str>,
) -> bool {
    if shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return true;
    }

    matches!(
        (current_turn_id, completed_turn_id, event_turn_id),
        (None, Some(completed), Some(event)) if completed == event
    )
}

/// Handles REPL Codex model rerouted.
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

/// Handles REPL Codex thread compacted.
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

/// Records REPL Codex completed agent message.
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

/// Handles REPL Codex event item completed.
fn handle_repl_codex_event_item_completed(
    message: &Value,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !repl_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        shared_codex_event_turn_id(message),
    ) {
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

/// Handles REPL Codex event agent message content delta.
fn handle_repl_codex_event_agent_message_content_delta(
    message: &Value,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !repl_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        shared_codex_event_turn_id(message),
    ) {
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

/// Handles REPL Codex event agent message.
fn handle_repl_codex_event_agent_message(
    message: &Value,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    if !repl_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        shared_codex_event_turn_id(message),
    ) {
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

/// Handles REPL Codex app server item completed.
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

/// Records REPL Codex agent message delta.
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

/// Shuts down REPL Codex process.
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
