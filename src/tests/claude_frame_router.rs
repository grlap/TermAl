// Schedules of Claude stdout frames through the production frame
// application (`claude_frame_application.rs`), the one function the stdout
// reader calls, on a real session: which turn and attempt each frame belongs
// to, what is retried (through the real delayed dispatch and the writer's own
// command function), how control requests are answered by their origin, and
// what a result finalizes. Frame shapes follow the live Claude Code 2.1.285
// captures replayed in `claude_turn_ownership.rs`; frames not captured live
// (a lifecycle 529 result, subagent control frames) are synthetic and say so.

use super::*;

/// One Claude runtime's stdout reader and stdin writer on a real session.
struct Reader {
    state: AppState,
    session_id: String,
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    /// What the runtime's stdin would receive (permission responses, retries).
    commands: mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl Reader {
    /// A Claude session running turn `turn_generation` on a fresh runtime.
    fn at_turn(turn_generation: u64) -> Self {
        let state = test_app_state();
        let session_id = test_session_id(&state, Agent::Claude);
        Self::on_session(state, session_id, "router-runtime", turn_generation)
    }

    fn new() -> Self {
        Self::at_turn(1)
    }

    /// A reader for a new runtime `runtime_id` installed on `session_id`.
    fn on_session(
        state: AppState,
        session_id: String,
        runtime_id: &str,
        turn_generation: u64,
    ) -> Self {
        let (runtime, commands) = test_claude_runtime_handle(runtime_id);
        let token = RuntimeToken::Claude(runtime.runtime_id.clone());
        let input_tx = runtime.input_tx.clone();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("the session exists");
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Claude(runtime);
            record.session.status = SessionStatus::Active;
            record.active_turn_generation = turn_generation;
            // Permission requests queue a card, so their origin is visible.
            record.session.claude_approval_mode = Some(ClaudeApprovalMode::Ask);
        }
        let context = ClaudeRuntimeContext::new(
            state.clone(),
            session_id.clone(),
            token,
            new_claude_turn_ownership(),
            Arc::new(Mutex::new(None)),
            "/tmp".to_owned(),
        );
        let frames = ClaudeReaderFrames::new(&context, input_tx, None);
        let recorder = SessionRecorder::new(state.clone(), session_id.clone());
        Self {
            state,
            session_id,
            context,
            frames,
            recorder,
            commands,
        }
    }

    /// Writes `command` as the writer thread does; returns what was written.
    fn write_command(&mut self, command: ClaudeRuntimeCommand) -> Vec<Value> {
        let mut wire = Vec::new();
        assert!(
            apply_claude_writer_command(&self.context, &mut wire, command),
            "the runtime stays writable"
        );
        String::from_utf8(wire)
            .expect("UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("NDJSON"))
            .collect()
    }

    /// Writes `prompt`; returns the uuid it was written with.
    fn write(&mut self, prompt: &ClaudePromptCommand) -> String {
        let written = self.write_command(ClaudeRuntimeCommand::Prompt(prompt.clone()));
        written[0]["uuid"].as_str().expect("uuid").to_owned()
    }

    /// Waits for the delayed dispatch of the retry `ticket`, then writes it
    /// as the writer thread does; returns the new attempt's uuid.
    fn next_retry(&mut self, ticket: &ClaudeRetryTicket) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match self.commands.recv_timeout(remaining) {
                Ok(ClaudeRuntimeCommand::RetryLastPrompt {
                    ticket: sent,
                    retry_detail,
                }) => {
                    assert_eq!(&sent, ticket, "the dispatched retry is the one scheduled");
                    let written = self.write_command(ClaudeRuntimeCommand::RetryLastPrompt {
                        ticket: sent,
                        retry_detail,
                    });
                    assert_eq!(written.len(), 1, "the retry is written once");
                    return written[0]["uuid"].as_str().expect("uuid").to_owned();
                }
                Ok(_) => continue,
                Err(err) => panic!("the scheduled retry was never dispatched: {err}"),
            }
        }
    }

    /// One frame, through the reader's own application function.
    fn feed(&mut self, frame: Value) -> ClaudeFramePlan {
        let outcome =
            apply_claude_frame(&self.context, &mut self.frames, &mut self.recorder, &frame);
        assert_eq!(outcome.next, ClaudeFrameApplied::Continue, "{frame}");
        outcome.plan
    }

    fn held(&self) -> Option<String> {
        claude_replay_generation(&self.context.replay_prompt)
    }

    fn record<T>(&self, read: impl FnOnce(&SessionRecord) -> T) -> T {
        let inner = self.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&self.session_id)
            .expect("the session exists");
        read(&inner.sessions[index])
    }

    /// The permission responses the runtime would receive, so far.
    fn permission_responses(&self) -> Vec<ClaudePermissionDecision> {
        self.commands
            .try_iter()
            .filter_map(|command| match command {
                ClaudeRuntimeCommand::PermissionResponse(decision) => Some(decision),
                _ => None,
            })
            .collect()
    }
}

/// The session's assistant text messages, in order.
fn assistant_texts(record: &SessionRecord) -> Vec<String> {
    record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                text,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn prompt(text: &str, turn_generation: u64, uuid: &str) -> ClaudePromptCommand {
    ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: uuid.to_owned(),
        text: text.to_owned(),
        turn_generation,
    }
}

fn capable_init() -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "router",
        "capabilities": ["interrupt_receipt_v1", "msg_lifecycle_v1"]})
}

fn lifecycle(uuid: &str, state: &str) -> Value {
    json!({"type": "command_lifecycle", "command_uuid": uuid, "state": state})
}

fn assistant_text(text: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}})
}

fn assistant_tool_use(id: &str, name: &str) -> Value {
    json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": id, "name": name,
        "input": {"command": "echo hi", "description": "A subagent", "prompt": "work"},
    }]}})
}

fn tool_result(id: &str) -> Value {
    json!({"type": "user", "message": {"role": "user", "content": [{
        "tool_use_id": id, "type": "tool_result", "content": "done",
    }]}})
}

fn nested(mut frame: Value, parent: &str) -> Value {
    frame["parent_tool_use_id"] = json!(parent);
    frame
}

fn result_naming(uuids: &[&str]) -> Value {
    let mut frame = json!({"type": "result", "subtype": "success", "is_error": false});
    if let Some(first) = uuids.first() {
        frame["user_message_uuid"] = json!(first);
        frame["user_message_uuids"] = json!(uuids);
    }
    frame
}

fn overloaded_naming(uuids: &[&str]) -> Value {
    let mut frame = result_naming(uuids);
    frame["is_error"] = json!(true);
    frame["api_error_status"] = json!(529);
    frame["result"] = json!("API Error: 529 Overloaded.");
    frame
}

fn task_notice_echo() -> Value {
    json!({"type": "user", "message": {"role": "user", "content":
        "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>"},
        "isReplay": true})
}

const P: &str = "11111111-0000-4000-8000-000000000001";
const Q: &str = "22222222-0000-4000-8000-000000000002";
const R: &str = "33333333-0000-4000-8000-000000000003";

#[test]
fn a_transient_error_naming_a_waiting_attempt_is_retried_without_started_and_boundedly() {
    // Synthetic: a lifecycle runtime's 529 result naming its attempt.
    let mut reader = Reader::at_turn(7);
    let host = prompt("retry me", 7, P);
    let mut uuid = reader.write(&host);
    assert_eq!(uuid, P);

    // queued -> init -> the attempt's transient error, with no `started`.
    let mut retries = Vec::new();
    for attempt in 1..CLAUDE_TRANSIENT_API_RETRY_ATTEMPTS {
        assert_eq!(
            reader.feed(lifecycle(&uuid, "queued")).parser,
            ClaudeParserStep::Skip
        );
        reader.feed(capable_init());
        let plan = reader.feed(overloaded_naming(&[&uuid]));
        assert_eq!(plan.parser, ClaudeParserStep::DiscardForRetry);
        assert_eq!(plan.terminal, None, "a retried attempt finalizes nothing");
        assert_eq!(plan.replay, ClaudeReplayStep::Retain);
        let retry = plan.retry.expect("the attempt is retried");
        assert_eq!(
            retry.ticket,
            ClaudeRetryTicket {
                replay_generation: P.to_owned(),
                turn_generation: 7,
                attempt,
            }
        );
        assert_eq!(retry.completed_attempts, attempt);
        assert_eq!(reader.held().as_deref(), Some(P));
        // The failed attempt's late `completed` moves nothing.
        assert_eq!(
            reader.feed(lifecycle(&uuid, "completed")).ownership,
            ClaudeFrameOwnership::Unchanged
        );
        uuid = reader.next_retry(&retry.ticket);
        retries.push(uuid.clone());
    }
    let mut distinct = retries.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        retries.len(),
        "each attempt has its own uuid"
    );
    assert!(!retries.iter().any(|retry| retry == P));

    // The last attempt allowed ends the turn with its error.
    reader.feed(lifecycle(&uuid, "queued"));
    let plan = reader.feed(overloaded_naming(&[&uuid]));
    assert!(plan.retry.is_none(), "retries are bounded");
    match plan.terminal {
        Some(ClaudeResultOwner::Host(owner)) => {
            assert_eq!(owner.replay_generation, P);
            assert_eq!(owner.attempt, CLAUDE_TRANSIENT_API_RETRY_ATTEMPTS - 1);
            assert_eq!(owner.attempt_uuid, uuid);
            assert_eq!(owner.turn_generation, 7);
        }
        other => panic!("the last attempt should end the prompt's turn: {other:?}"),
    }
    assert_eq!(plan.replay, ClaudeReplayStep::Retire(P.to_owned()));
    assert_eq!(reader.held(), None);
}

