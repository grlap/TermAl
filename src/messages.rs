// Session message bookkeeping + Codex thread → Message conversion.
//
// The first half covers generic per-SessionRecord message operations:
// recovery after an interrupt, expiring/cancelling pending interaction
// messages when a session is stopped, failing running commands,
// index/insert/push helpers, and the active-turn file-change grace
// window bookkeeping that keeps late watcher events attached to the
// just-finished turn (see tests/file_changes.rs for the invariants).
//
// The second half covers rehydration of a Codex thread's history into
// TermAl's `Message` model: `codex_thread_messages_from_json` walks the
// per-turn JSON payload Codex emits, projecting each item (user_message,
// agent_message, reasoning, command, file_change, etc.) into the
// matching `Message` variant so a forked or imported Codex session
// renders in the transcript with the right shape. The projection also
// carries Codex-specific fallback markdown when an item has no direct
// Message counterpart.
//
// Extracted from state.rs so state.rs can stay focused on `StateInner`
// + commit_locked() + SSE broadcasting.

/// Returns the transcript count carried by session-scoped SSE deltas.
fn session_message_count(record: &SessionRecord) -> u32 {
    debug_assert!(
        record.session.messages.len() <= u32::MAX as usize,
        "session transcript length exceeded the wire messageCount range"
    );
    let local_count = u32::try_from(record.session.messages.len()).unwrap_or(u32::MAX);
    if record.session.messages_loaded {
        local_count
    } else {
        record.session.message_count
    }
}

/// Recovers interrupted session record.
fn recover_interrupted_session_record(record: &mut SessionRecord) -> Option<String> {
    if !matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval
    ) {
        return None;
    }

    let interrupted_interaction_count =
        expire_pending_interaction_messages(&mut record.session.messages);
    fail_running_command_messages(&mut record.session.messages);

    let mut notice = if interrupted_interaction_count > 0
        || record.session.status == SessionStatus::Approval
    {
        "TermAl restarted while this session was waiting for approval or input. That request expired. Send another prompt to continue.".to_owned()
    } else {
        "TermAl restarted before this turn finished. The last response may be incomplete. Send another prompt to continue.".to_owned()
    };

    let queued_count = record.queued_prompts.len();
    if queued_count > 0 {
        let noun = if queued_count == 1 {
            "prompt remains"
        } else {
            "prompts remain"
        };
        notice.push_str(&format!(" {queued_count} queued {noun} saved."));
    }

    Some(notice)
}

/// Expires pending interaction messages.
fn expire_pending_interaction_messages(messages: &mut [Message]) -> usize {
    let mut count = 0;
    for message in messages {
        match message {
            Message::Approval { decision, .. } => {
                if *decision == ApprovalDecision::Pending {
                    *decision = ApprovalDecision::Interrupted;
                    count += 1;
                }
            }
            Message::UserInputRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            Message::McpElicitationRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            Message::CodexAppRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            _ => {}
        }
    }
    count
}

/// Cancels pending interaction messages.
fn cancel_pending_interaction_messages(messages: &mut [Message]) {
    for message in messages {
        match message {
            Message::Approval { decision, .. } => {
                if *decision == ApprovalDecision::Pending {
                    *decision = ApprovalDecision::Rejected;
                }
            }
            Message::UserInputRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            Message::McpElicitationRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            Message::CodexAppRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            _ => {}
        }
    }
}

/// Marks running command messages as failed.
fn fail_running_command_messages(messages: &mut [Message]) {
    for message in messages {
        if let Message::Command { status, .. } = message {
            if *status == CommandStatus::Running {
                *status = CommandStatus::Error;
            }
        }
    }
}

/// Builds message positions.
fn build_message_positions(messages: &[Message]) -> HashMap<String, usize> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| (message.id().to_owned(), index))
        .collect()
}

fn message_index_on_record(record: &mut SessionRecord, message_id: &str) -> Option<usize> {
    if let Some(index) = record.message_positions.get(message_id).copied() {
        if record
            .session
            .messages
            .get(index)
            .is_some_and(|message| message.id() == message_id)
        {
            return Some(index);
        }
    }

    record.message_positions = build_message_positions(&record.session.messages);
    record.message_positions.get(message_id).copied()
}

fn insert_message_on_record(record: &mut SessionRecord, index: usize, message: Message) -> usize {
    let index = index.min(record.session.messages.len());
    record.session.messages.insert(index, message);
    record.message_positions = build_message_positions(&record.session.messages);
    index
}

