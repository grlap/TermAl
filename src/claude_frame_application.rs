// Claude frame application: the one production function that carries out a
// stdout frame's plan, called by the reader loop and by the tests alike.
//
// Owns: applying each frame of one Claude runtime in a fixed order, from the
// router's plan (`claude_frame_router.rs`): the session effects of what the
// frame did to the runtime's turns; the replay barrier; session metadata it
// carries (native agent commands, model options); its control request or
// cancellation, by the origin the router resolved (root, a subagent, or
// unresolved); an automatic retry; the replay release; the parser step on
// the root or the subagents' parser state; and the finalization of the turn a
// result ended. A control request is answered or queued with the parser
// state of its own origin, and every request's origin is kept for this
// runtime, so a cancellation clears only a request it may name. It reports
// what the reader must do next: go on, stop, or stop the runtime.
//
// Does not own: reading or parsing stdout, or stopping the process
// (`claude_spawn.rs` does both, the latter on this file's explicit outcome),
// the routing decisions (`claude_frame_router.rs`), turn ownership
// (`claude_turn_ownership.rs`), the session paths it calls
// (`claude_runtime_turns.rs`, `turn_lifecycle.rs`), or how frames and
// permission requests are parsed and classified (`claude.rs`).
//
// New file: the per-frame body of the stdout reader loop in
// `claude_spawn.rs` moved here, so that loop parses a line and calls one
// function, and no second decision tree for control traffic runs beside the
// plan.

/// At most this many control requests' origins are kept per runtime, oldest
/// dropped first.
const CLAUDE_CONTROL_ORIGIN_LIMIT: usize = 256;

/// What one Claude runtime's stdout reader and stdin writer share with
/// every frame and command they apply. It holds no sender of the runtime's
/// command channel: the writer receives on that channel, and its loop ends
/// only once every sender is gone, so the sender stays reader-side
/// (`ClaudeReaderFrames::input_tx`).
#[derive(Clone)]
struct ClaudeRuntimeContext {
    state: AppState,
    session_id: String,
    token: RuntimeToken,
    ownership: ClaudeTurnOwnership,
    replay_prompt: ClaudeReplayPrompt,
    /// The session's own working directory, pre-normalized, for the
    /// read-only permission checker.
    cwd: String,
    /// The SessionStart compact hook this runtime registered at initialize,
    /// if any (`claude_compact_hook.rs`).
    compact_hook: Option<Arc<ClaudeCompactHook>>,
}

/// The control requests this runtime sent, by request id, with the origin
/// each came from.
#[derive(Default)]
struct ClaudeControlOrigins {
    origins: HashMap<String, ClaudeControlOrigin>,
    order: VecDeque<String>,
}

impl ClaudeControlOrigins {
    fn record(&mut self, request_id: &str, origin: ClaudeControlOrigin) {
        if self.origins.insert(request_id.to_owned(), origin).is_none() {
            self.order.push_back(request_id.to_owned());
            if self.order.len() > CLAUDE_CONTROL_ORIGIN_LIMIT {
                if let Some(oldest) = self.order.pop_front() {
                    self.origins.remove(&oldest);
                }
            }
        }
    }

    fn get(&self, request_id: &str) -> Option<&ClaudeControlOrigin> {
        self.origins.get(request_id)
    }

    fn forget(&mut self, request_id: &str) {
        if self.origins.remove(request_id).is_some() {
            self.order.retain(|known| known != request_id);
        }
    }
}

