// Turn recorder ecosystem — the vocabulary every agent-runtime handler
// (Claude NDJSON, Codex app-server JSON-RPC, ACP session updates) uses to
// deposit events as `Message` entries in a session transcript.
//
// A "turn" is one user prompt → N agent events → completion. While a turn
// is running each runtime parser calls into a recorder (`push_text`,
// `text_delta`, `push_thinking`, `push_diff`, `command_started`,
// `push_approval`, etc.) to persist the event. Recorders encapsulate the
// dual path of (a) mutating in-memory state via `AppState` and (b)
// broadcasting SSE deltas to the web UI — or, for the REPL, writing a
// human-readable line to stdout. This is what the user sees scrolling in
// each session pane.
//
// The `TurnRecorder` trait covers the shared surface. `CodexTurnRecorder`
// extends it with MCP elicitation, user-input, and generic app-request
// methods because only the Codex app server emits those protocol
// messages. The `SessionRecorderAccess` helper trait lets the 20+
// `recorder_*` free functions be generic over a single-session accessor
// so `SessionRecorder` and `BorrowedSessionRecorder` share one
// implementation body instead of duplicating logic.
//
// Three concrete implementations:
// - `SessionRecorder`: owns an `AppState` + session id; used in server
//   mode (ACP reader thread, Claude turn runner, tests).
// - `BorrowedSessionRecorder`: borrows an already-locked `StateInner`
//   + recorder state; used from the shared-Codex app-server dispatcher
//   where the state mutex is already held.
// - `ReplPrinter`: prints directly to stdout; used by `termal repl codex`
//   / `termal repl claude` / etc.
//
// Key invariant: recorders are stateful across events within a turn —
// they track the open streaming-text message id (so `text_delta` appends
// to it), the `command_messages` / `parallel_agents_messages` upsert keys
// (so later updates mutate the same message), and Claude/Codex
// streaming-assistant text reconciliation. `reset_turn_state` clears
// that state between turns. Streaming-text reconciliation
// (append-suffix / skip-duplicate / replace-divergent via `text_delta`
// + `replace_streaming_text` + `finish_streaming_text`) is the main
// source of subtle UI bugs, so keep those semantics stable.
//
// Extracted from turns.rs into its own `include!()` fragment alongside
// the rest of the backend so turns.rs can stay focused on the
// `run_turn_blocking` / `run_codex_turn` / `run_acp_turn` dispatch and
// shared helpers.

