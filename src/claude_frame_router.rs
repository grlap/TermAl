// Claude frame router: one owner-scoped decision for each stdout frame of a
// Claude runtime, made before the parser, the replay state, retries or
// finalization act on it.
//
// Owns: the frame's scope (the runtime's own frames, the top-level
// conversation, or a subagent's); which turn and which attempt of a prompt
// it belongs to (asked of `claude_turn_ownership.rs` once per frame); and the
// plan the stdout reader applies: whether the root parser opens for a new
// host attempt, is fed, or is left alone; whether the replay prompt is kept,
// barred from replay or released; whether a transient error result is
// retried, and under which ticket; which turn a top-level `result` ended,
// resolved once before any reset; and a control frame's origin (the root
// turn, a subagent, or nobody TermAl can name). Bookkeeping frames
// (lifecycle, init, request admission, telemetry, task notices) neither
// release the replay prompt nor bar it; tool, control and unknown frames bar
// it for good.
//
// Does not own: carrying the plan out (`claude_frame_application.rs`), the
// turn ownership rules (`claude_turn_ownership.rs`), the parser
// (`claude.rs`), what an adopted or unowned turn does on its session
// (`claude_runtime_turns.rs`), or the reader loop, the writer and the delayed
// retry dispatch (`claude_spawn.rs`).
//
// New file. The per-frame decisions it makes (the parser reset on a new
// replay generation, transient retry classification, the replay clear and
// the result's finalization) were inline in the stdout reader loop of
// `claude_spawn.rs`.

/// Where a frame sits in the runtime's conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeFrameScope {
    /// The runtime's own frames: `init` and control traffic.
    Process,
    /// The top-level conversation.
    Root,
    /// A subagent's frame: it carries the tool use it belongs to.
    Nested,
}

/// What the parser does with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClaudeParserStep {
    /// Not fed: lifecycle bookkeeping, a subagent's result, or a late frame
    /// of an attempt that has ended.
    Skip,
    /// Fed to the root parser. `open` resets it first, for the host attempt
    /// that begins with this frame; `carry_barrier` then bars that attempt
    /// from replay, since a barrier frame arrived while no turn was open.
    Root { open: bool, carry_barrier: bool },
    /// Not fed, but the host attempt begins with this bookkeeping frame (its
    /// lifecycle `started`): the root parser is reset for it, barred from
    /// replay when `carry_barrier`.
    Open { carry_barrier: bool },
    /// A subagent's frame, given to the nested parser state alone
    /// (`handle_claude_nested_event`): its tool calls and their results are
    /// recorded, so their commands are observed, while the root parser's
    /// pending tools, text stream, approvals and permission state are left
    /// untouched.
    Nested,
    /// The attempt's transient error result, which is retried: the parser is
    /// reset and nothing is recorded.
    DiscardForRetry,
}

/// Where a control request or cancellation comes from, resolved once by the
/// router. The control transport is the runtime's, but each request's effects
/// belong to the context it came from (`claude_frame_application.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeControlOrigin {
    /// The top-level conversation: the turn open now, or the turn no prompt
    /// owns that a request outside every turn opens.
    Root,
    /// A subagent, by the tool use it runs under.
    Nested { parent_tool_use_id: String },
    /// Nobody TermAl can name: a malformed parent, or identities naming only
    /// attempts that have ended.
    Unresolved,
}

/// A control frame's step, with its resolved origin.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeControlStep {
    /// A permission or question request, to be answered or queued.
    Request(ClaudeControlOrigin),
    /// A cancellation of an earlier request.
    Cancel(ClaudeControlOrigin),
    /// A hook callback, answered by the runtime's hook responder
    /// (`claude_compact_hook.rs`). It belongs to the process, not a turn.
    HookCallback,
    /// A cancellation of a hook callback this router routed.
    HookCallbackCancel,
}

