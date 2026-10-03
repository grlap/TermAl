// The SessionStart compact hook of one Claude runtime.
//
// Owns: deciding at initialize whether this runtime registers the hook (a
// session Engram-enabled at that moment; the registration is a snapshot and is
// not hot-added later), the hook entry written into the initialize request,
// and the answer to Claude Code's `hook_callback` control request: a callback
// for this runtime's registered id, SessionStart and source compact is
// answered once, from a worker, with the session's Engram work context as
// `additionalContext`; any other well-formed callback gets an empty answer; a
// callback missing its callback id or input gets an error answer. Each
// registered request has one lifecycle, from its context read through the
// queue to the writer: a cancellation revokes it at any point before the
// write, the writer takes it exactly once, and a settled request id is not
// reopened by a duplicate. Its answer is correlated with the ticket taken as
// the callback arrives (the compaction's generation and the settings
// identity) and bounded by a deadline counted from that arrival to the write.
// The writer acknowledges the delivery only after the answer was written in
// time (`claude_spawn.rs`).
//
// Does not own: preparing the context page or its generation
// (`engram_mcp_config.rs`), the routing and replay barrier of the callback
// frame (`claude_frame_router.rs`), the compaction's refresh request
// (`claude_frame_application.rs`), or writing the answer (`claude_spawn.rs`).
//
// New file.
//
// Claude Code 2.1.288 sends the callback while it compacts, before the
// compact_boundary frame, and its compaction waits for the answer; the
// registered timeout is how long it waits at most before it cancels. The host
// answers within a budget well under that timeout: a slow context read ends in
// an empty answer, and the next prompt carries the context instead.

/// How long the host takes at most to answer a compact callback: the context
/// command's own timeout and four seconds more, counted from the callback's
/// arrival to the end of the answer's write. A slow read, or an answer that
/// waits too long for the writer, ends in the empty answer, and its page waits
/// for the next prompt.
const CLAUDE_COMPACT_HOOK_ANSWER_BUDGET: Duration =
    Duration::from_secs(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT.as_secs() + 4);

/// The hook timeout registered with Claude Code: the answer budget with room
/// to spare, so the host's own empty answer always comes before a cancel.
const CLAUDE_COMPACT_HOOK_REGISTERED_TIMEOUT_SECS: u64 =
    CLAUDE_COMPACT_HOOK_ANSWER_BUDGET.as_secs() + 20;

/// Settled request ids kept so a duplicate cannot reopen one.
const CLAUDE_COMPACT_HOOK_SETTLED_LIMIT: usize = 64;

/// The SessionStart compact hook a runtime registered at initialize.
struct ClaudeCompactHook {
    callback_id: String,
    requests: Mutex<ClaudeHookRequests>,
    /// How long a callback's context read may take before the empty answer:
    /// `CLAUDE_COMPACT_HOOK_ANSWER_BUDGET`, which a test may shorten.
    answer_budget: Duration,
}

/// Where each registered callback request is in its single lifecycle.
#[derive(Default)]
struct ClaudeHookRequests {
    states: HashMap<String, ClaudeHookRequestState>,
    /// Settled ids, oldest first, at most `CLAUDE_COMPACT_HOOK_SETTLED_LIMIT`.
    settled: VecDeque<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeHookRequestState {
    /// Its context is being read.
    Reading,
    /// Its answer waits in the writer's queue; `expired` once its deadline
    /// passed there.
    Queued { expired: bool },
    /// The writer is writing its answer.
    Writing,
    /// Written, cancelled, expired while written, or dropped for a replaced
    /// runtime: done.
    Settled,
}

impl ClaudeHookRequests {
    fn settle(&mut self, request_id: &str) {
        self.states
            .insert(request_id.to_owned(), ClaudeHookRequestState::Settled);
        self.settled.push_back(request_id.to_owned());
        while self.settled.len() > CLAUDE_COMPACT_HOOK_SETTLED_LIMIT {
            if let Some(oldest) = self.settled.pop_front() {
                self.states.remove(&oldest);
            }
        }
    }
}

/// What the host answers one hook callback with.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeHookAnswer {
    /// The session's Engram work context for the compaction, and the ticket
    /// (generation and settings identity) whose delivery the written answer
    /// acknowledges.
    Context {
        text: String,
        ticket: EngramCompactHookTicket,
    },
    /// No context: an empty hook output.
    Empty,
    /// A malformed callback, refused.
    Error(String),
}