#[test]
fn an_effect_or_a_control_request_after_started_bars_the_transient_error_from_replay() {
    for effect in [
        assistant_text("partial output"),
        json!({"type": "control_request", "request_id": "perm-1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}}}),
    ] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("effect first", 3, P));
        reader.feed(lifecycle(P, "queued"));
        assert_eq!(
            reader.feed(lifecycle(P, "started")).parser,
            ClaudeParserStep::Open {
                carry_barrier: false
            }
        );
        reader.feed(effect);
        let plan = reader.feed(overloaded_naming(&[P]));
        assert!(
            plan.retry.is_none(),
            "an attempt with an effect is not replayed"
        );
        assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Host(_))));
        assert_eq!(plan.replay, ClaudeReplayStep::Retire(P.to_owned()));
        assert_eq!(reader.held(), None);
    }
}

#[test]
fn a_prompt_hook_before_its_attempt_opens_bars_it_from_replay() {
    let hook = json!({"type": "system", "subtype": "hook_response",
        "hook_event": "UserPromptSubmit"});
    // With `started`, and resolved by its result alone.
    for started in [true, false] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("hooked", 4, P));
        reader.feed(lifecycle(P, "queued"));
        reader.feed(hook.clone());
        if started {
            assert_eq!(
                reader.feed(lifecycle(P, "started")).parser,
                ClaudeParserStep::Open {
                    carry_barrier: true
                }
            );
        }
        let plan = reader.feed(overloaded_naming(&[P]));
        assert!(plan.retry.is_none(), "started: {started}");
        assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Host(_))));
    }
}

#[test]
fn a_subagents_frames_and_result_leave_the_root_tool_retry_count_and_terminal_alone() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("delegate", 5, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(assistant_tool_use("toolu-task", "Task"));
    assert!(reader.frames.root.pending_tools.contains_key("toolu-task"));

    let nested_started = nested(lifecycle(Q, "started"), "toolu-task");
    let plan = reader.feed(nested_started);
    assert_eq!(plan.scope, ClaudeFrameScope::Nested);
    assert_eq!(plan.parser, ClaudeParserStep::Skip);
    assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged);
    assert_eq!(
        reader
            .feed(nested(assistant_text("subagent output"), "toolu-task"))
            .parser,
        ClaudeParserStep::Nested
    );
    // A subagent's transient error result, naming the root attempt even.
    let plan = reader.feed(nested(overloaded_naming(&[P]), "toolu-task"));
    assert_eq!(plan.parser, ClaudeParserStep::Skip);
    assert!(plan.retry.is_none() && plan.terminal.is_none());
    assert!(
        reader.frames.root.pending_tools.contains_key("toolu-task"),
        "the root parser keeps its pending tool"
    );
    assert!(lock_claude_turn_ownership(&reader.context.ownership).turn_is_open());
    assert_eq!(reader.held().as_deref(), Some(P));

    reader.feed(tool_result("toolu-task"));
    let plan = reader.feed(result_naming(&[P]));
    match plan.terminal {
        Some(ClaudeResultOwner::Host(owner)) => assert_eq!(owner.attempt_uuid, P),
        other => panic!("the root result should end the root turn: {other:?}"),
    }
    assert_eq!(reader.held(), None);
}

#[test]
fn a_native_command_answered_by_its_result_alone_is_settled_by_its_identity() {
    for started in [false, true] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("/cost", 6, P));
        reader.feed(lifecycle(P, "queued"));
        if started {
            reader.feed(lifecycle(P, "started"));
        }
        let plan = reader.feed(result_naming(&[P]));
        assert_eq!(
            plan.parser,
            ClaudeParserStep::Root {
                open: !started,
                carry_barrier: false
            },
            "started: {started}"
        );
        match plan.terminal {
            Some(ClaudeResultOwner::Host(owner)) => assert_eq!(owner.turn_generation, 6),
            other => panic!("the command's result should settle it: {other:?}"),
        }
        assert_eq!(plan.replay, ClaudeReplayStep::Retire(P.to_owned()));
        reader.feed(lifecycle(P, "completed"));
        assert!(!lock_claude_turn_ownership(&reader.context.ownership).turn_is_open());
    }
}

#[test]
fn a_runtime_started_segment_never_retries_releases_or_finalizes_the_waiting_prompt() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.feed(json!({"type": "system", "subtype": "task_notification", "task_id": "b1"}));
    let plan = reader.feed(task_notice_echo());
    assert_eq!(
        plan.ownership,
        ClaudeFrameOwnership::OpenedRuntime {
            cause: ClaudeUnownedCause::TaskNotice
        }
    );
    // A prompt written while the runtime's turn runs waits behind it.
    reader.write(&prompt("waiting", 8, P));
    assert_eq!(
        reader.feed(lifecycle(P, "queued")).parser,
        ClaudeParserStep::Skip
    );
    reader.feed(assistant_text("the runtime's own work"));
    let plan = reader.feed(overloaded_naming(&[]));
    assert!(
        plan.retry.is_none(),
        "the runtime's error never retries the prompt"
    );
    assert_eq!(plan.replay, ClaudeReplayStep::Retain);
    assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Runtime(_))));
    assert_eq!(reader.held().as_deref(), Some(P));

    // Then the prompt's own attempt.
    assert_eq!(
        reader.feed(lifecycle(P, "started")).parser,
        ClaudeParserStep::Open {
            carry_barrier: false
        }
    );
    reader.feed(assistant_text("the prompt's answer"));
    let plan = reader.feed(result_naming(&[P]));
    assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Host(_))));
    assert_eq!(reader.held(), None);
}

#[test]
fn mixed_unresolved_and_plural_identities_are_never_retried_and_finalize_only_what_they_name() {
    // Taken up part-way by a turn no prompt owned.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.feed(task_notice_echo());
    reader.write(&prompt("taken up", 9, P));
    assert!(matches!(
        reader.feed(lifecycle(P, "started")).ownership,
        ClaudeFrameOwnership::JoinedHost(_)
    ));
    let plan = reader.feed(overloaded_naming(&[P]));
    assert!(plan.retry.is_none(), "a mixed turn is never replayed");
    assert!(matches!(
        plan.terminal,
        Some(ClaudeResultOwner::HostAfterUnowned(_))
    ));
    assert_eq!(plan.replay, ClaudeReplayStep::Retire(P.to_owned()));

    // A result naming two prompts.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("first", 10, Q));
    reader.write(&prompt("second", 10, R));
    reader.feed(lifecycle(Q, "started"));
    let plan = reader.feed(overloaded_naming(&[Q, R]));
    assert!(plan.retry.is_none());
    assert_eq!(plan.replay, ClaudeReplayStep::Retain);
    assert!(matches!(
        plan.terminal,
        Some(ClaudeResultOwner::Unresolved { .. })
    ));

    // A result naming nobody known.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("waiting", 11, P));
    let plan = reader.feed(overloaded_naming(&[R]));
    assert!(plan.retry.is_none());
    assert!(matches!(
        plan.terminal,
        Some(ClaudeResultOwner::Unresolved {
            prompt_waiting: true,
            ..
        })
    ));
    assert_eq!(reader.held().as_deref(), Some(P));
}