/// What one Claude runtime's stdout reader keeps between frames.
struct ClaudeReaderFrames {
    router: ClaudeFrameRouter,
    /// The root turn's parser state.
    root: ClaudeTurnState,
    /// The subagents' parser state: their tool calls and their permission
    /// requests, apart from the root turn's.
    nested: ClaudeTurnState,
    resolved_session_id: Option<String>,
    control_origins: ClaudeControlOrigins,
    initialize_model_options_tx:
        Option<Sender<std::result::Result<Vec<SessionModelOption>, String>>>,
    /// The reader's sender of the runtime's command channel: permission
    /// responses and delayed retries go to the writer through it.
    input_tx: Sender<ClaudeRuntimeCommand>,
    /// The compaction under way already asked for a fresh Engram context
    /// through its hook callback, so its compact_boundary does not ask again.
    /// A registered callback always asks itself, whatever this flag says, so
    /// a flag left by a compaction that ended without its boundary costs no
    /// later compaction its request. Reset when a compaction starts and when
    /// its boundary ends it.
    compaction_refresh_requested: bool,
}

impl ClaudeReaderFrames {
    fn new(
        context: &ClaudeRuntimeContext,
        input_tx: Sender<ClaudeRuntimeCommand>,
        initialize_model_options_tx: Option<
            Sender<std::result::Result<Vec<SessionModelOption>, String>>,
        >,
    ) -> Self {
        Self {
            router: ClaudeFrameRouter::new(
                context.ownership.clone(),
                context.replay_prompt.clone(),
                context.session_id.clone(),
            ),
            root: ClaudeTurnState::default(),
            nested: ClaudeTurnState::default(),
            resolved_session_id: None,
            control_origins: ClaudeControlOrigins::default(),
            initialize_model_options_tx,
            input_tx,
            compaction_refresh_requested: false,
        }
    }
}

/// What the reader does after a frame was applied.
#[derive(Debug, PartialEq, Eq)]
enum ClaudeFrameApplied {
    Continue,
    /// The turn failed (already recorded) and the reader stops.
    Stop,
    /// A control-protocol failure: the reader stops the runtime with this
    /// detail and stops reading.
    TerminateRuntime(String),
}

impl ClaudeRuntimeContext {
    fn new(
        state: AppState,
        session_id: String,
        token: RuntimeToken,
        ownership: ClaudeTurnOwnership,
        replay_prompt: ClaudeReplayPrompt,
        cwd: String,
    ) -> Self {
        Self {
            state,
            session_id,
            token,
            ownership,
            replay_prompt,
            cwd,
            compact_hook: None,
        }
    }

    /// This runtime's SessionStart compact hook, decided before initialize.
    fn with_compact_hook(mut self, compact_hook: Option<Arc<ClaudeCompactHook>>) -> Self {
        self.compact_hook = compact_hook;
        self
    }

    /// Records a turn failure for this runtime and tells the reader to stop.
    fn fail(&self, detail: &str) -> ClaudeFrameApplied {
        let _ = self
            .state
            .fail_turn_if_runtime_matches(&self.session_id, &self.token, detail);
        ClaudeFrameApplied::Stop
    }
}

/// Sends a permission response to the runtime's writer.
fn send_claude_permission_response(
    input_tx: &Sender<ClaudeRuntimeCommand>,
    decision: ClaudePermissionDecision,
    what: &str,
) -> Result<()> {
    input_tx
        .send(ClaudeRuntimeCommand::PermissionResponse(decision))
        .map_err(|err| anyhow!("failed to {what}: {err}"))
}

/// One applied frame: what the reader does next, and the plan that was
/// carried out (which the reader ignores and tests read).
struct ClaudeFrameOutcome {
    next: ClaudeFrameApplied,
    #[cfg_attr(not(test), allow(dead_code))]
    plan: ClaudeFramePlan,
}

/// Applies one stdout frame of the runtime `context` names: one decision for
/// the frame, before anything acts on it, then its effects in order.
fn apply_claude_frame(
    context: &ClaudeRuntimeContext,
    frames: &mut ClaudeReaderFrames,
    recorder: &mut SessionRecorder,
    message: &Value,
) -> ClaudeFrameOutcome {
    let plan = frames
        .router
        .route(message, !frames.root.replay_became_unsafe);
    let next = apply_claude_frame_plan(context, frames, recorder, message, &plan);
    ClaudeFrameOutcome { next, plan }
}