/// One hook callback's answer, for the runtime's writer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeHookResponse {
    request_id: String,
    answer: ClaudeHookAnswer,
    /// The answer of a request of this runtime's compact hook: the writer
    /// writes it only if it can still take that request.
    registered: bool,
    /// When the registered request's answer budget ends, counted from the
    /// callback's arrival: context still queued then is written as the empty
    /// answer, and context whose write ends after it is not delivered.
    deadline: Option<std::time::Instant>,
}

/// What the writer settles once its write of a hook answer is over.
struct ClaudeHookWrite {
    request_id: String,
    registered: bool,
    deadline: Option<std::time::Instant>,
    /// The context's ticket, when the answer written carries context.
    ticket: Option<EngramCompactHookTicket>,
}

impl ClaudeHookResponse {
    fn write_record(&self) -> ClaudeHookWrite {
        ClaudeHookWrite {
            request_id: self.request_id.clone(),
            registered: self.registered,
            deadline: self.deadline,
            ticket: match &self.answer {
                ClaudeHookAnswer::Context { ticket, .. } => Some(ticket.clone()),
                _ => None,
            },
        }
    }
}

impl ClaudeCompactHook {
    fn new(runtime_id: &str) -> Self {
        Self {
            callback_id: format!("termal-session-start-compact-{runtime_id}"),
            requests: Mutex::new(ClaudeHookRequests::default()),
            answer_budget: CLAUDE_COMPACT_HOOK_ANSWER_BUDGET,
        }
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, ClaudeHookRequests> {
        self.requests
            .lock()
            .expect("Claude compact hook mutex poisoned")
    }

    /// Opens `request_id`'s lifecycle; `false` for an id already known,
    /// pending or settled.
    fn begin(&self, request_id: &str) -> bool {
        let mut requests = self.requests();
        if requests.states.contains_key(request_id) {
            return false;
        }
        requests
            .states
            .insert(request_id.to_owned(), ClaudeHookRequestState::Reading);
        true
    }

    /// The read is over and its answer goes to the writer; `false` when the
    /// request was cancelled meanwhile.
    fn queue(&self, request_id: &str) -> bool {
        let mut requests = self.requests();
        match requests.states.get_mut(request_id) {
            Some(state @ ClaudeHookRequestState::Reading) => {
                *state = ClaudeHookRequestState::Queued { expired: false };
                true
            }
            _ => false,
        }
    }

    /// The writer takes the answer to write: `Some(in_time)`, `false` when
    /// its deadline passed while it was queued; `None` when the request was
    /// cancelled after it was queued, or was already taken.
    fn take_for_write(&self, request_id: &str) -> Option<bool> {
        let mut requests = self.requests();
        let state = requests.states.get_mut(request_id)?;
        let ClaudeHookRequestState::Queued { expired } = *state else {
            return None;
        };
        *state = ClaudeHookRequestState::Writing;
        Some(!expired)
    }

    /// The writer's write is over; `true` when the request was neither
    /// cancelled nor expired while it was written. Its lifecycle ends.
    fn finish_write(&self, request_id: &str) -> bool {
        let mut requests = self.requests();
        if requests.states.get(request_id) != Some(&ClaudeHookRequestState::Writing) {
            return false;
        }
        requests.settle(request_id);
        true
    }

    /// The request's answer budget ended. Queued, its context will be
    /// written as the empty answer; being written, its write delivers
    /// nothing. A request still read is the worker's to end.
    fn expire(&self, request_id: &str) {
        let mut requests = self.requests();
        match requests.states.get_mut(request_id) {
            Some(ClaudeHookRequestState::Queued { expired }) => *expired = true,
            Some(ClaudeHookRequestState::Writing) => requests.settle(request_id),
            _ => {}
        }
    }

    /// Ends the request without a delivered answer: Claude Code cancelled
    /// it, or its runtime is no longer the session's. A write under way
    /// still completes, but delivers nothing.
    fn settle_unanswered(&self, request_id: &str) {
        let mut requests = self.requests();
        if requests
            .states
            .get(request_id)
            .is_some_and(|state| *state != ClaudeHookRequestState::Settled)
        {
            requests.settle(request_id);
        }
    }
}

/// The hook a runtime registers: one when its session is Engram-enabled at
/// spawn, as the Engram MCP configuration the caller resolved under its state
/// lock says. It takes no lock itself.
fn claude_compact_hook_for_runtime(
    engram_mcp: Option<&TermalDelegationMcpStdioConfig>,
    runtime_id: &str,
) -> Option<ClaudeCompactHook> {
    engram_mcp.map(|_| ClaudeCompactHook::new(runtime_id))
}

/// The `hooks` object of the initialize request.
fn claude_initialize_hooks(hook: Option<&ClaudeCompactHook>) -> Value {
    match hook {
        Some(hook) => json!({
            "SessionStart": [{
                "matcher": "compact",
                "hookCallbackIds": [hook.callback_id],
                "timeout": CLAUDE_COMPACT_HOOK_REGISTERED_TIMEOUT_SECS,
            }]
        }),
        None => json!({}),
    }
}

/// The control response for `response`, as the writer writes it.
fn claude_hook_response_message(response: &ClaudeHookResponse) -> Value {
    match &response.answer {
        ClaudeHookAnswer::Context { text, .. } => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": response.request_id,
                "response": {
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": engram_context_fence(text),
                    }
                }
            }
        }),
        ClaudeHookAnswer::Empty => json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": response.request_id,
                "response": {}
            }
        }),
        ClaudeHookAnswer::Error(error) => json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": response.request_id,
                "error": error,
            }
        }),
    }
}