#[test]
fn duplicate_terminals_and_late_frames_of_an_ended_attempt_close_nothing() {
    let mut reader = Reader::at_turn(13);
    reader.feed(capable_init());
    reader.write(&prompt("once", 12, P));
    reader.feed(lifecycle(P, "started"));
    assert!(matches!(
        reader.feed(result_naming(&[P])).terminal,
        Some(ClaudeResultOwner::Host(_))
    ));
    let plan = reader.feed(result_naming(&[P]));
    assert_eq!(plan.parser, ClaudeParserStep::Skip);
    assert_eq!(plan.terminal, None, "a duplicate terminal ends nothing");
    assert_eq!(
        reader.feed(lifecycle(P, "completed")).parser,
        ClaudeParserStep::Skip
    );

    // A retried prompt: the failed attempt's late frames never close the
    // attempt open now.
    reader.write(&prompt("retried", 13, Q));
    reader.feed(lifecycle(Q, "started"));
    let ticket = reader
        .feed(overloaded_naming(&[Q]))
        .retry
        .expect("the attempt is retried")
        .ticket;
    let retry = reader.next_retry(&ticket);
    reader.feed(lifecycle(&retry, "started"));
    reader.feed(assistant_tool_use("toolu-work", "Bash"));
    assert!(reader.frames.root.pending_tools.contains_key("toolu-work"));
    for late in [
        overloaded_naming(&[Q]),
        lifecycle(Q, "started"),
        lifecycle(Q, "completed"),
        {
            let mut output = assistant_text("late output of the failed attempt");
            output["user_message_uuid"] = json!(Q);
            output
        },
    ] {
        let plan = reader.feed(late);
        assert!(plan.terminal.is_none() && plan.retry.is_none());
        assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged);
        assert_eq!(plan.parser, ClaudeParserStep::Skip, "fed nowhere");
    }
    assert!(
        reader.frames.root.pending_tools.contains_key("toolu-work"),
        "the open attempt's parser state is untouched"
    );
    match reader.feed(result_naming(&[&retry])).terminal {
        Some(ClaudeResultOwner::Host(owner)) => {
            assert_eq!(owner.attempt, 1);
            assert_eq!(owner.attempt_uuid, retry);
        }
        other => panic!("the retried attempt's result should end it: {other:?}"),
    }
    assert_eq!(reader.held(), None);
}

#[test]
fn a_subagents_text_tools_and_results_never_touch_the_root_parser_state() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("delegate", 14, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(
        json!({"type": "stream_event", "event": {"type": "content_block_delta",
        "delta": {"text": "Root text before the task."}}}),
    );
    reader.feed(assistant_tool_use("toolu-task", "Task"));
    reader.frames.root.permission_denied_this_turn = true;
    reader
        .frames
        .root
        .approval_keys_this_turn
        .insert("root-approval".to_owned());
    let root_text = reader.frames.root.streamed_assistant_text.clone();
    let root_saw_delta = reader.frames.root.saw_text_delta;
    let root_texts = reader.record(assistant_texts);

    for frame in [
        assistant_text("subagent text"),
        json!({"type": "assistant", "message": {"content": [
            {"type": "thinking", "thinking": "subagent thinking"}]}}),
        json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"text": "subagent delta"}}}),
        json!({"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": "toolu-sub-bash", "name": "Bash",
            "input": {"command": "cargo test", "description": "Run the tests"}}]}}),
        tool_result("toolu-sub-bash"),
    ] {
        let plan = reader.feed(nested(frame, "toolu-task"));
        assert_eq!(plan.scope, ClaudeFrameScope::Nested);
        assert_eq!(plan.parser, ClaudeParserStep::Nested);
        assert_eq!(
            plan.replay,
            ClaudeReplayStep::Block,
            "a subagent's work bars replay"
        );
    }
    let plan = reader.feed(nested(result_naming(&[]), "toolu-task"));
    assert_eq!(plan.parser, ClaudeParserStep::Skip);

    let root = &reader.frames.root;
    assert!(root.pending_tools.contains_key("toolu-task"));
    assert!(
        !root.pending_tools.contains_key("toolu-sub-bash"),
        "a subagent's tool is not registered in the root parser"
    );
    assert_eq!(root.streamed_assistant_text, root_text);
    assert_eq!(root.saw_text_delta, root_saw_delta);
    assert!(root.permission_denied_this_turn);
    assert!(root.approval_keys_this_turn.contains("root-approval"));
    assert_eq!(
        reader.record(assistant_texts),
        root_texts,
        "a subagent's text is not rendered"
    );
    assert!(
        reader.record(|record| record.session.messages.iter().any(
            |message| matches!(message, Message::Command { command, .. } if command == "cargo test")
        )),
        "a subagent's command is recorded, so it is observed"
    );

    // The root result clears the subagents' state with the root's.
    reader.feed(tool_result("toolu-task"));
    assert!(matches!(
        reader.feed(result_naming(&[P])).terminal,
        Some(ClaudeResultOwner::Host(_))
    ));
    assert!(reader.frames.nested.pending_tools.is_empty());
}

#[test]
fn only_the_exact_echo_of_a_waiting_prompt_is_bookkeeping_between_turns() {
    for (echoed, barred) in [
        ("the waiting prompt", false),
        ("some other user content", true),
    ] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        let waiting = prompt("the waiting prompt", 15, P);
        reader.write(&waiting);
        reader.feed(lifecycle(P, "queued"));
        let plan = reader.feed(json!({"type": "user", "isReplay": true, "message": {
            "role": "user", "content": claude_prompt_content(&prompt(echoed, 15, Q))}}));
        assert_eq!(
            plan.ownership,
            ClaudeFrameOwnership::Unchanged,
            "an echo owns nothing on a lifecycle runtime"
        );
        assert_eq!(
            reader.feed(lifecycle(P, "started")).parser,
            ClaudeParserStep::Open {
                carry_barrier: barred
            },
            "echo of {echoed:?}"
        );
        let plan = reader.feed(overloaded_naming(&[P]));
        assert_eq!(plan.retry.is_none(), barred, "echo of {echoed:?}");
    }
}

/// A permission request for a Bash command. Synthetic: subagent control
/// frames were not captured live; `parent` puts it under that tool use.
fn permission_request(request_id: &str, parent: Option<Value>) -> Value {
    let mut frame = json!({"type": "control_request", "request_id": request_id, "request": {
        "subtype": "can_use_tool", "tool_name": "Bash",
        "input": {"command": format!("echo {request_id}")}}});
    if let Some(parent) = parent {
        frame["parent_tool_use_id"] = parent;
    }
    frame
}

fn cancel(request_id: &str, parent: Option<&str>) -> Value {
    let mut frame = json!({"type": "control_cancel_request", "request_id": request_id});
    if let Some(parent) = parent {
        frame["parent_tool_use_id"] = json!(parent);
    }
    frame
}