/// Pushes message on record.
fn push_message_on_record(record: &mut SessionRecord, message: Message) -> usize {
    insert_message_on_record(record, record.session.messages.len(), message)
}

/// Keeps a short post-turn window for watcher events that arrive after completion.
fn finish_active_turn_file_change_tracking(record: &mut SessionRecord) {
    if record.active_turn_start_message_count.take().is_some() {
        record.active_turn_file_change_grace_deadline =
            Some(std::time::Instant::now() + ACTIVE_TURN_FILE_CHANGE_GRACE);
        return;
    }

    record.active_turn_file_changes.clear();
    record.active_turn_file_change_grace_deadline = None;
}

/// Clears active turn file-change tracking.
fn clear_active_turn_file_change_tracking(record: &mut SessionRecord) {
    record.active_turn_start_message_count = None;
    record.active_turn_file_changes.clear();
    record.active_turn_file_change_grace_deadline = None;
}

/// Pushes the active turn file-change summary on record.
fn push_active_turn_file_changes_on_record(
    record: &mut SessionRecord,
    message_id: String,
) -> bool {
    if record.active_turn_file_changes.is_empty() {
        return false;
    }

    let files = std::mem::take(&mut record.active_turn_file_changes)
        .into_iter()
        .map(|(path, kind)| FileChangeSummaryEntry { path, kind })
        .collect::<Vec<_>>();
    let count = files.len();
    let title = if count == 1 {
        "Agent changed 1 file".to_owned()
    } else {
        format!("Agent changed {count} files")
    };
    push_message_on_record(
        record,
        Message::FileChanges {
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            title,
            files,
        },
    );
    true
}

/// Pushes session markdown note on record.
fn push_session_markdown_note_on_record(
    record: &mut SessionRecord,
    message_id: String,
    title: &str,
    markdown: String,
) {
    let message = Message::Markdown {
        id: message_id,
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: title.to_owned(),
        markdown,
    };
    if let Some(preview) = message.preview_text() {
        record.session.preview = preview;
    }
    push_message_on_record(record, message);
}

/// Replaces session messages on record.
fn replace_session_messages_on_record(
    record: &mut SessionRecord,
    messages: Vec<Message>,
    fallback_preview: Option<String>,
) {
    record.session.messages = messages;
    record.message_positions = build_message_positions(&record.session.messages);
    record.session.preview = record
        .session
        .messages
        .iter()
        .rev()
        .find_map(Message::preview_text)
        .or(fallback_preview)
        .unwrap_or_else(|| "Ready for a prompt.".to_owned());
}

/// Handles Codex thread messages from JSON.
fn codex_thread_messages_from_json(inner: &mut StateInner, thread: &Value) -> Option<Vec<Message>> {
    let turns = thread.get("turns").and_then(Value::as_array)?;
    let mut messages = Vec::new();
    for turn in turns {
        append_codex_thread_turn_messages(inner, turn, &mut messages)?;
    }
    (!messages.is_empty()).then_some(messages)
}

/// Appends Codex thread turn messages.
fn append_codex_thread_turn_messages(
    inner: &mut StateInner,
    turn: &Value,
    messages: &mut Vec<Message>,
) -> Option<()> {
    let items = turn.get("items").and_then(Value::as_array)?;
    for item in items {
        append_codex_thread_item_messages(inner, item, messages);
    }
    if let Some(text) = codex_thread_turn_status_text(turn) {
        messages.push(Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text,
            expanded_text: None,
        });
    }
    Some(())
}

/// Appends Codex thread item messages.
fn append_codex_thread_item_messages(
    inner: &mut StateInner,
    item: &Value,
    messages: &mut Vec<Message>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("userMessage") => {
            if let Some(text) = codex_thread_user_message_text(item) {
                messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: inner.next_message_id(),
                    timestamp: stamp_now(),
                    author: Author::You,
                    text,
                    expanded_text: None,
                });
            }
        }
        Some("agentMessage") => {
            let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Text {
                attachments: Vec::new(),
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: text.to_owned(),
                expanded_text: None,
            });
        }
        Some("reasoning") => {
            let lines = codex_thread_reasoning_lines(item);
            if lines.is_empty() {
                return;
            }
            messages.push(Message::Thinking {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex reasoning".to_owned(),
                lines,
            });
        }
        Some("plan") => {
            let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Markdown {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex plan".to_owned(),
                markdown: text.to_owned(),
            });
        }
        Some("commandExecution") => {
            let Some(command) = item
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Command {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: command.to_owned(),
                command_language: Some(shell_language().to_owned()),
                output: item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                output_language: infer_command_output_language(command).map(str::to_owned),
                status: codex_thread_command_status(item),
            });
        }
        Some("fileChange") => {
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return;
            };
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return;
            }
            for change in changes {
                let Some(file_path) = change
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
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
                let message_id = inner.next_message_id();
                messages.push(Message::Diff {
                    id: message_id.clone(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    change_set_id: Some(diff_change_set_id(&message_id)),
                    file_path: file_path.to_owned(),
                    summary,
                    diff: diff.to_owned(),
                    language: Some("diff".to_owned()),
                    change_type,
                });
            }
        }
        Some(item_type) => {
            let Some(markdown) = codex_thread_fallback_markdown(item, item_type) else {
                return;
            };
            messages.push(Message::Markdown {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: codex_thread_fallback_title(item_type),
                markdown,
            });
        }
        None => {}
    }
}