/// Whether `message` is a `hook_callback` control request.
fn claude_message_is_hook_callback(message: &Value) -> bool {
    message.get("type").and_then(Value::as_str) == Some("control_request")
        && message.pointer("/request/subtype").and_then(Value::as_str) == Some("hook_callback")
}

/// Sends a hook answer to the runtime's writer; `false` when the writer is
/// gone, which leaves Claude Code waiting until its own timeout and is logged.
fn send_claude_hook_response(
    input_tx: &Sender<ClaudeRuntimeCommand>,
    session_id: &str,
    response: ClaudeHookResponse,
) -> bool {
    let sent = input_tx
        .send(ClaudeRuntimeCommand::HookResponse(response))
        .is_ok();
    if !sent {
        eprintln!(
            "runtime state warning> Claude session `{session_id}` hook answer could not reach \
             its writer"
        );
    }
    sent
}

/// Answers a `hook_callback` control request of this runtime. The context
/// read runs on a worker; this returns at once, so the reader keeps reading
/// (permission requests included) while the answer is prepared. A
/// registered compact callback always records its compaction's request for a
/// fresh context, whatever an earlier compaction left; the caller records that
/// this compaction asked, so its boundary does not ask again.
fn answer_claude_hook_callback(
    context: &ClaudeRuntimeContext,
    input_tx: &Sender<ClaudeRuntimeCommand>,
    message: &Value,
) -> ClaudeHookCallbackApplied {
    let Some(request_id) = message.get("request_id").and_then(Value::as_str) else {
        eprintln!(
            "runtime state warning> Claude session `{}` sent a hook callback with no request \
             id; it cannot be answered",
            context.session_id
        );
        return ClaudeHookCallbackApplied::NoCompaction;
    };
    let respond = |answer| {
        send_claude_hook_response(
            input_tx,
            &context.session_id,
            ClaudeHookResponse {
                request_id: request_id.to_owned(),
                answer,
                registered: false,
                deadline: None,
            },
        );
    };
    let request = message.get("request");
    let callback_id = request
        .and_then(|request| request.get("callback_id"))
        .and_then(Value::as_str);
    let input = request.and_then(|request| request.get("input"));
    let (Some(callback_id), Some(input)) = (callback_id, input.filter(|input| input.is_object()))
    else {
        respond(ClaudeHookAnswer::Error(
            "TermAl could not read this hook callback: it names no callback id or input".to_owned(),
        ));
        return ClaudeHookCallbackApplied::NoCompaction;
    };
    let compact = context.compact_hook.as_ref().filter(|hook| {
        hook.callback_id == callback_id
            && input.get("hook_event_name").and_then(Value::as_str) == Some("SessionStart")
            && input.get("source").and_then(Value::as_str) == Some("compact")
    });
    let Some(hook) = compact.cloned() else {
        respond(ClaudeHookAnswer::Empty);
        return ClaudeHookCallbackApplied::NoCompaction;
    };
    if !hook.begin(request_id) {
        // A duplicate of a request already seen: its one answer stands, and
        // it asks nothing more of this compaction.
        return ClaudeHookCallbackApplied::NoCompaction;
    }
    // The answer budget runs from the callback's arrival, to the write.
    let deadline = std::time::Instant::now() + hook.answer_budget;
    // What the answer is correlated with, taken as the callback arrives,
    // with the compaction's refresh request recorded first.
    let ticket = context
        .state
        .begin_engram_compact_hook_read(&context.session_id);
    let state = context.state.clone();
    let session_id = context.session_id.clone();
    let token = context.token.clone();
    let input_tx = input_tx.clone();
    let request_id = request_id.to_owned();
    std::thread::spawn(move || {
        let answer = match ticket {
            Some(ticket) => read_claude_compact_hook_answer(
                state.clone(),
                &session_id,
                &token,
                ticket,
                deadline,
            ),
            None => ClaudeHookAnswer::Empty,
        };
        if !state.claude_runtime_is_current(&session_id, &token) {
            hook.settle_unanswered(&request_id);
            return;
        }
        if !hook.queue(&request_id) {
            // Cancelled by Claude Code meanwhile: the request was abandoned.
            return;
        }
        let sent = send_claude_hook_response(
            &input_tx,
            &session_id,
            ClaudeHookResponse {
                request_id: request_id.clone(),
                answer,
                registered: true,
                deadline: Some(deadline),
            },
        );
        if !sent {
            // No writer will take it: the lifecycle still ends.
            hook.settle_unanswered(&request_id);
            return;
        }
        // The budget's end, wherever the answer is then: still queued, its
        // context goes out empty; still being written, it delivers nothing.
        drop(state);
        std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
        hook.expire(&request_id);
    });
    ClaudeHookCallbackApplied::Compaction
}