fn apply_claude_frame_plan(
    context: &ClaudeRuntimeContext,
    frames: &mut ClaudeReaderFrames,
    recorder: &mut SessionRecorder,
    message: &Value,
    plan: &ClaudeFramePlan,
) -> ClaudeFrameApplied {
    // A turn Claude Code started by itself is opened on its session here,
    // before its first frame is recorded.
    context.state.apply_claude_frame_ownership(
        &context.session_id,
        &context.token,
        &context.ownership,
        &plan.ownership,
    );
    // A frame that opens a prompt's attempt prepares the root parser for it
    // first, whatever the frame is (a lifecycle `started`, turn output, an
    // echo, a result or a control request): reset once, an inherited barrier
    // kept. The frame's own barrier below comes after, so the reset never
    // erases it, and the rest of the frame is applied without a second reset.
    let parser = match plan.parser {
        ClaudeParserStep::Open { carry_barrier }
        | ClaudeParserStep::Root {
            open: true,
            carry_barrier,
        } => {
            if let Err(err) = apply_claude_parser_step(
                ClaudeParserStep::Open { carry_barrier },
                message,
                &mut frames.resolved_session_id,
                &mut frames.root,
                &mut frames.nested,
                recorder,
            ) {
                return context.fail(&format!("failed to open the Claude turn: {err:#}"));
            }
            match plan.parser {
                ClaudeParserStep::Open { .. } => ClaudeParserStep::Skip,
                _ => ClaudeParserStep::Root {
                    open: false,
                    carry_barrier: false,
                },
            }
        }
        other => other,
    };
    if plan.replay == ClaudeReplayStep::Block {
        frames.root.replay_became_unsafe = true;
    }

    // Who produced what this frame records, fixed before any of its effects:
    // this runtime and the turn the router found for it. The sink checks it
    // again where each observation is applied (`claude_outstanding_work.rs`).
    let provenance = ClaudeObservationProvenance {
        token: context.token.clone(),
        origin: plan.origin.clone(),
    };
    recorder.set_observation_provenance(EngramObservationProvenance::Claude(provenance.clone()));
    // The work the frame starts is registered, and a frame that proves tool
    // activity for another turn excludes the live grant, before its handler
    // runs; what it proves ended is settled after its effects.
    let nested_frame = plan.scope == ClaudeFrameScope::Nested;
    let work = claude_frame_work(message, nested_frame);
    context
        .state
        .admit_claude_frame(&context.session_id, &provenance, nested_frame, &work);

    // Session metadata the runtime reports; it touches no turn's state.
    if let Some(agent_commands) = claude_agent_commands(message) {
        if let Err(err) = context
            .state
            .sync_session_agent_commands(&context.session_id, agent_commands)
        {
            return context.fail(&format!("failed to sync Claude agent commands: {err:#}"));
        }
    }
    if let Some(model_options) = claude_model_options(message) {
        if let Err(err) = context.state.sync_session_model_options(
            &context.session_id,
            None,
            model_options.clone(),
        ) {
            if let Some(tx) = frames.initialize_model_options_tx.take() {
                let _ = tx.send(Err(format!("failed to sync Claude model options: {err:#}")));
            }
            return context.fail(&format!("failed to sync Claude model options: {err:#}"));
        }
        if let Some(tx) = frames.initialize_model_options_tx.take() {
            let _ = tx.send(Ok(model_options));
        }
    }

    if let Some(control) = &plan.control {
        let applied = apply_claude_control(context, frames, recorder, message, control);
        context
            .state
            .give_claude_evidence_notices(&context.session_id, &context.token);
        return applied;
    }

    if let Some(retry) = plan.retry.clone() {
        let retry_detail = format!(
            "Claude API returned transient status {}; retrying automatically (attempt {} of \
             {CLAUDE_TRANSIENT_API_RETRY_ATTEMPTS}).",
            retry.status,
            retry.completed_attempts + 1
        );
        // The failed attempt ended (the router retired it); the parser is
        // reset and nothing of it is recorded.
        if let Err(err) = apply_claude_parser_step(
            parser,
            message,
            &mut frames.resolved_session_id,
            &mut frames.root,
            &mut frames.nested,
            recorder,
        ) {
            return context.fail(&format!(
                "failed to reset Claude turn for automatic retry: {err:#}"
            ));
        }
        let retry_context = context.clone();
        let retry_tx = frames.input_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(retry.delay);
            dispatch_claude_retry_if_current(
                &retry_context.state,
                &retry_context.session_id,
                &retry_context.token,
                &retry_tx,
                &retry_context.replay_prompt,
                &retry_context.ownership,
                &retry.ticket,
                &retry_detail,
            );
        });
        return ClaudeFrameApplied::Continue;
    }

    if let ClaudeReplayStep::Retire(replay_generation) = &plan.replay {
        clear_claude_replay_prompt_if_matches(&context.replay_prompt, replay_generation);
    }

    // One compaction asks once for a fresh context: its hook callback may
    // already have asked, before this boundary.
    if claude_event_starts_compaction(message) {
        frames.compaction_refresh_requested = false;
    }
    if claude_event_marks_engram_context_nudge(message) {
        if !std::mem::take(&mut frames.compaction_refresh_requested) {
            context
                .state
                .mark_engram_context_nudge_pending(&context.session_id);
        }
    }

    // A subagent's frame may record cards, diffs and error lines; none of
    // them closes the root turn's open text message (`keeping_streaming_text`).
    let parsed = if parser == ClaudeParserStep::Nested {
        recorder.keeping_streaming_text(|recorder| {
            apply_claude_parser_step(
                parser,
                message,
                &mut frames.resolved_session_id,
                &mut frames.root,
                &mut frames.nested,
                recorder,
            )
        })
    } else {
        apply_claude_parser_step(
            parser,
            message,
            &mut frames.resolved_session_id,
            &mut frames.root,
            &mut frames.nested,
            recorder,
        )
    };
    if let Err(err) = parsed {
        return context.fail(&format!("failed to handle Claude event: {err:#}"));
    }
    context
        .state
        .settle_claude_frame(&context.session_id, &context.token, &work);
    // The cards of the subagents' calls still pending survive the root
    // turn's next reset, so their late results update them.
    recorder.keep_command_messages(frames.nested.pending_tools.keys());
    // What the session owes about its evidence, once (`claude_outstanding_work.rs`).
    context
        .state
        .give_claude_evidence_notices(&context.session_id, &context.token);

    // A top-level result ends the turn the router resolved for it and
    // finalizes that turn's generation only; a subagent's result, a retried
    // one and a late duplicate end none.
    if let Some(owner) = plan.terminal.clone() {
        let is_error = message
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let error_summary = summarize_error(message);
        let error_detail = is_error.then_some(error_summary.as_str());
        // On a runtime that reports no message lifecycles, a prompt taken up
        // inside a turn the host did not adopt is never settled: nothing
        // names it again. The waiting prompt is not declared done; the
        // transcript says how to recover.
        let prompt_stranded = matches!(
            &owner,
            ClaudeResultOwner::Runtime(runtime) if runtime.adopted_generation.is_none()
        ) && {
            let ownership = lock_claude_turn_ownership(&context.ownership);
            !ownership.reports_lifecycles() && ownership.prompt_is_waiting()
        };
        if let Err(err) = context.state.finish_claude_result(
            &context.session_id,
            &context.token,
            owner,
            error_detail,
        ) {
            eprintln!(
                "runtime state warning> failed to finalize Claude turn for session `{}`: {err:#}",
                context.session_id
            );
        }
        if prompt_stranded {
            let _ = context.state.push_claude_system_notice(
                &context.session_id,
                &context.token,
                "Claude ended a turn TermAl did not start while a prompt was waiting. This \
                 Claude Code does not say which turn a prompt belongs to, so TermAl cannot tell \
                 whether the waiting prompt ran. If the session stays busy, stop the session.",
            );
        }
    }
    ClaudeFrameApplied::Continue
}