/// The request ids of the session's pending Claude approvals.
fn pending_requests(record: &SessionRecord) -> Vec<String> {
    let mut ids = record
        .pending_claude_approvals
        .values()
        .map(|approval| approval.request_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[test]
fn a_subagents_permission_request_is_queued_from_its_own_state_and_leaves_the_root_alone() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("delegate", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(
        json!({"type": "stream_event", "event": {"type": "content_block_delta",
        "delta": {"text": "Root text still streaming."}}}),
    );
    reader.feed(assistant_tool_use("toolu-task", "Task"));
    let root_keys = reader.frames.root.approval_keys_this_turn.clone();
    let root_unattended = reader
        .frames
        .root
        .unattended_questions_self_resolved_this_turn;

    let plan = reader.feed(permission_request("perm-nested", Some(json!("toolu-task"))));
    assert_eq!(
        plan.control,
        Some(ClaudeControlStep::Request(ClaudeControlOrigin::Nested {
            parent_tool_use_id: "toolu-task".to_owned()
        }))
    );
    assert_eq!(
        plan.replay,
        ClaudeReplayStep::Block,
        "a request bars replay"
    );
    assert_eq!(
        reader.record(pending_requests),
        vec!["perm-nested".to_owned()],
        "the subagent's request is queued through the ordinary approval flow"
    );
    assert_eq!(reader.frames.root.approval_keys_this_turn, root_keys);
    assert_eq!(
        reader
            .frames
            .root
            .unattended_questions_self_resolved_this_turn,
        root_unattended
    );
    assert!(
        !reader.frames.nested.approval_keys_this_turn.is_empty(),
        "the subagent's request is counted in the subagents' own state"
    );
    assert!(reader.frames.root.pending_tools.contains_key("toolu-task"));
}

fn root_delta(text: &str) -> Value {
    json!({"type": "stream_event", "event": {"type": "content_block_delta",
        "delta": {"text": text}}})
}

/// The session's assistant text messages, by id, in order.
fn assistant_text_messages(record: &SessionRecord) -> Vec<(String, String)> {
    record
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Text {
                author: Author::Assistant,
                id,
                text,
                ..
            } => Some((id.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

/// A subagent's presentation writes under a background task, each with what
/// it leaves in the transcript beside the root text. Synthetic: subagent
/// frames were not captured live.
fn nested_presentation_writes() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        (
            "approval card",
            vec![permission_request("perm-nested", Some(json!("toolu-task")))],
        ),
        (
            "Write diff",
            vec![
                nested(
                    json!({"type": "assistant", "message": {"content": [{
                        "type": "tool_use", "id": "toolu-sub-write", "name": "Write",
                        "input": {"file_path": "/tmp/new.txt", "content": "hello"}}]}}),
                    "toolu-task",
                ),
                nested(
                    json!({"type": "user", "message": {"role": "user", "content": [{
                        "tool_use_id": "toolu-sub-write", "type": "tool_result",
                        "content": "File created"}]},
                        "tool_use_result": {"type": "create", "filePath": "/tmp/new.txt",
                            "content": "hello"}}),
                    "toolu-task",
                ),
            ],
        ),
        (
            "error line",
            vec![
                nested(
                    json!({"type": "assistant", "message": {"content": [{
                        "type": "tool_use", "id": "toolu-sub-ask", "name": "AskUserQuestion",
                        "input": {"questions": []}}]}}),
                    "toolu-task",
                ),
                nested(
                    json!({"type": "user", "message": {"role": "user", "content": [{
                        "tool_use_id": "toolu-sub-ask", "type": "tool_result",
                        "is_error": true, "content": "the subagent's question failed"}]}}),
                    "toolu-task",
                ),
            ],
        ),
    ]
}

fn nested_write_is_recorded(record: &SessionRecord, write: &str) -> bool {
    record
        .session
        .messages
        .iter()
        .any(|message| match (write, message) {
            ("approval card", Message::Approval { .. }) => true,
            ("Write diff", Message::Diff { file_path, .. }) => file_path == "/tmp/new.txt",
            ("error line", Message::Text { text, .. }) => {
                text == "Error: the subagent's question failed"
            }
            _ => false,
        })
}

#[test]
fn a_subagents_cards_and_diffs_leave_the_root_text_one_message_through_its_completed_text() {
    // The completed payload repeats, extends, or corrects what was streamed.
    for (completed, expected) in [
        ("AlphaBeta", "AlphaBeta"),
        ("AlphaBeta!", "AlphaBeta!"),
        ("Alpha, then Beta", "Alpha, then Beta"),
    ] {
        for (write, frames) in nested_presentation_writes() {
            let case = format!("{write}, completed {completed:?}");
            let mut reader = Reader::new();
            reader.feed(capable_init());
            reader.write(&prompt("delegate in the background", 1, P));
            reader.feed(lifecycle(P, "started"));
            // The background task is established before the root message.
            reader.feed(assistant_tool_use("toolu-task", "Task"));
            reader.feed(tool_result("toolu-task"));
            let texts_before = reader.record(assistant_text_messages).len();

            reader.feed(root_delta("Alpha"));
            let root_message = reader.record(assistant_text_messages)[texts_before]
                .0
                .clone();
            let root_tools = reader
                .frames
                .root
                .pending_tools
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let root_keys = reader.frames.root.approval_keys_this_turn.clone();
            let root_denied = reader.frames.root.permission_denied_this_turn;
            for frame in frames {
                reader.feed(frame);
            }
            assert!(
                reader.record(|record| nested_write_is_recorded(record, write)),
                "{case}: the subagent's write is recorded"
            );
            reader.feed(root_delta("Beta"));
            reader.feed(assistant_text(completed));

            let texts = reader.record(assistant_text_messages);
            let root_texts = texts[texts_before..]
                .iter()
                .filter(|(_, text)| !text.starts_with("Error: "))
                .collect::<Vec<_>>();
            assert_eq!(
                root_texts,
                vec![&(root_message.clone(), expected.to_owned())],
                "{case}: the root text is one message, with no duplicated prefix"
            );
            let errors = texts
                .iter()
                .filter(|(_, text)| text == "Error: the subagent's question failed")
                .count();
            assert_eq!(errors, usize::from(write == "error line"), "{case}");
            assert_eq!(
                reader
                    .frames
                    .root
                    .pending_tools
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                root_tools,
                "{case}"
            );
            assert_eq!(
                reader.frames.root.approval_keys_this_turn, root_keys,
                "{case}"
            );
            assert_eq!(
                reader.frames.root.permission_denied_this_turn, root_denied,
                "{case}"
            );
            assert!(
                matches!(
                    reader.feed(result_naming(&[P])).terminal,
                    Some(ClaudeResultOwner::Host(_))
                ),
                "{case}: the root result still ends the prompt's turn"
            );
        }
    }
}

#[test]
fn a_root_boundary_between_root_text_still_closes_the_root_message() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("work", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(root_delta("Alpha"));
    reader.feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": "toolu-root-bash", "name": "Bash",
        "input": {"command": "ls", "description": "List"}}]}}));
    reader.feed(root_delta("Beta"));
    reader.feed(assistant_text("Beta"));
    let texts = reader
        .record(assistant_text_messages)
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>();
    assert_eq!(texts, vec!["Alpha".to_owned(), "Beta".to_owned()]);
}

#[test]
fn a_cancellation_clears_only_a_request_its_origin_may_name() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("work", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(assistant_tool_use("toolu-task", "Task"));
    reader.feed(permission_request("perm-root", None));
    reader.feed(permission_request("perm-sub", Some(json!("toolu-task"))));
    assert_eq!(
        reader.record(pending_requests),
        vec!["perm-root".to_owned(), "perm-sub".to_owned()]
    );

    // A subagent's cancellation never clears the root turn's request, nor
    // another subagent's.
    let plan = reader.feed(cancel("perm-root", Some("toolu-task")));
    assert!(matches!(
        plan.control,
        Some(ClaudeControlStep::Cancel(
            ClaudeControlOrigin::Nested { .. }
        ))
    ));
    reader.feed(cancel("perm-sub", Some("toolu-other")));
    assert_eq!(
        reader.record(pending_requests),
        vec!["perm-root".to_owned(), "perm-sub".to_owned()],
        "neither cancellation may name its request"
    );

    // A subagent cancels its own; the root may name any request of this
    // runtime by its id.
    reader.feed(cancel("perm-sub", Some("toolu-task")));
    assert_eq!(
        reader.record(pending_requests),
        vec!["perm-root".to_owned()]
    );
    reader.feed(permission_request("perm-sub-2", Some(json!("toolu-task"))));
    reader.feed(cancel("perm-sub-2", None));
    reader.feed(cancel("perm-root", None));
    assert!(reader.record(pending_requests).is_empty());
}

#[test]
fn an_unattributable_request_is_refused_rather_than_dropped_or_given_root_authority() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("once", 1, P));
    reader.feed(lifecycle(P, "started"));
    assert!(matches!(
        reader.feed(result_naming(&[P])).terminal,
        Some(ClaudeResultOwner::Host(_))
    ));
    let root_keys = reader.frames.root.approval_keys_this_turn.clone();

    // A malformed parent, and a request naming only an attempt that ended.
    let mut stale = permission_request("perm-stale", None);
    stale["user_message_uuid"] = json!(P);
    for (frame, request_id) in [
        (
            permission_request("perm-malformed", Some(json!(5))),
            "perm-malformed",
        ),
        (stale, "perm-stale"),
    ] {
        let plan = reader.feed(frame);
        assert_eq!(
            plan.control,
            Some(ClaudeControlStep::Request(ClaudeControlOrigin::Unresolved)),
            "{request_id}"
        );
        assert_eq!(
            plan.ownership,
            ClaudeFrameOwnership::Unchanged,
            "{request_id}"
        );
        let responses = reader.permission_responses();
        assert!(
            matches!(
                responses.as_slice(),
                [ClaudePermissionDecision::Deny { request_id: denied, .. }] if denied == request_id
            ),
            "the runtime is answered with one refusal ({} responses)",
            responses.len()
        );
    }
    assert!(
        reader.record(pending_requests).is_empty(),
        "nothing is queued"
    );
    assert_eq!(reader.frames.root.approval_keys_this_turn, root_keys);
    assert!(!lock_claude_turn_ownership(&reader.context.ownership).turn_is_open());
}