/// What a frame does to the replay prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeReplayStep {
    /// The replay prompt stays as it is.
    Retain,
    /// A barrier: the open host attempt may no longer be replayed.
    Block,
    /// The attempt of this replay generation ended for good: its prompt is
    /// released.
    Retire(String),
}

/// An automatic retry of the host attempt a transient error result ended.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeRetryPlan {
    ticket: ClaudeRetryTicket,
    delay: Duration,
    status: u16,
    /// The attempts made so far, the failed one included.
    completed_attempts: u32,
}

/// The decision for one frame, which the stdout reader applies.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaudeFramePlan {
    scope: ClaudeFrameScope,
    /// What the frame did to the runtime's turns, for the session effects
    /// (`AppState::apply_claude_frame_ownership`).
    ownership: ClaudeFrameOwnership,
    parser: ClaudeParserStep,
    replay: ClaudeReplayStep,
    retry: Option<ClaudeRetryPlan>,
    /// The turn a top-level `result` ended. `None` for every other frame, a
    /// retried result and a late duplicate.
    terminal: Option<ClaudeResultOwner>,
    /// A control frame's request or cancellation, with its origin.
    control: Option<ClaudeControlStep>,
    /// The turn the frame's work belongs to, which its observations carry
    /// to the Engram sink (`claude_outstanding_work.rs`): the prompt attempt
    /// whose turn is open, for a top-level frame of it; nobody's otherwise (a
    /// turn no prompt owns, an unresolved turn, no turn, or any subagent's
    /// frame, whose work is credited to no grant). It is a candidate only:
    /// the sink checks it against the session's live turn and runtime.
    origin: ClaudeWorkOrigin,
}

impl ClaudeFramePlan {
    fn new(
        scope: ClaudeFrameScope,
        ownership: ClaudeFrameOwnership,
        parser: ClaudeParserStep,
        replay: ClaudeReplayStep,
    ) -> Self {
        Self {
            scope,
            ownership,
            parser,
            replay,
            retry: None,
            terminal: None,
            control: None,
            origin: ClaudeWorkOrigin::Unattributed,
        }
    }
}

/// The turn a top-level frame's work belongs to, from what the ownership
/// ledger has open after observing it: the prompt attempt whose own turn is
/// open, or nobody.
fn claude_open_turn_origin(ownership: &ClaudeTurnOwnershipState) -> ClaudeWorkOrigin {
    ownership
        .open_host_owner()
        .map_or(ClaudeWorkOrigin::Unattributed, |owner| {
            ClaudeWorkOrigin::Attempt {
                turn_generation: owner.turn_generation,
            }
        })
}

/// The scope of `message` (`ClaudeFrameScope`).
fn claude_frame_scope(message: &Value) -> ClaudeFrameScope {
    if !claude_frame_is_top_level(message) {
        return ClaudeFrameScope::Nested;
    }
    let process = match message.get("type").and_then(Value::as_str) {
        Some("control_request" | "control_cancel_request") => true,
        Some("system") => message.get("subtype").and_then(Value::as_str) == Some("init"),
        _ => false,
    };
    if process {
        ClaudeFrameScope::Process
    } else {
        ClaudeFrameScope::Root
    }
}

/// Whether a top-level frame that arrives between turns, while no turn is
/// open and a written prompt waits, leaves the waiting prompt's next attempt
/// replayable: lifecycle and request-admission bookkeeping, process-scoped
/// hooks, telemetry, Claude Code's background-task bookkeeping, and the exact
/// echo of a waiting prompt (`echo_of_waiting`). Any other frame between
/// turns bars that attempt from replay, an echo that matches no waiting
/// prompt included. This is a between-turns rule only and claims no replay
/// safety inside an open attempt: there the root parser decides, and it
/// treats background-task frames as barriers (`handle_claude_event`), since a
/// task notification can carry context into the running prompt.
fn claude_frame_is_bookkeeping(
    message: &Value,
    echo_of_waiting: bool,
    before_process_init: bool,
) -> bool {
    match message.get("type").and_then(Value::as_str) {
        Some("command_lifecycle" | "rate_limit_event") => true,
        Some("system") => {
            claude_system_event_is_effect_free(message, before_process_init)
                || matches!(
                    message.get("subtype").and_then(Value::as_str),
                    Some(
                        "task_notification"
                            | "task_started"
                            | "task_updated"
                            | "background_tasks_changed"
                    )
                )
        }
        Some("user") => echo_of_waiting,
        _ => false,
    }
}