/// A control request or cancellation, by the origin the router resolved.
fn apply_claude_control(
    context: &ClaudeRuntimeContext,
    frames: &mut ClaudeReaderFrames,
    recorder: &mut SessionRecorder,
    message: &Value,
    control: &ClaudeControlStep,
) -> ClaudeFrameApplied {
    let request_id = message.get("request_id").and_then(Value::as_str);
    match control {
        ClaudeControlStep::HookCallback => {
            // A process-scoped callback: answered by the runtime's hook
            // responder, never with a permission decision, and it opens or
            // closes no turn.
            if answer_claude_hook_callback(context, &frames.input_tx, message)
                == ClaudeHookCallbackApplied::Compaction
            {
                frames.compaction_refresh_requested = true;
            }
            ClaudeFrameApplied::Continue
        }
        ClaudeControlStep::HookCallbackCancel => {
            if let Some(request_id) = request_id {
                cancel_claude_hook_callback(context, request_id);
            }
            ClaudeFrameApplied::Continue
        }
        ClaudeControlStep::Request(ClaudeControlOrigin::Unresolved) => {
            // Nobody's request may borrow the root turn's authority, and an
            // unanswered request would hang the runtime: refuse it.
            let Some(request_id) = request_id else {
                return ClaudeFrameApplied::TerminateRuntime(
                    "failed to handle Claude control request: a request TermAl cannot attribute \
                     carries no request id"
                        .to_owned(),
                );
            };
            eprintln!(
                "runtime state warning> Claude session `{}` sent control request `{request_id}` \
                 that TermAl cannot attribute to a turn; refusing it",
                context.session_id
            );
            let decision = ClaudePermissionDecision::Deny {
                request_id: request_id.to_owned(),
                message: "TermAl could not tell which turn this request belongs to, so it \
                          refused it."
                    .to_owned(),
            };
            match send_claude_permission_response(
                &frames.input_tx,
                decision,
                "refuse an unattributed request",
            ) {
                Ok(()) => ClaudeFrameApplied::Continue,
                Err(err) => ClaudeFrameApplied::TerminateRuntime(format!(
                    "failed to handle Claude control request: {err:#}"
                )),
            }
        }
        ClaudeControlStep::Request(origin) => {
            // Approval mode and delegation-child identity are read under one
            // state lock so the attendedness policy never sees a torn pair.
            let (approval_mode, delegation_child) = match context
                .state
                .claude_control_request_context(&context.session_id)
            {
                Ok(pair) => pair,
                Err(err) => {
                    return context.fail(&format!(
                        "failed to resolve Claude approval mode for session: {err:#}"
                    ));
                }
            };
            let nested = matches!(origin, ClaudeControlOrigin::Nested { .. });
            // A subagent's request is classified with the subagents' state:
            // it reads and writes none of the root turn's approvals,
            // unattended-question count or permission state.
            let parser_state = if nested {
                &mut frames.nested
            } else {
                &mut frames.root
            };
            let action = match classify_claude_control_request(
                message,
                parser_state,
                approval_mode,
                delegation_child,
                &context.cwd,
                context
                    .state
                    .claude_host_admission(&context.session_id, message),
            ) {
                Ok(action) => action,
                Err(err) => {
                    return ClaudeFrameApplied::TerminateRuntime(format!(
                        "failed to handle Claude control request: {err:#}"
                    ));
                }
            };
            if let Some(request_id) = request_id {
                frames.control_origins.record(request_id, origin.clone());
            }
            let Some(action) = action else {
                return ClaudeFrameApplied::Continue;
            };
            // The root turn's text block is closed before its own request's
            // card. A subagent's card closes neither the root parser's text
            // nor the recorder's open message, so the root text keeps one
            // message across it.
            let input_tx = &frames.input_tx;
            let result = if nested {
                recorder.keeping_streaming_text(|recorder| {
                    perform_claude_control_action(input_tx, recorder, action)
                })
            } else {
                finish_claude_assistant_text_stream(&mut frames.root, recorder)
                    .and_then(|_| perform_claude_control_action(input_tx, recorder, action))
            };
            match result {
                Ok(()) => ClaudeFrameApplied::Continue,
                Err(err) => ClaudeFrameApplied::TerminateRuntime(format!(
                    "failed to handle Claude control request: {err:#}"
                )),
            }
        }
        ClaudeControlStep::Cancel(origin) => {
            let Some(request_id) = request_id else {
                return ClaudeFrameApplied::Continue;
            };
            // Only a request this runtime sent may be cleared, and a
            // subagent may cancel only its own: a subagent's cancellation
            // never clears the root turn's request or another subagent's.
            // The wire contract names the request by its id, so a root
            // cancellation may clear any request of this runtime.
            let allowed = match (origin, frames.control_origins.get(request_id)) {
                (_, None) => false,
                (ClaudeControlOrigin::Root, Some(_)) => true,
                (
                    ClaudeControlOrigin::Nested { parent_tool_use_id },
                    Some(ClaudeControlOrigin::Nested {
                        parent_tool_use_id: recorded,
                    }),
                ) => parent_tool_use_id == recorded,
                (ClaudeControlOrigin::Nested { .. } | ClaudeControlOrigin::Unresolved, Some(_)) => {
                    false
                }
            };
            if !allowed {
                eprintln!(
                    "runtime state warning> Claude session `{}` sent a cancellation of request \
                     `{request_id}` that it may not name ({origin:?}); ignoring it",
                    context.session_id
                );
                return ClaudeFrameApplied::Continue;
            }
            frames.control_origins.forget(request_id);
            match context
                .state
                .clear_claude_pending_interaction_by_request(&context.session_id, request_id)
            {
                Ok(_) => ClaudeFrameApplied::Continue,
                // Without the owning session, the cancellation cannot be
                // reconciled with the persisted request card. Stop this
                // reader instead of accepting more control traffic for stale
                // state.
                Err(err) => ClaudeFrameApplied::TerminateRuntime(format!(
                    "failed to cancel Claude interaction request: {err:#}"
                )),
            }
        }
    }
}