#[test]
fn a_cancellation_reaching_a_replaced_runtime_clears_nothing_of_the_old_one() {
    let mut old = Reader::new();
    old.feed(capable_init());
    old.write(&prompt("work", 1, P));
    old.feed(lifecycle(P, "started"));
    old.feed(permission_request("perm-old", None));
    assert_eq!(old.record(pending_requests), vec!["perm-old".to_owned()]);

    // A new runtime on the same session: it never sent that request.
    let mut new = Reader::on_session(
        old.state.clone(),
        old.session_id.clone(),
        "router-runtime-2",
        1,
    );
    new.feed(capable_init());
    new.feed(cancel("perm-old", None));
    assert_eq!(
        new.record(pending_requests),
        vec!["perm-old".to_owned()],
        "the new runtime's cancellation names no request of its own"
    );
}

#[test]
fn a_waiting_prompt_left_behind_an_unadopted_turn_on_a_legacy_runtime_gets_the_stop_notice() {
    // A runtime without msg_lifecycle_v1: a background notice starts a turn
    // while the session is busy (not adopted), and a prompt written meanwhile
    // can never be settled by an identity.
    let mut reader = Reader::new();
    reader.feed(json!({"type": "system", "subtype": "init", "session_id": "legacy"}));
    let waiting = prompt("the waiting prompt", 1, P);
    reader.feed(task_notice_echo());
    reader.write(&waiting);
    reader.feed(assistant_text("the runtime's own work"));
    let plan = reader.feed(result_naming(&[]));
    assert!(matches!(
        plan.terminal,
        Some(ClaudeResultOwner::Runtime(ClaudeRuntimeTurnOwner {
            adopted_generation: None,
            ..
        }))
    ));
    let notices = reader.record(|record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Text {
                    author: Author::System,
                    text,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert!(
        notices.last().is_some_and(|notice| notice
            .contains("cannot tell whether the waiting prompt ran")
            && notice.contains("stop the session")),
        "{notices:?}"
    );
    assert_eq!(
        reader.record(|record| record.session.status),
        SessionStatus::Active,
        "the waiting prompt is not declared done"
    );

    // On a lifecycle runtime the waiting prompt is settled by its own
    // `started`, so no such notice is given.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.feed(task_notice_echo());
    reader.write(&prompt("the waiting prompt", 1, P));
    reader.feed(assistant_text("the runtime's own work"));
    reader.feed(result_naming(&[]));
    assert!(!reader.record(|record| {
        record.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text.contains("cannot tell whether the waiting prompt ran")
    ))
    }));
}

#[test]
fn claude_live_frame_sequence_keeps_only_pre_effect_overloads_replayable() {
    // Synthetic 529 results; the other frames follow Claude Code 2.1.220
    // stream captures. Overloads are replayable only before any effect.
    let overloaded = || {
        json!({"type": "result", "subtype": "success", "is_error": true,
            "api_error_status": 529, "result": "API Error: 529 Overloaded."})
    };
    let mut reader = Reader::new();
    // Process-scoped SessionStart hooks, and an unknown event no prompt was
    // waiting for, bar no later prompt.
    reader.feed(json!({"type": "system", "subtype": "hook_started", "hook_event": "SessionStart"}));
    reader.feed(json!({"type": "system", "subtype": "future_post_turn_event"}));
    let first = prompt("Review this change.", 1, P);
    reader.write(&first);
    reader.feed(json!({"type": "system", "subtype": "status", "status": "requesting"}));
    let echo = |prompt: &ClaudePromptCommand| json!({"type": "user", "message": {"role": "user", "content": claude_prompt_content(prompt)}});
    let plan = reader.feed(echo(&first));
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Root {
            open: true,
            carry_barrier: false
        },
        "the prompt's echo opens a clean attempt"
    );
    reader.feed(json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}));
    assert!(!reader.frames.root.replay_became_unsafe);
    assert!(matches!(
        reader.feed(overloaded()).retry,
        Some(ClaudeRetryPlan { status: 529, .. })
    ));

    // Prompt hooks occur after the prompt boundary and may have side effects.
    let second = prompt("Review the next change.", 1, Q);
    reader.write(&second);
    reader.feed(echo(&second));
    reader.feed(
        json!({"type": "system", "subtype": "hook_response", "hook_event": "UserPromptSubmit"}),
    );
    let plan = reader.feed(overloaded());
    assert_eq!(plan.retry, None);
    assert_eq!(plan.replay, ClaudeReplayStep::Retire(Q.to_owned()));

    // So does partial assistant output.
    let third = prompt("Review a third change.", 1, R);
    reader.write(&third);
    reader.feed(echo(&third));
    reader.feed(
        json!({"type": "stream_event", "event": {"type": "content_block_delta",
        "delta": {"text": "partial assistant output"}}}),
    );
    assert_eq!(reader.feed(overloaded()).retry, None);
}

#[test]
fn the_application_syncs_session_metadata_and_arms_the_compaction_nudge() {
    let mut reader = Reader::new();
    let (models_tx, models_rx) = mpsc::channel();
    reader.frames.initialize_model_options_tx = Some(models_tx);
    reader.feed(json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": "initialize", "response": {
            "commands": [{"name": "review", "description": "Review the change"}],
            "models": [{"value": "opus", "displayName": "Opus"}]}}}));
    let models = models_rx
        .try_recv()
        .expect("the initialize answer is passed on")
        .expect("the model options sync");
    assert_eq!(models.len(), 1);
    assert!(
        reader.record(|record| record
            .agent_commands
            .iter()
            .any(|command| command.name == "review")),
        "the native commands reach the session"
    );
    // A new session starts with the nudge armed; consume it first.
    {
        let mut inner = reader.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&reader.session_id)
            .expect("the session exists");
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    reader.feed(json!({"type": "system", "subtype": "compact_boundary"}));
    reader.record(|record| {
        assert!(
            record.engram.context_nudge_pending,
            "a compaction asks for fresh context on the next prompt"
        )
    });
}

/// A stdin whose writes fail, as when the runtime has exited.
struct ClosedStdin;

impl std::io::Write for ClosedStdin {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "the runtime's stdin is closed",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A writer context as the writer thread builds it.
fn writer_context(
    state: &AppState,
    session_id: &str,
) -> (
    ClaudeRuntimeContext,
    ClaudeTurnOwnership,
    ClaudeReplayPrompt,
) {
    let ownership = new_claude_turn_ownership();
    let replay_prompt: ClaudeReplayPrompt = Arc::new(Mutex::new(None));
    let context = ClaudeRuntimeContext::new(
        state.clone(),
        session_id.to_owned(),
        RuntimeToken::Claude("writer-loop-runtime".to_owned()),
        ownership.clone(),
        replay_prompt.clone(),
        "/tmp".to_owned(),
    );
    (context, ownership, replay_prompt)
}

#[test]
fn the_writer_loop_drains_its_queue_and_ends_once_every_sender_is_gone() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (context, ownership, replay_prompt) = writer_context(&state, &session_id);
    let (input_tx, input_rx) = mpsc::channel();
    // A command still queued when the last sender goes is written first.
    input_tx
        .send(ClaudeRuntimeCommand::Prompt(prompt("queued", 1, P)))
        .expect("the command should queue");
    // The runtime handle and the reader drop their senders; the queue then
    // holds only what was already sent.
    drop(input_tx);
    let (done_tx, done_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut wire = Vec::new();
        run_claude_writer(&context, &mut wire, input_rx);
        drop(context);
        let _ = done_tx.send(wire);
    });
    let wire = done_rx
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .expect("the writer loop ends once every sender is gone");
    writer.join().expect("the writer thread should not panic");
    let lines = String::from_utf8(wire)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("NDJSON"))
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "the queued prompt was written before the loop ended"
    );
    assert_eq!(lines[0]["uuid"], P);
    assert_eq!(
        Arc::strong_count(&ownership),
        1,
        "the ended writer released the runtime's turn ownership"
    );
    assert_eq!(
        Arc::strong_count(&replay_prompt),
        1,
        "the ended writer released the replay prompt"
    );
}

#[test]
fn the_writer_loop_ends_on_a_write_error_while_senders_remain() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (context, _ownership, _replay_prompt) = writer_context(&state, &session_id);
    let (input_tx, input_rx) = mpsc::channel();
    input_tx
        .send(ClaudeRuntimeCommand::Prompt(prompt("unwritable", 1, P)))
        .expect("the command should queue");
    let (done_tx, done_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        run_claude_writer(&context, &mut ClosedStdin, input_rx);
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
        .expect("a write error ends the writer loop although a sender remains");
    writer.join().expect("the writer thread should not panic");
    drop(input_tx);
}