/// Decides, frame by frame, what each stdout frame of one Claude runtime may
/// do (`ClaudeFramePlan`). It lives on the runtime's stdout reader; the turn
/// ownership and the replay prompt are shared with the writer.
struct ClaudeFrameRouter {
    ownership: ClaudeTurnOwnership,
    replay_prompt: ClaudeReplayPrompt,
    session_id: String,
    /// The host attempt (its uuid) the root parser was opened for.
    parser_attempt: Option<String>,
    /// A barrier frame arrived at top level while no turn was open and a
    /// written prompt waited; the next host attempt inherits it.
    barrier_between_turns: bool,
    /// The prompt (its replay generation) whose automatic retries so far are
    /// counted, and their count.
    retries: Option<(String, u32)>,
    /// The runtime reported its first `init`. A process's startup hooks come
    /// before it; a SessionStart hook frame after it (a compaction's, or one
    /// TermAl cannot place) is no startup bookkeeping. A later `init`, such
    /// as the one a compaction repeats, does not reset it.
    saw_process_init: bool,
    /// Hook callback requests routed and not yet cancelled or echoed, so their
    /// cancellation and the runtime's echo of their answer route as theirs.
    hook_requests: HashSet<String>,
}

impl ClaudeFrameRouter {
    fn new(
        ownership: ClaudeTurnOwnership,
        replay_prompt: ClaudeReplayPrompt,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            ownership,
            replay_prompt,
            session_id: session_id.into(),
            parser_attempt: None,
            barrier_between_turns: false,
            retries: None,
            saw_process_init: false,
            hook_requests: HashSet::new(),
        }
    }

    /// The plan for `message`. `parser_replay_safe` says whether the root
    /// parser has seen no barrier since it opened for the current attempt.
    fn route(&mut self, message: &Value, parser_replay_safe: bool) -> ClaudeFramePlan {
        let frame_type = message.get("type").and_then(Value::as_str);
        let request_id = message.get("request_id").and_then(Value::as_str);
        match frame_type {
            Some("control_request") if claude_message_is_hook_callback(message) => {
                return self.route_hook_callback(request_id);
            }
            Some("control_cancel_request")
                if request_id.is_some_and(|id| self.hook_requests.contains(id)) =>
            {
                return self.route_hook_settled(request_id, true);
            }
            Some("control_response")
                if message
                    .pointer("/response/request_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| self.hook_requests.contains(id)) =>
            {
                // The runtime's echo of the host's own answer: it did nothing
                // new.
                return self.route_hook_settled(
                    message
                        .pointer("/response/request_id")
                        .and_then(Value::as_str),
                    false,
                );
            }
            Some("control_request") => return self.route_control(message, true),
            Some("control_cancel_request") => return self.route_control(message, false),
            _ => {}
        }
        let scope = claude_frame_scope(message);
        if scope == ClaudeFrameScope::Nested {
            // A subagent's frames move no root turn and touch nothing of the
            // root parser: not its pending tools, text stream, approvals,
            // permission state, retry count or terminal owner. Its result
            // and lifecycle frames are not parsed at all; its other frames
            // are work done inside the root attempt, a barrier to replay.
            let (parser, replay) = match frame_type {
                Some("result" | "command_lifecycle") => {
                    (ClaudeParserStep::Skip, ClaudeReplayStep::Retain)
                }
                _ => (ClaudeParserStep::Nested, ClaudeReplayStep::Block),
            };
            let mut plan =
                ClaudeFramePlan::new(scope, ClaudeFrameOwnership::Unchanged, parser, replay);
            // A subagent's work is credited to no grant
            // (`claude_outstanding_work.rs`).
            plan.origin = ClaudeWorkOrigin::Unattributed;
            return plan;
        }
        if frame_type == Some("result") {
            return self.route_result(scope, message, parser_replay_safe);
        }

        let mut ownership = lock_claude_turn_ownership(&self.ownership);
        let stale = claude_frame_is_turn_output(message) && ownership.names_only_retired(message);
        // Read before `observe`, which may take the matched prompt.
        let echo_of_waiting =
            frame_type == Some("user") && ownership.is_exact_echo_of_waiting_prompt(message);
        let observed = ownership.observe(message);
        // The frame's work is the open prompt attempt's, if a prompt's turn
        // is open now.
        let origin = claude_open_turn_origin(&ownership);
        let turn_open = ownership.turn_is_open();
        // Only while a written prompt waits for its turn can a frame outside
        // every turn be that prompt's effect (a prompt hook, say).
        let between_turns = !turn_open && ownership.prompt_is_waiting();
        drop(ownership);
        if stale {
            return ClaudeFramePlan::new(
                scope,
                observed,
                ClaudeParserStep::Skip,
                ClaudeReplayStep::Retain,
            );
        }

        if between_turns
            && !claude_frame_is_bookkeeping(message, echo_of_waiting, !self.saw_process_init)
        {
            self.barrier_between_turns = true;
        }
        if frame_type == Some("system")
            && message.get("subtype").and_then(Value::as_str) == Some("init")
        {
            self.saw_process_init = true;
        }
        let lifecycle = frame_type == Some("command_lifecycle");
        let parser = match (self.prepare_opening(&observed), lifecycle) {
            (Some(carry_barrier), true) => ClaudeParserStep::Open { carry_barrier },
            (Some(carry_barrier), false) => ClaudeParserStep::Root {
                open: true,
                carry_barrier,
            },
            (None, true) => ClaudeParserStep::Skip,
            (None, false) => ClaudeParserStep::Root {
                open: false,
                carry_barrier: false,
            },
        };
        let mut plan = ClaudeFramePlan::new(scope, observed, parser, ClaudeReplayStep::Retain);
        plan.origin = origin;
        plan
    }

    /// The one place where the opening the ownership ledger decided for a
    /// frame (`observed`) becomes the root parser's preparation, whatever
    /// carried it: a lifecycle `started`, turn output, an echo or a control
    /// request (a result that opens and ends its attempt is
    /// `route_result`'s, from the ledger's own provenance). Returns the
    /// inherited barrier the preparation carries, or `None` when the frame
    /// opens nothing to prepare. The frame application performs the
    /// preparation once, before any of the frame's effects.
    /// - A prompt's attempt is bound to the parser and takes the barrier
    ///   seen between turns while it waited.
    /// - A turn Claude Code started, or one no prompt owns, gets a clean
    ///   parser and binds no attempt. It keeps its own provenance (adoption,
    ///   generation, no grant), and the barrier stays for the waiting prompt.
    /// - A waiting prompt taken up inside a turn no prompt owned (a mixed
    ///   join), an unresolved turn, and every frame that opens nothing keep
    ///   the parser as it is: what ran before is not reset away.
    fn prepare_opening(&mut self, observed: &ClaudeFrameOwnership) -> Option<bool> {
        match observed {
            ClaudeFrameOwnership::OpenedHost(owner) => {
                self.parser_attempt = Some(owner.attempt_uuid.clone());
                Some(std::mem::take(&mut self.barrier_between_turns))
            }
            ClaudeFrameOwnership::OpenedRuntime { .. } => {
                self.parser_attempt = None;
                Some(false)
            }
            ClaudeFrameOwnership::JoinedHost(_)
            | ClaudeFrameOwnership::BecameUnresolved
            | ClaudeFrameOwnership::Unchanged => None,
        }
    }

    /// A hook callback: the process's, answered by the hook responder. It
    /// observes no turn, so it opens and closes none, and it is never parsed.
    /// It bars the attempt it reaches from replay, as every control request
    /// does: its answer changes what the continuing model sees, and another
    /// hook may do anything. Arriving while a written prompt waits outside
    /// every turn, it bars that prompt's next attempt.
    fn route_hook_callback(&mut self, request_id: Option<&str>) -> ClaudeFramePlan {
        let ownership = lock_claude_turn_ownership(&self.ownership);
        let between_turns = !ownership.turn_is_open() && ownership.prompt_is_waiting();
        drop(ownership);
        if between_turns {
            self.barrier_between_turns = true;
        }
        if let Some(request_id) = request_id {
            // At most a few are ever pending; bound what a runtime that never
            // settles them can leave behind.
            if self.hook_requests.len() >= CLAUDE_CONTROL_ORIGIN_LIMIT {
                self.hook_requests.clear();
            }
            self.hook_requests.insert(request_id.to_owned());
        }
        let mut plan = ClaudeFramePlan::new(
            ClaudeFrameScope::Process,
            ClaudeFrameOwnership::Unchanged,
            ClaudeParserStep::Skip,
            ClaudeReplayStep::Block,
        );
        plan.control = Some(ClaudeControlStep::HookCallback);
        plan
    }

    /// A hook callback's cancellation (`cancel`) or the runtime's echo of its
    /// answer. Neither does anything new: no turn is observed, nothing is
    /// parsed, and no barrier is set or cleared.
    fn route_hook_settled(&mut self, request_id: Option<&str>, cancel: bool) -> ClaudeFramePlan {
        if let Some(request_id) = request_id {
            self.hook_requests.remove(request_id);
        }
        let mut plan = ClaudeFramePlan::new(
            ClaudeFrameScope::Process,
            ClaudeFrameOwnership::Unchanged,
            ClaudeParserStep::Skip,
            ClaudeReplayStep::Retain,
        );
        if cancel {
            plan.control = Some(ClaudeControlStep::HookCallbackCancel);
        }
        plan
    }

    /// A control request (`request`) or cancellation: its origin is resolved
    /// here, and its effects are carried out by the frame application. A
    /// control frame is never parsed, and it bars the attempt it reaches from
    /// replay: a permission request may already have led to an effect.
    fn route_control(&mut self, message: &Value, request: bool) -> ClaudeFramePlan {
        let parent = match message.get("parent_tool_use_id") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(parent)) => Ok(Some(parent.clone())),
            Some(_) => Err(()),
        };
        let mut observed = ClaudeFrameOwnership::Unchanged;
        let mut parser = ClaudeParserStep::Skip;
        let mut root_origin = ClaudeWorkOrigin::Unattributed;
        let (scope, origin) = match parent {
            Ok(Some(parent_tool_use_id)) => (
                ClaudeFrameScope::Nested,
                ClaudeControlOrigin::Nested { parent_tool_use_id },
            ),
            Err(()) => (ClaudeFrameScope::Process, ClaudeControlOrigin::Unresolved),
            Ok(None) => {
                let mut ownership = lock_claude_turn_ownership(&self.ownership);
                if ownership.names_only_retired(message) {
                    (ClaudeFrameScope::Process, ClaudeControlOrigin::Unresolved)
                } else {
                    // A request outside every turn opens a turn no prompt
                    // owns: an unassigned or runtime-started segment whose
                    // effects are nobody's grant (`claude_turn_ownership.rs`).
                    // It never proves that a waiting prompt owns execution.
                    observed = ownership.observe(message);
                    root_origin = claude_open_turn_origin(&ownership);
                    let between_turns = !ownership.turn_is_open() && ownership.prompt_is_waiting();
                    drop(ownership);
                    if let Some(carry_barrier) = self.prepare_opening(&observed) {
                        // A request that opens a turn is prepared as any
                        // opening frame is, before the request has any
                        // effect; its own barrier is applied after that.
                        parser = ClaudeParserStep::Open { carry_barrier };
                    } else if between_turns {
                        // Only a cancellation stays outside every turn; it
                        // bars the waiting prompt's next attempt.
                        self.barrier_between_turns = true;
                    }
                    (ClaudeFrameScope::Process, ClaudeControlOrigin::Root)
                }
            }
        };
        let mut plan = ClaudeFramePlan::new(scope, observed, parser, ClaudeReplayStep::Block);
        plan.origin = match origin {
            ClaudeControlOrigin::Nested { .. } => ClaudeWorkOrigin::Unattributed,
            ClaudeControlOrigin::Root => root_origin,
            ClaudeControlOrigin::Unresolved => ClaudeWorkOrigin::Unattributed,
        };
        plan.control = Some(if request {
            ClaudeControlStep::Request(origin)
        } else {
            ClaudeControlStep::Cancel(origin)
        });
        plan
    }

    /// A top-level `result`: the turn it ended is resolved once, here, and
    /// decides the retry, the replay prompt and the finalization.
    fn route_result(
        &mut self,
        scope: ClaudeFrameScope,
        message: &Value,
        parser_replay_safe: bool,
    ) -> ClaudeFramePlan {
        let mut ownership = lock_claude_turn_ownership(&self.ownership);
        let (terminal, opened_by_result) = ownership.close_for_result_with_opening(message);
        let mut plan = ClaudeFramePlan::new(
            scope,
            ClaudeFrameOwnership::Unchanged,
            ClaudeParserStep::Root {
                open: false,
                carry_barrier: false,
            },
            ClaudeReplayStep::Retain,
        );
        match terminal {
            // A late duplicate: nothing at all, the open turn's parser state
            // included.
            ClaudeResultOwner::Stale => {
                plan.parser = ClaudeParserStep::Skip;
                return plan;
            }
            ClaudeResultOwner::Host(owner) => {
                plan.origin = ClaudeWorkOrigin::Attempt {
                    turn_generation: owner.turn_generation,
                };
                // A result that names a waiting attempt alone resolves it
                // even with no `started` before it; the attempt then opens
                // and ends with this frame, inheriting any barrier seen
                // while no turn was open.
                // The ledger says whether this result alone opened the
                // attempt. If it did, the attempt did nothing before it but
                // may inherit a barrier; if the ledger had it open already,
                // the parser must have been prepared for it, and its state
                // decides. Any disagreement is never replayed.
                let prepared = self.parser_attempt.as_deref() == Some(owner.attempt_uuid.as_str());
                let inherited_barrier =
                    opened_by_result && std::mem::take(&mut self.barrier_between_turns);
                let replay_safe = match (opened_by_result, prepared) {
                    (true, false) => !inherited_barrier,
                    (false, true) => parser_replay_safe,
                    _ => false,
                };
                self.parser_attempt = None;
                let held = claude_replay_generation(&self.replay_prompt).as_deref()
                    == Some(owner.replay_generation.as_str());
                let prior = match &self.retries {
                    Some((generation, count)) if *generation == owner.replay_generation => *count,
                    _ => 0,
                };
                if let Some(ClaudeTransientApiResult::Retry {
                    completed_attempts,
                    delay,
                    status,
                }) = classify_claude_transient_api_result(
                    message,
                    &self.session_id,
                    prior,
                    replay_safe && held,
                ) {
                    let ticket = ClaudeRetryTicket {
                        replay_generation: owner.replay_generation.clone(),
                        turn_generation: owner.turn_generation,
                        attempt: owner.attempt.saturating_add(1),
                    };
                    ownership.schedule_retry(ticket.clone());
                    self.retries = Some((owner.replay_generation.clone(), completed_attempts));
                    plan.parser = ClaudeParserStep::DiscardForRetry;
                    plan.retry = Some(ClaudeRetryPlan {
                        ticket,
                        delay,
                        status,
                        completed_attempts,
                    });
                    return plan;
                }
                self.retries = None;
                plan.parser = ClaudeParserStep::Root {
                    open: opened_by_result,
                    carry_barrier: false,
                };
                plan.replay = ClaudeReplayStep::Retire(owner.replay_generation.clone());
                plan.terminal = Some(ClaudeResultOwner::Host(owner));
            }
            // Taken up part-way by a turn no prompt owned: settled, never
            // replayed.
            ClaudeResultOwner::HostAfterUnowned(owner) => {
                plan.origin = ClaudeWorkOrigin::Attempt {
                    turn_generation: owner.turn_generation,
                };
                self.parser_attempt = None;
                self.retries = None;
                plan.replay = ClaudeReplayStep::Retire(owner.replay_generation.clone());
                plan.terminal = Some(ClaudeResultOwner::HostAfterUnowned(owner));
            }
            other => {
                self.parser_attempt = None;
                plan.terminal = Some(other);
            }
        }
        plan
    }
}