/// Carries out a classified control request: queue its card, or answer it.
fn perform_claude_control_action(
    input_tx: &Sender<ClaudeRuntimeCommand>,
    recorder: &mut SessionRecorder,
    action: ClaudeControlRequestAction,
) -> Result<()> {
    use ClaudeControlRequestAction as Action;
    match action {
        Action::QueueApproval {
            title,
            command,
            detail,
            approval,
        } => recorder.push_claude_approval(&title, &command, &detail, approval),
        Action::QueueUserInput {
            title,
            detail,
            questions,
            request,
        } => recorder.push_claude_user_input_request(&title, &detail, questions, request),
        Action::Respond(decision) => {
            send_claude_permission_response(input_tx, decision, "auto-approve Claude tool request")
        }
        Action::RecordSelfResolvedQuestion {
            title,
            detail,
            questions,
            response,
        } => {
            // The audit card is recorded first so the transcript explains the
            // answer the runtime is about to receive.
            recorder.push_claude_self_resolved_user_input(&title, &detail, questions)?;
            send_claude_permission_response(input_tx, response, "self-resolve Claude question")
        }
        Action::RecordSelfResolvedQuestionError { detail, response } => {
            recorder.error(&detail)?;
            send_claude_permission_response(
                input_tx,
                response,
                "self-resolve malformed Claude question",
            )
        }
    }
}