/// A reader whose session is idle, so a turn Claude Code starts by itself is
/// adopted: Active under a new generation.
fn idle_reader() -> Reader {
    let reader = Reader::new();
    {
        let mut inner = reader.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&reader.session_id)
            .expect("the session exists");
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    reader
}

/// Opens an adopted turn of Claude Code's own; returns its generation.
fn adopt(reader: &mut Reader) -> u64 {
    reader.feed(capable_init());
    reader.feed(task_notice_echo());
    reader.record(|record| {
        assert_eq!(record.session.status, SessionStatus::Active, "adopted");
        assert!(unmediated_claude_turn_is_current(record));
        record.active_turn_generation
    })
}

fn queue_prompt(reader: &Reader, text: &str) {
    let queued = reader
        .state
        .dispatch_turn(
            &reader.session_id,
            SendMessageRequest {
                text: text.to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the prompt should be accepted");
    assert!(
        matches!(queued, DispatchTurnResult::Queued),
        "a prompt sent during the adopted turn queues behind it"
    );
}

/// The next prompt the runtime receives, within the test guard.
fn next_prompt(reader: &Reader) -> String {
    let deadline = std::time::Instant::now() + TEST_PHASE_DEADLOCK_GUARD;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match reader.commands.recv_timeout(remaining) {
            Ok(ClaudeRuntimeCommand::Prompt(prompt)) => return prompt.text,
            Ok(_) => continue,
            Err(err) => panic!("no prompt was dispatched: {err}"),
        }
    }
}

#[test]
fn an_adopted_interval_ended_by_an_unresolved_result_releases_the_queue_behind_it() {
    // An adopted turn whose result names an identity TermAl does not know.
    let mut reader = idle_reader();
    let generation = adopt(&mut reader);
    queue_prompt(&reader, "queued behind the runtime's turn");
    reader.feed(assistant_text("the runtime's own work"));
    let plan = reader.feed(result_naming(&[R]));
    match &plan.terminal {
        Some(ClaudeResultOwner::Unresolved {
            adopted_interval: Some(owner),
            prompt_waiting: false,
            ..
        }) => assert_eq!(owner.adopted_generation, Some(generation)),
        other => panic!("the result should end the adopted interval: {other:?}"),
    }
    assert_eq!(
        next_prompt(&reader),
        "queued behind the runtime's turn",
        "the queued prompt is dispatched once the adopted interval closes"
    );
    let notices = reader.record(|record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Text {
                    author: Author::System,
                    text,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("no prompt and no grant was credited")),
        "{notices:?}"
    );

    // An adopted turn made unresolved by an unknown lifecycle identity
    // before its result.
    let mut reader = idle_reader();
    let generation = adopt(&mut reader);
    assert_eq!(
        reader.feed(lifecycle(R, "started")).ownership,
        ClaudeFrameOwnership::BecameUnresolved
    );
    reader.feed(result_naming(&[]));
    reader.record(|record| {
        assert_eq!(
            record.session.status,
            SessionStatus::Idle,
            "the interval closed"
        );
        assert_eq!(record.active_turn_generation, generation);
    });
}

#[test]
fn an_unresolved_result_never_ends_an_adopted_interval_a_prompt_or_successor_is_part_of() {
    // A prompt's attempt waits: the adopted interval is not ended.
    let mut reader = idle_reader();
    adopt(&mut reader);
    reader.write(&prompt("waiting", 1, P));
    let plan = reader.feed(result_naming(&[R]));
    assert!(matches!(
        plan.terminal,
        Some(ClaudeResultOwner::Unresolved {
            adopted_interval: None,
            prompt_waiting: true,
            ..
        })
    ));
    reader.record(|record| assert_eq!(record.session.status, SessionStatus::Active));
    assert!(lock_claude_turn_ownership(&reader.context.ownership).prompt_is_waiting());

    // A subagent's result and a late duplicate of an ended attempt end
    // nothing.
    let mut reader = idle_reader();
    reader.feed(capable_init());
    reader.write(&prompt("first", 1, Q));
    reader.feed(lifecycle(Q, "started"));
    reader.feed(result_naming(&[Q]));
    reader.record(|record| assert_eq!(record.session.status, SessionStatus::Idle));
    adopt(&mut reader);
    for late in [
        nested(result_naming(&[R]), "toolu-sub"),
        result_naming(&[Q]),
    ] {
        let plan = reader.feed(late);
        assert_eq!(plan.terminal, None);
        reader.record(|record| assert_eq!(record.session.status, SessionStatus::Active));
    }

    // A successor generation began: the adopted interval's result ends
    // nothing of it.
    let mut reader = idle_reader();
    let generation = adopt(&mut reader);
    {
        let mut inner = reader.state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&reader.session_id)
            .expect("the session exists");
        inner.sessions[index].active_turn_generation = generation + 1;
    }
    reader.feed(result_naming(&[R]));
    reader.record(|record| {
        assert_eq!(
            record.session.status,
            SessionStatus::Active,
            "the successor runs on"
        );
        assert_eq!(record.active_turn_generation, generation + 1);
    });
}

const X: &str = "99999999-0000-4000-8000-00000000000a";

/// Top-level turn output that names `uuids` as its host identities.
fn output_naming(uuids: &[&str]) -> Value {
    let mut frame = assistant_text("output");
    frame["user_message_uuid"] = json!(uuids[0]);
    if uuids.len() > 1 {
        frame["user_message_uuids"] = json!(uuids);
    }
    frame
}

fn session_status(reader: &Reader) -> SessionStatus {
    reader.record(|record| record.session.status)
}

/// A new prompt `uuid` (written now unless it already waits), started and
/// ended by its own result, settles its turn on the session.
fn a_new_prompt_settles_on_its_own(reader: &mut Reader, uuid: &str, case: &str) {
    let waiting = lock_claude_turn_ownership(&reader.context.ownership)
        .outstanding
        .iter()
        .any(|owner| owner.attempt_uuid == uuid);
    if !waiting {
        reader.write(&prompt("the next prompt", 1, uuid));
    }
    assert!(
        matches!(
            reader.feed(lifecycle(uuid, "started")).ownership,
            ClaudeFrameOwnership::OpenedHost(_)
        ),
        "{case}: the new prompt's start opens its own turn"
    );
    match reader.feed(result_naming(&[uuid])).terminal {
        Some(ClaudeResultOwner::Host(owner)) => assert_eq!(owner.attempt_uuid, uuid, "{case}"),
        other => panic!("{case}: the new prompt's result should settle it: {other:?}"),
    }
    assert_eq!(session_status(reader), SessionStatus::Idle, "{case}");
}

#[test]
fn an_attempt_whose_turn_closes_unresolved_is_retired_so_its_late_frames_strand_no_prompt() {
    // Each way a prompt's turn can close unresolved: a result naming it with
    // a waiting prompt beside it, a result naming nobody on a lifecycle
    // runtime, a contradiction part-way through, and a turn no prompt owned
    // that took the prompt up and then ended naming someone else.
    for case in [
        "plural result",
        "identity-less result",
        "mid-turn contradiction",
        "taken up",
    ] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        if case == "taken up" {
            reader.feed(assistant_text("a turn no prompt owns"));
        }
        reader.write(&prompt("first", 1, P));
        reader.write(&prompt("waiting beside it", 1, Q));
        let opened = reader.feed(lifecycle(P, "started")).ownership;
        if case == "taken up" {
            assert!(
                matches!(opened, ClaudeFrameOwnership::JoinedHost(_)),
                "{case}"
            );
        } else {
            assert!(
                matches!(opened, ClaudeFrameOwnership::OpenedHost(_)),
                "{case}"
            );
        }
        let closing = match case {
            "plural result" => result_naming(&[P, Q]),
            "identity-less result" => result_naming(&[]),
            "mid-turn contradiction" => {
                assert_eq!(
                    reader.feed(output_naming(&[X])).ownership,
                    ClaudeFrameOwnership::BecameUnresolved
                );
                result_naming(&[])
            }
            _ => result_naming(&[X]),
        };
        let plan = reader.feed(closing);
        assert!(
            matches!(plan.terminal, Some(ClaudeResultOwner::Unresolved { .. })),
            "{case}: {:?}",
            plan.terminal
        );
        assert_eq!(plan.retry, None, "{case}");
        assert_eq!(
            session_status(&reader),
            SessionStatus::Active,
            "{case}: nothing settled"
        );

        // The ended attempt's late frames move nothing and open nothing.
        for late in [
            output_naming(&[P]),
            lifecycle(P, "started"),
            result_naming(&[P]),
        ] {
            let plan = reader.feed(late);
            assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged, "{case}");
            assert_eq!(plan.terminal, None, "{case}");
        }
        assert_eq!(
            lock_claude_turn_ownership(&reader.context.ownership).open,
            None,
            "{case}: no stray interval is open"
        );
        // The prompt the plural result merely named still waits, and its own
        // start and result settle it.
        a_new_prompt_settles_on_its_own(&mut reader, Q, case);
    }
}