/// Applies a plan's parser step to the root parser state (`turn_state`) or
/// to the subagents' (`nested_state`). Whatever resets the root parser
/// clears the subagents' turn-local state with it, but not their pending
/// tool calls, whose results may arrive after the root turn ended
/// (`clear_claude_subagent_turn_state`).
fn apply_claude_parser_step(
    step: ClaudeParserStep,
    message: &Value,
    session_id: &mut Option<String>,
    turn_state: &mut ClaudeTurnState,
    nested_state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    match step {
        ClaudeParserStep::Skip => Ok(()),
        ClaudeParserStep::Root {
            open,
            carry_barrier,
        } => {
            if open {
                reset_claude_turn_state(turn_state, recorder)?;
                clear_claude_subagent_turn_state(nested_state);
                if carry_barrier {
                    turn_state.replay_became_unsafe = true;
                }
            }
            handle_claude_event(message, session_id, turn_state, recorder)?;
            // A top-level result reset the root parser (`handle_claude_event`).
            if message.get("type").and_then(Value::as_str) == Some("result") {
                clear_claude_subagent_turn_state(nested_state);
            }
            Ok(())
        }
        ClaudeParserStep::Open { carry_barrier } => {
            reset_claude_turn_state(turn_state, recorder)?;
            clear_claude_subagent_turn_state(nested_state);
            if carry_barrier {
                turn_state.replay_became_unsafe = true;
            }
            Ok(())
        }
        ClaudeParserStep::Nested => handle_claude_nested_event(message, nested_state, recorder),
        ClaudeParserStep::DiscardForRetry => {
            clear_claude_subagent_turn_state(nested_state);
            reset_claude_turn_state(turn_state, recorder)
        }
    }
}

/// A subagent's frame, parsed in the subagents' own state: its tool calls
/// are registered there and their results resolved there, so the commands
/// they run are recorded and observed. Its text and thinking are not
/// rendered (the subagent's outcome reaches the transcript through its
/// tool's result in the root turn), and it never finishes or resets the root
/// turn's text stream, pending tools, approvals or permission state. The
/// cards and diffs it records leave the recorder's open root text message
/// open too: the frame application runs this step under
/// `SessionRecorder::keeping_streaming_text`.
fn handle_claude_nested_event(
    message: &Value,
    nested_state: &mut ClaudeTurnState,
    recorder: &mut dyn TurnRecorder,
) -> Result<()> {
    match message.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let Some(contents) = message
                .pointer("/message/content")
                .and_then(Value::as_array)
            else {
                return Ok(());
            };
            for content in contents {
                if content.get("type").and_then(Value::as_str) == Some("tool_use") {
                    register_claude_tool_use(content, nested_state, recorder)?;
                }
            }
            Ok(())
        }
        Some("user") => handle_claude_tool_result(message, nested_state, recorder),
        _ => Ok(()),
    }
}