/// Reads the compaction's context for `ticket` on its own thread and waits
/// for it until `deadline`; the empty answer when it fails or is late. A late
/// read still finishes, and its page waits for the next prompt.
fn read_claude_compact_hook_answer(
    state: AppState,
    session_id: &str,
    token: &RuntimeToken,
    ticket: EngramCompactHookTicket,
    deadline: std::time::Instant,
) -> ClaudeHookAnswer {
    let (prepared_tx, prepared_rx) = mpsc::channel();
    {
        let session_id = session_id.to_owned();
        let token = token.clone();
        let ticket = ticket.clone();
        std::thread::spawn(move || {
            let _ = prepared_tx.send(state.prepare_engram_compact_hook_context_off_lock(
                &session_id,
                &token,
                &ticket,
            ));
        });
    }
    match prepared_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
        Ok(Some(text)) => ClaudeHookAnswer::Context { text, ticket },
        Ok(None) => ClaudeHookAnswer::Empty,
        Err(_) => {
            eprintln!(
                "engram> session={session_id} compact hook context was not ready in time; the \
                 next prompt carries it"
            );
            ClaudeHookAnswer::Empty
        }
    }
}

/// What answering a hook callback meant for the compaction under way.
#[derive(Debug, PartialEq, Eq)]
enum ClaudeHookCallbackApplied {
    /// The callback of this runtime's compact hook: the compaction asked for
    /// a fresh context.
    Compaction,
    /// Any other callback, or a duplicate.
    NoCompaction,
}

/// Claude Code abandoned a hook callback request: no answer is written for
/// it, even one already queued, and its context counts as not delivered.
fn cancel_claude_hook_callback(context: &ClaudeRuntimeContext, request_id: &str) {
    if let Some(hook) = context.compact_hook.as_ref() {
        hook.settle_unanswered(request_id);
    }
}

/// The writer's step for a hook answer: whether to write it, and what. A
/// registered request is written only if the writer can still take it; its
/// context is written only before the request's deadline and while that page
/// is still the session's current one for this runtime and settings, and an
/// empty answer replaces it otherwise.
fn claude_hook_response_to_write(
    context: &ClaudeRuntimeContext,
    mut response: ClaudeHookResponse,
) -> Option<ClaudeHookResponse> {
    let mut in_time = true;
    if response.registered {
        in_time = context
            .compact_hook
            .as_ref()
            .and_then(|hook| hook.take_for_write(&response.request_id))?;
    }
    if let ClaudeHookAnswer::Context { ticket, .. } = &response.answer {
        let in_time = in_time
            && response
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() < deadline);
        if !in_time
            || !context.state.engram_compact_hook_context_is_current(
                &context.session_id,
                &context.token,
                ticket,
            )
        {
            response.answer = ClaudeHookAnswer::Empty;
        }
    }
    Some(response)
}

/// The writer's step once its write of a hook answer is over, `written` or
/// failed. A registered request's lifecycle ends here. Its context counts as
/// delivered only when it was written, its write ended before the deadline,
/// and Claude Code did not cancel it nor did it expire while it was written;
/// otherwise the page stays for the next prompt.
fn finish_claude_hook_write(context: &ClaudeRuntimeContext, write: ClaudeHookWrite, written: bool) {
    let unrevoked = !write.registered
        || context
            .compact_hook
            .as_ref()
            .is_some_and(|hook| hook.finish_write(&write.request_id));
    let in_time = write
        .deadline
        .is_some_and(|deadline| std::time::Instant::now() < deadline);
    if let Some(ticket) = write.ticket.filter(|_| written && unrevoked && in_time) {
        context.state.acknowledge_engram_compact_hook_delivery(
            &context.session_id,
            &context.token,
            &ticket,
        );
    }
}