#[test]
fn a_prompt_that_starts_inside_another_prompts_turn_takes_part_and_is_retired_with_it() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("first", 1, P));
    reader.write(&prompt("second", 1, Q));
    reader.feed(lifecycle(P, "started"));
    assert_eq!(
        reader.feed(lifecycle(Q, "started")).ownership,
        ClaudeFrameOwnership::BecameUnresolved
    );
    assert!(
        !lock_claude_turn_ownership(&reader.context.ownership).prompt_is_waiting(),
        "the prompt that started takes part; it waits no longer"
    );
    assert!(matches!(
        reader.feed(result_naming(&[Q])).terminal,
        Some(ClaudeResultOwner::Unresolved { .. })
    ));
    // Both participants are retired: their late frames are stale.
    for late in [
        lifecycle(Q, "started"),
        output_naming(&[Q]),
        result_naming(&[Q]),
        result_naming(&[P]),
    ] {
        let plan = reader.feed(late);
        assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged);
        assert_eq!(plan.terminal, None);
    }
    a_new_prompt_settles_on_its_own(&mut reader, R, "after two participants");
}

/// Frames that name no attempt TermAl can tie to anything, with no turn
/// open: each opens an unresolved prefix with no owner and no participant.
fn stray_prefix_frames() -> Vec<(&'static str, Value)> {
    vec![
        ("unknown identity", output_naming(&[X])),
        ("plural identities", output_naming(&[X, R])),
        (
            "malformed identity",
            json!({"type": "assistant", "user_message_uuid": 5,
                "message": {"content": [{"type": "text", "text": "output"}]}}),
        ),
        ("unknown started", lifecycle(X, "started")),
    ]
}

#[test]
fn a_waiting_prompts_own_start_takes_up_an_unattributed_prefix_as_mixed_and_settles_on_its_result()
{
    for (stray, frame) in stray_prefix_frames() {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("waiting", 1, Q));
        assert_eq!(
            reader.feed(frame).ownership,
            ClaudeFrameOwnership::BecameUnresolved,
            "{stray}"
        );
        let marker = reader.record(|record| record.unmediated_claude_turn.clone());
        assert!(marker.is_some(), "{stray}: the prefix is marked unassigned");

        // The prompt's own start takes the prefix up: not a clean opening.
        let plan = reader.feed(lifecycle(Q, "started"));
        assert!(
            matches!(&plan.ownership, ClaudeFrameOwnership::JoinedHost(owner) if owner.attempt_uuid == Q),
            "{stray}: {:?}",
            plan.ownership
        );
        assert_eq!(
            plan.parser,
            ClaudeParserStep::Skip,
            "{stray}: the parser is not reopened"
        );
        reader.record(|record| {
            assert!(
                unmediated_claude_turn_hides_observations(record),
                "{stray}: what follows stays unassigned until the result"
            );
            assert_eq!(
                record.unmediated_claude_turn, marker,
                "{stray}: the same marker"
            );
        });
        reader.feed(assistant_text("the prompt's work"));

        let plan = reader.feed(result_naming(&[Q]));
        assert!(
            matches!(&plan.terminal, Some(ClaudeResultOwner::HostAfterUnowned(owner)) if owner.attempt_uuid == Q),
            "{stray}: {:?}",
            plan.terminal
        );
        assert_eq!(
            plan.replay,
            ClaudeReplayStep::Retire(Q.to_owned()),
            "{stray}"
        );
        reader.record(|record| {
            assert_eq!(
                record.session.status,
                SessionStatus::Idle,
                "{stray}: the prompt settled"
            );
            assert_eq!(
                record.unmediated_claude_turn, None,
                "{stray}: the marker is closed"
            );
        });
    }
}

#[test]
fn a_waiting_prompts_own_result_takes_up_an_unattributed_prefix_as_mixed() {
    for (stray, frame) in stray_prefix_frames() {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("waiting", 1, Q));
        reader.feed(frame.clone());
        let plan = reader.feed(result_naming(&[Q]));
        assert!(
            matches!(&plan.terminal, Some(ClaudeResultOwner::HostAfterUnowned(owner)) if owner.attempt_uuid == Q),
            "{stray}: {:?}",
            plan.terminal
        );
        reader.record(|record| {
            assert_eq!(record.session.status, SessionStatus::Idle, "{stray}");
            assert_eq!(record.unmediated_claude_turn, None, "{stray}");
        });

        // A result naming a prompt nobody wrote ends the prefix and settles
        // nothing; the waiting prompt still settles on its own.
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("waiting", 1, Q));
        reader.feed(frame);
        assert!(matches!(
            reader.feed(result_naming(&[R])).terminal,
            Some(ClaudeResultOwner::Unresolved {
                prompt_waiting: true,
                ..
            })
        ));
        a_new_prompt_settles_on_its_own(&mut reader, Q, stray);
    }
}

#[test]
fn a_transient_error_after_an_unattributed_prefix_is_never_retried() {
    for started in [true, false] {
        let mut reader = Reader::new();
        reader.feed(capable_init());
        reader.write(&prompt("waiting", 1, Q));
        reader.feed(output_naming(&[X]));
        if started {
            reader.feed(lifecycle(Q, "started"));
        }
        let plan = reader.feed(overloaded_naming(&[Q]));
        assert_eq!(plan.retry, None, "started: {started}");
        assert!(
            matches!(plan.terminal, Some(ClaudeResultOwner::HostAfterUnowned(_))),
            "started: {started}: {:?}",
            plan.terminal
        );
        assert_eq!(plan.replay, ClaudeReplayStep::Retire(Q.to_owned()));
        assert_eq!(reader.held(), None);
        assert_eq!(
            session_status(&reader),
            SessionStatus::Error,
            "started: {started}"
        );
    }
}

#[test]
fn a_prompt_start_inside_an_interval_a_prompt_or_adopted_turn_took_part_in_takes_nothing_up() {
    // A prompt's turn made unresolved part-way.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("first", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(output_naming(&[X]));
    reader.write(&prompt("second", 1, Q));
    assert_eq!(
        reader.feed(lifecycle(Q, "started")).ownership,
        ClaudeFrameOwnership::Unchanged,
        "the prefix rule does not apply where a prompt took part"
    );
    assert!(matches!(
        reader.feed(result_naming(&[Q])).terminal,
        Some(ClaudeResultOwner::Unresolved { .. })
    ));
    assert_eq!(session_status(&reader), SessionStatus::Active);

    // An adopted turn of Claude Code's own.
    let mut reader = idle_reader();
    adopt(&mut reader);
    reader.write(&prompt("during the adopted turn", 2, Q));
    assert_eq!(
        reader.feed(lifecycle(Q, "started")).ownership,
        ClaudeFrameOwnership::BecameUnresolved,
        "a prompt starting inside an adopted turn takes nothing up"
    );
    assert!(matches!(
        reader.feed(result_naming(&[Q])).terminal,
        Some(ClaudeResultOwner::Unresolved {
            adopted_interval: None,
            ..
        })
    ));
    assert_eq!(session_status(&reader), SessionStatus::Active);
}

fn set_approval_mode(reader: &Reader, mode: ClaudeApprovalMode) {
    let mut inner = reader.state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&reader.session_id)
        .expect("the session exists");
    inner.sessions[index].session.claude_approval_mode = Some(mode);
}

/// A root permission request that names `uuid` as its host identity.
/// Synthetic, like the other control frames here.
fn permission_request_naming(request_id: &str, uuid: &str) -> Value {
    let mut frame = permission_request(request_id, None);
    frame["user_message_uuid"] = json!(uuid);
    frame
}