/// Shared recorder surface that every agent runtime parser drives.
/// Each method persists one event into the session transcript.
trait TurnRecorder {
    /// Records the agent-native session/thread id discovered mid-turn so it can
    /// be associated with this termal session (for later reconnect / resume).
    fn note_external_session(&mut self, session_id: &str) -> Result<()>;
    /// Records a pending tool/command approval request that the user must
    /// accept or reject before the agent proceeds.
    fn push_approval(&mut self, title: &str, command: &str, detail: &str) -> Result<()>;
    /// Appends a completed, non-streaming assistant text block (e.g. a whole
    /// message body the agent delivered in one chunk).
    fn push_text(&mut self, text: &str) -> Result<()>;
    /// Records the summary emitted when a nested subagent / delegated task
    /// completes, including optional external conversation/turn ids.
    fn push_subagent_result(
        &mut self,
        title: &str,
        summary: &str,
        conversation_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<()>;
    /// Records a reasoning / thinking block (Claude thinking, Codex reasoning
    /// summaries).
    fn push_thinking(&mut self, title: &str, lines: Vec<String>) -> Result<()>;
    /// Records a unified-diff file change block (edit or create).
    fn push_diff(
        &mut self,
        file_path: &str,
        summary: &str,
        diff: &str,
        change_type: ChangeType,
    ) -> Result<()>;
    /// Appends a streaming-assistant-text delta. First call opens a streaming
    /// text message; subsequent calls append to the same message id until
    /// `finish_streaming_text` closes it.
    fn text_delta(&mut self, delta: &str) -> Result<()>;
    /// Rewrites the currently open streaming-text message wholesale. Used when
    /// the agent emits a "completed assistant message" that diverges from what
    /// was streamed so far (see Claude's append-vs-replace reconciliation).
    fn replace_streaming_text(&mut self, text: &str) -> Result<()>;
    /// Closes the currently open streaming-text message. Called before any
    /// non-text event so new events appear after the finalized text block.
    fn finish_streaming_text(&mut self) -> Result<()>;
    /// Resets per-turn recorder state (streaming id, command/parallel-agent
    /// upsert keys). Default impl just closes the streaming text — the
    /// session-backed impls override to clear the upsert maps too.
    fn reset_turn_state(&mut self) -> Result<()> {
        self.finish_streaming_text()
    }
    /// Upserts a command-execution message in the `Running` state, keyed by
    /// `key` so a later `command_completed` with the same key mutates the
    /// same message.
    fn command_started(&mut self, key: &str, command: &str) -> Result<()>;
    /// Upserts the same command message with its captured output + final
    /// status (success / error / cancelled).
    fn command_completed(
        &mut self,
        key: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()>;
    /// Upserts a parallel-agent progress message keyed by `key` so follow-up
    /// progress events (agent completions, status changes) mutate the same
    /// group message instead of appending duplicates.
    fn upsert_parallel_agents(&mut self, key: &str, agents: &[ParallelAgentProgress])
    -> Result<()>;
    /// Records an error line in the transcript (prefixed with `Error:` for
    /// the session-backed impls).
    fn error(&mut self, detail: &str) -> Result<()>;
}

/// Codex-only recorder surface. Extends `TurnRecorder` with the three
/// interaction-request kinds the Codex app server can emit (approvals with
/// structured pending payload, user-input questions, MCP elicitation
/// requests, generic app requests) — these all register a pending interaction
/// in state that the UI resolves by submitting an answer/decision.
trait CodexTurnRecorder: TurnRecorder {
    /// Records a Codex tool-call approval request and registers the pending
    /// approval payload so the resolver can route the user's decision back to
    /// the app server.
    fn push_codex_approval(
        &mut self,
        title: &str,
        command: &str,
        detail: &str,
        approval: CodexPendingApproval,
    ) -> Result<()>;

    /// Records a Codex user-input request (structured questions the agent
    /// wants answered) and registers the pending request.
    fn push_codex_user_input_request(
        &mut self,
        title: &str,
        detail: &str,
        questions: Vec<UserInputQuestion>,
        request: CodexPendingUserInput,
    ) -> Result<()>;

    /// Records an MCP elicitation request proxied through the Codex app
    /// server and registers the pending elicitation.
    fn push_codex_mcp_elicitation_request(
        &mut self,
        title: &str,
        detail: &str,
        request: McpElicitationRequestPayload,
        pending: CodexPendingMcpElicitation,
    ) -> Result<()>;

    /// Records a generic Codex app-server JSON-RPC request (catch-all for
    /// method+params pairs that don't map to a dedicated kind) and registers
    /// the pending app request.
    fn push_codex_app_request(
        &mut self,
        title: &str,
        detail: &str,
        method: &str,
        params: Value,
        pending: CodexPendingAppRequest,
    ) -> Result<()>;
}

/// Owns-its-state recorder used in server mode. Holds a cloned `AppState`
/// handle (so it can be freely moved to a reader thread) and an independent
/// `SessionRecorderState`. Used by the ACP reader thread, the Claude turn
/// runner, and the shared Codex runtime's per-turn setup path.
struct SessionRecorder {
    /// Streaming-text id + command/parallel-agent upsert keys for this turn.
    recorder_state: SessionRecorderState,
    /// Local termal session id this recorder writes into.
    session_id: String,
    /// App state handle — cloned into each `recorder_*` call so the state
    /// mutex can be acquired fresh per event.
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

    /// Claude-specific approval entry point. Not on `TurnRecorder` because
    /// the Claude pending-approval payload type is distinct from Codex/ACP.
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

    /// ACP-specific approval entry point. Mirrors `push_claude_approval` but
    /// registers the pending approval against the ACP runtime handle so the
    /// user's decision gets forwarded to the ACP agent's permission request.
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

/// Minimal accessor trait that lets the `recorder_*` free functions work
/// against any single-session recorder without caring whether the state
/// handle is owned (`SessionRecorder`) or borrowed from an already-locked
/// guard (`BorrowedSessionRecorder`). Each impl body is reused by both.
trait SessionRecorderAccess {
    /// Returns the `AppState` handle used to push messages + broadcast SSE.
    fn state(&self) -> &AppState;
    /// Returns the local termal session id this recorder targets.
    fn session_id(&self) -> &str;
    /// Returns a mutable reference to the per-turn recorder state
    /// (streaming id, command upsert map, parallel-agents upsert map).
    fn recorder_state_mut(&mut self) -> &mut SessionRecorderState;
}

// Wires `SessionRecorder` (owned `AppState` + owned state) into the generic
// free-function implementation body.
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

// Pushes an `Approval` message in the `Pending` decision state and invokes
// `register` with the allocated message id so the caller can associate the
// pending approval payload (Claude / ACP / Codex) with the message. Used by
// all three per-agent approval entry points.
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

// Pushes a `UserInputRequest` message (structured questions) and registers
// the pending request so an answer submission can route back to the Codex
// app server. Codex-only.
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

// Pushes an `McpElicitationRequest` message (MCP-server-initiated user
// prompt proxied through Codex) and registers the pending elicitation.
// Codex-only.
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

// Pushes a `CodexAppRequest` message (catch-all for arbitrary Codex
// app-server JSON-RPC requests not covered by the dedicated kinds) and
// registers the pending request keyed by method + params. Codex-only.
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

// Records the agent-native session id on the session record so reconnects
// can resume the right underlying agent thread. Does not emit a transcript
// message.
fn recorder_note_external_session<R: SessionRecorderAccess>(
    recorder: &mut R,
    session_id: &str,
) -> Result<()> {
    let state = recorder.state().clone();
    let current_session_id = recorder.session_id().to_owned();
    state.set_external_session_id(&current_session_id, session_id.to_owned())
}

// Pushes a bare `Approval` message (no pending-approval registration).
// Used for approval displays that don't need resolver routing — e.g. the
// REPL stubs or historical approvals. For live agent approvals, the
// per-agent helpers (`push_claude_approval`, `push_codex_approval`,
// `push_acp_approval`) call `recorder_push_pending_approval` instead.
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

// Appends a completed, non-streaming `Text` message. Finishes any open
// streaming-text message first so this block appears after it. Empty/
// whitespace-only input is a no-op. Use this for whole-message bodies the
// agent delivers in one shot — for incremental streaming use
// `recorder_text_delta` instead.
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

// Appends a `SubagentResult` message that summarizes the output of a
// nested/delegated subagent turn. Carries optional external conversation +
// turn ids so the UI can link back to the nested run.
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

// Appends a streaming-assistant-text delta. If no streaming-text message
// is open yet, opens one (allocates a new message id and pushes an empty
// `Text` message) and remembers its id in `recorder_state.streaming_text_message_id`.
// Subsequent deltas within the same turn append to that same message via
// `AppState::append_text_delta` (which also broadcasts a `TextDelta` SSE
// event). Empty input is a no-op — callers don't need to pre-filter.
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

// Rewrites the currently open streaming-text message with `text`. Used
// when a runtime parser detects that the agent's completed message text
// diverged from what was streamed (e.g. the stream showed partial output
// that was later rewritten). If no streaming message is open, falls back
// to pushing a fresh `Text` message. Differs from `recorder_push_text`:
// `push_text` always appends a new message and finishes the streaming one;
// `replace_streaming_text` mutates the in-place streaming message id.
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

// Appends a `Thinking` reasoning block with a title + bullet lines.
// Empty-lines input is a no-op. Closes any open streaming text first.
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

// Appends a `Diff` message carrying a unified-diff blob plus metadata
// (file path, summary, change type). Allocates a deterministic change-set
// id so the UI can link related diff messages. Empty-diff input is a no-op.
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

// Closes the currently open streaming-text message by clearing the
// remembered id — the message itself stays in the transcript as-is; the
// next event simply starts a new message. Cheap: just a local state reset,
// no state-lock acquisition.
fn recorder_finish_streaming_text<R: SessionRecorderAccess>(recorder: &mut R) -> Result<()> {
    recorder.recorder_state_mut().streaming_text_message_id = None;
    Ok(())
}

// Clears all per-turn recorder state (streaming id + command upsert map +
// parallel-agent upsert map) so the next turn starts fresh and a stray
// upsert key from the previous turn can't collide with a new message.
fn recorder_reset_turn_state<R: SessionRecorderAccess>(recorder: &mut R) -> Result<()> {
    reset_recorder_state_fields(recorder.recorder_state_mut());
    Ok(())
}

// Upserts a `Command` message in `Running` status. The first call for a
// given `key` allocates a new message id and stores it in
// `recorder_state.command_messages`; later calls for the same key (from
// `recorder_command_completed`) reuse it so output + final status mutate
// the same message rather than appending a duplicate.
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

// Upserts the same command message keyed by `key` with its captured
// output + terminal status. If `command_started` was never called (e.g. a
// tool emits only a single completion event), this allocates the message
// id lazily so the command still shows up in the transcript.
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

// Upserts a `ParallelAgents` message keyed by `key` (the parallel-agent
// group identifier). Repeated calls during the group's lifetime mutate
// the same message so per-agent status / completion transitions replace
// rather than append.
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

// Appends an error as a `Text` message prefixed with `Error: `. Empty
// detail is a no-op. Finishes any open streaming text first so the error
// visually follows the text it interrupted.
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

// Wires `SessionRecorder` to `CodexTurnRecorder` by delegating to the
// shared `recorder_*` free functions (Codex-specific approvals register
// against `register_codex_pending_approval`).
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

/// Borrowed-state recorder used when the caller already holds a state
/// mutex guard and a borrowed `SessionRecorderState` (typically the
/// shared-Codex session state stored in the app-server dispatcher). Reuses
/// those borrows to avoid re-locking per event. Same recorder surface as
/// `SessionRecorder`.
struct BorrowedSessionRecorder<'a> {
    /// Recorder state borrowed from the shared-Codex session map.
    recorder_state: &'a mut SessionRecorderState,
    /// Local termal session id this recorder targets.
    session_id: &'a str,
    /// App state handle borrowed from the calling context.
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

// Wires `BorrowedSessionRecorder` (borrowed `AppState` + borrowed recorder
// state) into the same generic free-function body used by `SessionRecorder`.
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

// Wires `BorrowedSessionRecorder` to `CodexTurnRecorder` — same delegation
// pattern as `SessionRecorder`, just over the borrowed state.
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

// Wires `SessionRecorder` to the shared `TurnRecorder` surface by
// delegating every method to the corresponding `recorder_*` free function.
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

    /// Upserts parallel agents.
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

// Wires `BorrowedSessionRecorder` to the shared `TurnRecorder` surface —
// same delegation pattern as the `SessionRecorder` impl above.
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

    /// Upserts parallel agents.
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

/// Stdout-only recorder used in REPL mode (`termal repl codex` /
/// `termal repl claude` / etc.). Implements the same recorder surface but
/// writes human-readable lines to stdout instead of mutating `AppState`.
/// No SSE broadcast, no session transcript — the REPL stream is the
/// transcript.
#[derive(Default)]
struct ReplPrinter {
    /// True while an `assistant> ` line is mid-emission; cleared by
    /// `finish_streaming_text` (which prints the trailing newline).
    assistant_stream_open: bool,
}

// Wires `ReplPrinter` to the shared `TurnRecorder` surface with
// stdout-formatted output instead of state mutations.
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

// Wires `ReplPrinter` to `CodexTurnRecorder` with stdout-formatted
// output for the Codex-specific event kinds. The pending payloads are
// ignored (REPL has no resolver path for them).
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