/// Handles Codex thread user message text.
fn codex_thread_user_message_text(item: &Value) -> Option<String> {
    let content = item.get("content").and_then(Value::as_array)?;
    let parts: Vec<String> = content
        .iter()
        .filter_map(codex_thread_user_input_text)
        .collect();
    let joined = parts.join("\n\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Handles Codex thread user input text.
fn codex_thread_user_input_text(input: &Value) -> Option<String> {
    match input.get("type").and_then(Value::as_str) {
        Some("text") => input
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        Some("image") => input
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("Image: {value}")),
        Some("localImage") => input
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("Local image: {value}")),
        Some("skill") => codex_thread_named_path_text(input, "Skill"),
        Some("mention") => codex_thread_named_path_text(input, "Mention"),
        _ => None,
    }
}

/// Handles Codex thread named path text.
fn codex_thread_named_path_text(input: &Value, label: &str) -> Option<String> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (name, path) {
        (Some(name), Some(path)) => Some(format!("{label}: {name} ({path})")),
        (Some(name), None) => Some(format!("{label}: {name}")),
        (None, Some(path)) => Some(format!("{label}: {path}")),
        (None, None) => None,
    }
}

/// Handles Codex thread reasoning lines.
fn codex_thread_reasoning_lines(item: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for key in ["summary", "content"] {
        let Some(values) = item.get(key).and_then(Value::as_array) else {
            continue;
        };
        for value in values {
            let Some(text) = value.as_str() else {
                continue;
            };
            for line in text.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_owned());
                }
            }
        }
    }
    lines
}

/// Handles Codex thread command status.
fn codex_thread_command_status(item: &Value) -> CommandStatus {
    match item.get("status").and_then(Value::as_str) {
        Some("completed") => match item.get("exitCode").and_then(Value::as_i64) {
            Some(0) | None => CommandStatus::Success,
            Some(_) => CommandStatus::Error,
        },
        Some("failed") | Some("declined") => CommandStatus::Error,
        _ => CommandStatus::Running,
    }
}

/// Handles Codex thread turn status text.
fn codex_thread_turn_status_text(turn: &Value) -> Option<String> {
    match turn.get("status").and_then(Value::as_str) {
        Some("failed") => {
            let detail = turn
                .get("error")
                .filter(|value| !value.is_null())
                .map(summarize_error)
                .unwrap_or_else(|| "Codex reported a turn failure.".to_owned());
            Some(format!("Turn failed: {detail}"))
        }
        Some("interrupted") => Some("Turn interrupted.".to_owned()),
        _ => None,
    }
}

/// Handles Codex thread fallback title.
fn codex_thread_fallback_title(item_type: &str) -> String {
    match item_type {
        "mcpToolCall" => "Codex MCP tool call".to_owned(),
        "dynamicToolCall" => "Codex dynamic tool call".to_owned(),
        _ => format!("Codex {item_type}"),
    }
}

/// Handles Codex thread fallback markdown.
fn codex_thread_fallback_markdown(item: &Value, item_type: &str) -> Option<String> {
    let mut sections = vec![format!("Codex returned a `{item_type}` thread item.")];
    if let Some(tool) = item
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Tool: `{tool}`"));
    }
    if let Some(server) = item
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Server: `{server}`"));
    }
    if let Some(status) = item
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Status: `{status}`"));
    }
    if let Some(prompt) = item
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(prompt.to_owned());
    }
    if let Some(error) = item.get("error").filter(|value| !value.is_null()) {
        sections.push(format!("Error: {}", summarize_error(error)));
    }
    Some(sections.join("\n\n"))
}