#[test]
fn a_control_request_that_opens_an_attempt_prepares_it_first_and_bars_its_replay() {
    for (mode, queued) in [
        (ClaudeApprovalMode::Ask, true),
        (ClaudeApprovalMode::AutoApprove, false),
    ] {
        for started in [false, true] {
            let case = format!("{mode:?}, started after the request: {started}");
            let mut reader = Reader::new();
            set_approval_mode(&reader, mode);
            reader.feed(capable_init());
            reader.write(&prompt("guarded", 1, P));
            // Leftover root parser state from before the attempt.
            reader
                .frames
                .root
                .approval_keys_this_turn
                .insert("stale".to_owned());

            let plan = reader.feed(permission_request_naming("perm-open", P));
            assert!(
                matches!(&plan.ownership, ClaudeFrameOwnership::OpenedHost(owner) if owner.attempt_uuid == P),
                "{case}: {:?}",
                plan.ownership
            );
            assert_eq!(
                plan.parser,
                ClaudeParserStep::Open {
                    carry_barrier: false
                },
                "{case}"
            );
            // At the handler boundary: the parser is bound to the attempt
            // and was reset once, and the request's barrier came after it.
            assert_eq!(
                reader.frames.router.parser_attempt.as_deref(),
                Some(P),
                "{case}"
            );
            assert!(reader.frames.root.replay_became_unsafe, "{case}");
            assert!(
                !reader.frames.root.approval_keys_this_turn.contains("stale"),
                "{case}: the opening reset the root parser"
            );
            if queued {
                assert_eq!(
                    reader.record(pending_requests),
                    vec!["perm-open".to_owned()],
                    "{case}"
                );
            } else {
                assert_eq!(reader.permission_responses().len(), 1, "{case}");
            }

            if started {
                let plan = reader.feed(lifecycle(P, "started"));
                assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged, "{case}");
                assert_eq!(
                    plan.parser,
                    ClaudeParserStep::Skip,
                    "{case}: a duplicate start resets nothing"
                );
                assert!(reader.frames.root.replay_became_unsafe, "{case}");
            }

            let plan = reader.feed(overloaded_naming(&[P]));
            assert_eq!(plan.retry, None, "{case}: never replayed after the request");
            assert!(
                matches!(&plan.terminal, Some(ClaudeResultOwner::Host(owner)) if owner.attempt_uuid == P),
                "{case}: {:?}",
                plan.terminal
            );
            assert_eq!(
                plan.replay,
                ClaudeReplayStep::Retire(P.to_owned()),
                "{case}"
            );
            assert_eq!(
                session_status(&reader),
                SessionStatus::Error,
                "{case}: the owned error ends P's turn"
            );
        }
    }
}

#[test]
fn only_an_attempt_its_result_alone_opened_is_replayed_without_parser_preparation() {
    // A clean result-only opening is still retried.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("clean", 1, P));
    let plan = reader.feed(overloaded_naming(&[P]));
    assert!(
        plan.retry.is_some(),
        "a clean result-only opening is retried"
    );

    // The ledger opened the attempt, but the parser was not prepared for it:
    // that disagreement never reads as a clean result-only opening.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("mismatch", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.frames.router.parser_attempt = None;
    let plan = reader.feed(overloaded_naming(&[P]));
    assert_eq!(plan.retry, None, "a preparation mismatch fails closed");
    assert!(matches!(plan.terminal, Some(ClaudeResultOwner::Host(_))));
}

#[test]
fn every_opening_disposition_is_prepared_at_the_one_application_point() {
    // A turn Claude Code started, opened by its output: a clean parser, no
    // host attempt bound, the waiting prompt's barrier kept, and its own
    // adopted provenance.
    let mut reader = idle_reader();
    reader.feed(capable_init());
    reader
        .frames
        .root
        .approval_keys_this_turn
        .insert("stale".to_owned());
    reader.frames.router.barrier_between_turns = true;
    let plan = reader.feed(assistant_text("the runtime's own work"));
    assert!(matches!(
        plan.ownership,
        ClaudeFrameOwnership::OpenedRuntime { .. }
    ));
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Root {
            open: true,
            carry_barrier: false
        }
    );
    assert!(!reader.frames.root.approval_keys_this_turn.contains("stale"));
    assert_eq!(reader.frames.router.parser_attempt, None);
    assert!(
        reader.frames.router.barrier_between_turns,
        "the barrier stays for the prompt it was seen for"
    );
    reader.record(|record| assert!(unmediated_claude_turn_is_current(record)));

    // The same turn opened by a control request: prepared the same way,
    // before the request is handled.
    let mut reader = idle_reader();
    reader.feed(capable_init());
    reader
        .frames
        .root
        .approval_keys_this_turn
        .insert("stale".to_owned());
    let plan = reader.feed(permission_request("perm-runtime", None));
    assert!(matches!(
        plan.ownership,
        ClaudeFrameOwnership::OpenedRuntime { .. }
    ));
    assert_eq!(
        plan.parser,
        ClaudeParserStep::Open {
            carry_barrier: false
        }
    );
    assert!(!reader.frames.root.approval_keys_this_turn.contains("stale"));
    assert!(reader.frames.root.replay_became_unsafe);
    assert_eq!(reader.frames.router.parser_attempt, None);

    // A mixed join keeps what ran before: no reset, its barrier kept.
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.feed(assistant_text("a turn no prompt owns"));
    reader
        .frames
        .root
        .approval_keys_this_turn
        .insert("accumulated".to_owned());
    reader.frames.root.replay_became_unsafe = true;
    reader.write(&prompt("taken up", 1, Q));
    let plan = reader.feed(lifecycle(Q, "started"));
    assert!(matches!(
        plan.ownership,
        ClaudeFrameOwnership::JoinedHost(_)
    ));
    assert_eq!(plan.parser, ClaudeParserStep::Skip);
    assert!(
        reader
            .frames
            .root
            .approval_keys_this_turn
            .contains("accumulated")
    );
    assert!(reader.frames.root.replay_became_unsafe);
    assert_eq!(reader.frames.router.parser_attempt, None);
}

#[test]
fn a_subagents_control_request_never_prepares_the_root_attempt() {
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("delegate", 1, P));
    reader.feed(lifecycle(P, "started"));
    reader.feed(assistant_tool_use("toolu-task", "Task"));
    reader
        .frames
        .root
        .approval_keys_this_turn
        .insert("root-approval".to_owned());
    let mut frame = permission_request_naming("perm-nested", P);
    frame["parent_tool_use_id"] = json!("toolu-task");
    let plan = reader.feed(frame);
    assert_eq!(plan.parser, ClaudeParserStep::Skip);
    assert_eq!(plan.ownership, ClaudeFrameOwnership::Unchanged);
    assert_eq!(reader.frames.router.parser_attempt.as_deref(), Some(P));
    assert!(reader.frames.root.pending_tools.contains_key("toolu-task"));
    assert!(
        reader
            .frames
            .root
            .approval_keys_this_turn
            .contains("root-approval"),
        "the root parser was not reset"
    );
}

#[test]
fn a_subagent_frame_is_nobodys_and_a_root_frame_is_its_open_attempts() {
    // A subagent's work is credited to no grant, even under the prompt
    // attempt that launched its task; a top-level frame of the open attempt
    // is that attempt's (`claude_outstanding_work.rs`).
    let nested_bash = |id: &str, parent: &str| {
        nested(
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use",
                "id": id, "name": "Bash", "input": {"command": "ls"}}]}}),
            parent,
        )
    };
    let mut reader = Reader::new();
    reader.feed(capable_init());
    reader.write(&prompt("launch", 1, P));
    reader.feed(lifecycle(P, "started"));
    assert_eq!(
        reader.feed(assistant_tool_use("toolu-task", "Task")).origin,
        ClaudeWorkOrigin::Attempt { turn_generation: 1 },
        "the open attempt's own top-level frame"
    );
    for (id, parent) in [("own", "toolu-task"), ("stray", "toolu-unknown")] {
        assert_eq!(
            reader.feed(nested_bash(id, parent)).origin,
            ClaudeWorkOrigin::Unattributed,
            "{id}: a subagent's frame"
        );
    }
    assert_eq!(
        reader
            .feed(permission_request("perm", Some(json!("toolu-task"))))
            .origin,
        ClaudeWorkOrigin::Unattributed,
        "a subagent's control request"
    );
    reader.record(|record| assert_eq!(record.unmediated_claude_turn, None));

    // After the attempt ends, nothing is open: a top-level frame is nobody's.
    reader.feed(result_naming(&[P]));
    assert_eq!(
        reader
            .feed(assistant_tool_use("toolu-after", "Bash"))
            .origin,
        ClaudeWorkOrigin::Unattributed
    );
}
