// Codex event-handling layer for the shared-app-server runtime.
//
// Codex (OpenAI's Codex CLI) runs as one long-lived helper process that
// hosts many TermAl sessions at once. Traffic flows as newline-delimited
// JSON-RPC over stdio: TermAl sends requests (prompts, approvals,
// cancellations), Codex replies with results and emits a stream of
// protocol events.
//
// Two inbound shapes arrive interleaved on the same wire:
//   * messages — carry a `method` + `params` and expect a response back
//     (permission approval, user input, MCP elicitation, generic
//     app-server request).
//   * notifications — same JSON-RPC shape but no response expected
//     (thread/turn lifecycle, task_complete, agent_message, model
//     rerouted, thread compacted, global notices, etc.).
//
// Routing: each inbound line is parsed, the session_id/turn_id/
// conversation_id are extracted from params (or derived from the
// thread_id via `thread_sessions`), the session's record is located in
// `AppState.inner`, a `BorrowedSessionRecorder` (TurnRecorder) is
// instantiated against that record, and the correct handler is
// dispatched.
//
// The three main handler families live here:
//   * `handle_shared_codex_app_server_message` — entry point; splits
//     JSON-RPC responses from method-bearing messages and routes by
//     method.
//   * `handle_shared_codex_app_server_notification` — dispatches
//     fire-and-forget events (thread/turn lifecycle, task complete,
//     agent message, model rerouted, thread compacted, global notices).
//   * `handle_codex_app_server_request` — handles inbound requests that
//     need a response (command/file-change/permissions approvals,
//     user input, MCP elicitation, generic app-server request).
//
// A cluster of event-matching helpers defends against stale events
// arriving from prior turns (Codex sometimes emits them late):
// `shared_codex_event_turn_id` extracts the turn id from the various
// pointer positions Codex uses; `shared_codex_event_matches_active_turn`
// matches only the in-flight turn; `shared_codex_event_matches_visible_turn`
// also accepts the most recently completed turn for finalization
// events; `shared_codex_app_server_event_matches_active_turn` is the
// variant used for method-bearing app-server traffic.
//
// Related modules: `src/codex.rs` owns the spawn/init + delta-dedup +
// stdout line helpers; `src/codex_rpc.rs` owns `send_codex_json_rpc_request`
// and `wait_for_codex_json_rpc_response`; `src/codex_validation.rs`
// validates request bodies before dispatch; `src/state.rs` +
// `src/session_interaction.rs` define the record-mutation pattern the
// recorders use.

/// Entry point for every inbound shared-app-server line. Splits JSON-RPC
/// responses (routed to the pending-request map) from method-bearing
/// messages, registers the session lazily if unseen, auto-rejects server
/// requests for unknown or runtime-mismatched sessions, resets stale
/// recorder/turn state when `turn/started` carries a fresh turn id, and
/// dispatches to the request or notification handler.
fn handle_shared_codex_app_server_message(
    message: &Value,
    state: &AppState,
    runtime_id: &str,
    pending_requests: &CodexPendingRequestMap,
    sessions: &SharedCodexSessionMap,
    thread_sessions: &SharedCodexThreadMap,
    input_tx: &Sender<CodexRuntimeCommand>,
) -> Result<()> {
    if let Some(response_id) = message.get("id") {
        if message.get("result").is_some() || message.get("error").is_some() {
            let key = codex_request_id_key(response_id);
            let sender = pending_requests
                .lock()
                .expect("Codex pending requests mutex poisoned")
                .remove(&key);
            if let Some(sender) = sender {
                let response = if let Some(result) = message.get("result") {
                    Ok(result.clone())
                } else {
                    Err(CodexResponseError::JsonRpc(summarize_codex_json_rpc_error(
                        message.get("error").unwrap_or(&Value::Null),
                    )))
                };
                let _ = sender.send(response);
            }
            return Ok(());
        }
    }

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        log_unhandled_codex_event("Codex app-server message missing method", message);
        return Ok(());
    };

    if method == "account/rateLimits/updated" {
        let Some(rate_limits) = message.pointer("/params/rateLimits") else {
            log_unhandled_codex_event(
                "Codex rate limit notification missing params.rateLimits",
                message,
            );
            return Ok(());
        };

        match serde_json::from_value::<CodexRateLimits>(rate_limits.clone()) {
            Ok(rate_limits) => state.note_codex_rate_limits(rate_limits)?,
            Err(err) => {
                log_unhandled_codex_event(
                    &format!("failed to parse Codex rate limits notification: {err}"),
                    message,
                );
            }
        }
        return Ok(());
    }

    if handle_shared_codex_global_notice(method, message, state)? {
        return Ok(());
    }

    let Some(thread_id) = shared_codex_session_thread_id(method, message) else {
        match method {
            "thread/archived"
            | "thread/closed"
            | "thread/compacted"
            | "thread/name/updated"
            | "thread/realtime/closed"
            | "thread/realtime/error"
            | "thread/realtime/itemAdded"
            | "thread/realtime/outputAudio/delta"
            | "thread/realtime/started"
            | "thread/status/changed"
            | "thread/tokenUsage/updated" => return Ok(()),
            _ => {
                if let Some(notice) = build_shared_codex_runtime_notice(method, message) {
                    state.note_codex_notice(notice)?;
                    return Ok(());
                }
                log_unhandled_codex_event(
                    &format!("shared Codex event missing thread id for `{method}`"),
                    message,
                );
                return Ok(());
            }
        }
    };

    let Some(session_id) = find_shared_codex_session_id(state, thread_sessions, thread_id) else {
        // Auto-reject server requests for unknown sessions so Codex does not
        // hang waiting for a response that will never come.
        reject_undeliverable_codex_server_request(message, input_tx);
        return Ok(());
    };
    let runtime_token = RuntimeToken::Codex(runtime_id.to_owned());
    if !state.session_matches_runtime_token(&session_id, &runtime_token) {
        reject_undeliverable_codex_server_request(message, input_tx);
        return Ok(());
    }

    let mut shared_sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    // Lazy registration: the session may exist in state.inner (via
    // find_shared_codex_session_id) but not yet in the shared session map
    // because remember_shared_codex_thread hasn't run. Insert a default
    // entry so early events from Codex are not dropped.
    let session_state = shared_sessions.entry(session_id.clone()).or_default();
    let SharedCodexSessionState {
        pending_turn_start_request_id,
        recorder: recorder_state,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        turn_state,
    } = session_state;
    let turn_started_turn_id = (method == "turn/started")
        .then(|| message.pointer("/params/turn/id").and_then(Value::as_str))
        .flatten();
    if method == "turn/started" && turn_started_turn_id != turn_id.as_deref() {
        clear_shared_codex_turn_recorder_state(recorder_state);
        clear_codex_turn_state(turn_state);
        *pending_turn_start_request_id = None;
        *completed_turn_id = None;
        *turn_started = false;
    }
    let mut recorder = BorrowedSessionRecorder::new(state, &session_id, recorder_state);

    if message.get("id").is_some() {
        let event_turn_id = shared_codex_event_turn_id(message);
        if !shared_codex_app_server_event_matches_active_turn(
            turn_id.as_deref(),
            *turn_started,
            event_turn_id,
        ) {
            return Ok(());
        }
        return handle_codex_app_server_request(method, message, &mut recorder);
    }

    handle_shared_codex_app_server_notification(
        method,
        message,
        state,
        &session_id,
        &runtime_token,
        sessions,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        pending_turn_start_request_id,
        turn_state,
        thread_sessions,
        &mut recorder,
    )
}


/// Dispatches fire-and-forget notifications (no response expected) to
/// the appropriate per-event handler. Owns the session-scoped mutable
/// state: thread-id registration on `thread/started`, turn-id tracking
/// and recorder finalize on `turn/started`/`turn/completed`, error
/// handling (retryable vs fatal), and subagent-result flushing. Quietly
/// ignores events whose turn id does not match the active turn via the
/// `shared_codex_app_server_event_matches_active_turn` guard, and drops
/// uninteresting realtime/token-usage notifications that TermAl does
/// not surface.
fn handle_shared_codex_app_server_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime_token: &RuntimeToken,
    sessions: &SharedCodexSessionMap,
    session_thread_id: &mut Option<String>,
    turn_id: &mut Option<String>,
    completed_turn_id: &mut Option<String>,
    turn_started: &mut bool,
    pending_turn_start_request_id: &mut Option<String>,
    turn_state: &mut CodexTurnState,
    thread_sessions: &SharedCodexThreadMap,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match method {
        "thread/started" => {
            if let Some(thread_id) = message.pointer("/params/thread/id").and_then(Value::as_str) {
                let previous_thread_id = session_thread_id.replace(thread_id.to_owned());
                *turn_id = None;
                *completed_turn_id = None;
                *turn_started = false;
                *pending_turn_start_request_id = None;
                let mut thread_sessions = thread_sessions
                    .lock()
                    .expect("shared Codex thread mutex poisoned");
                if let Some(previous_thread_id) = previous_thread_id {
                    if previous_thread_id != thread_id {
                        thread_sessions.remove(&previous_thread_id);
                    }
                }
                thread_sessions.insert(thread_id.to_owned(), session_id.to_owned());
                state.set_external_session_id(session_id, thread_id.to_owned())?;
                recorder.note_external_session(thread_id)?;
            }
        }
        "thread/archived" => {
            state.set_codex_thread_state_if_runtime_matches(
                session_id,
                runtime_token,
                CodexThreadState::Archived,
            )?;
        }
        "thread/unarchived" => {
            state.set_codex_thread_state_if_runtime_matches(
                session_id,
                runtime_token,
                CodexThreadState::Active,
            )?;
        }
        "turn/started" => {
            let next_turn_id = message.pointer("/params/turn/id").and_then(Value::as_str);
            let turn_changed = turn_id.as_deref() != next_turn_id;
            *turn_id = next_turn_id.map(str::to_owned);
            *completed_turn_id = None;
            *turn_started = true;
            *pending_turn_start_request_id = None;
            if turn_changed {
                recorder.finish_streaming_text()?;
            }
        }
        "turn/completed" => {
            if let Some(error) = message.pointer("/params/turn/error") {
                if !error.is_null() {
                    *turn_id = None;
                    *completed_turn_id = None;
                    *turn_started = false;
                    *pending_turn_start_request_id = None;
                    clear_codex_turn_state(turn_state);
                    recorder.reset_turn_state()?;
                    state.fail_turn_if_runtime_matches(
                        session_id,
                        runtime_token,
                        &summarize_error(error),
                    )?;
                    return Ok(());
                }
            }

            *completed_turn_id = turn_id.clone().or_else(|| {
                message
                    .pointer("/params/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            *turn_id = None;
            *turn_started = false;
            *pending_turn_start_request_id = None;
            flush_pending_codex_subagent_results(turn_state, recorder)?;
            // Keep the streaming text message id alive through the completed-turn
            // grace window. Codex can emit the canonical final agent message after
            // turn/completed; if the streamed chunks were deduped incorrectly or
            // otherwise diverged, that late final must replace the existing bubble
            // in place rather than appending a second message. The cleanup worker
            // or the next turn/started event clears this recorder state.
            state.finish_turn_ok_if_runtime_matches(session_id, runtime_token)?;
            if let Some(completed_turn_id) = completed_turn_id.as_deref() {
                schedule_shared_codex_completed_turn_cleanup(
                    sessions,
                    session_id,
                    completed_turn_id,
                );
            }
        }
        "item/started" => {
            let event_turn_id = shared_codex_event_turn_id(message);
            if !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            if let Some(item) = message.get("params").and_then(|params| params.get("item")) {
                handle_codex_app_server_item_started(item, recorder)?;
            }
        }
        "item/completed" => {
            let Some(item) = message.get("params").and_then(|params| params.get("item")) else {
                return Ok(());
            };
            let event_turn_id = shared_codex_event_turn_id(message);
            let matches_completed_agent_message = turn_id.is_none()
                && completed_turn_id.is_some()
                && matches!(item.get("type").and_then(Value::as_str), Some("agentMessage"))
                && match event_turn_id {
                    Some(event) => completed_turn_id.as_deref() == Some(event),
                    None => true,
                };
            if !matches_completed_agent_message
                && !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            handle_codex_app_server_item_completed(item, state, session_id, turn_state, recorder)?;
        }
        "item/agentMessage/delta" => {
            let event_turn_id = shared_codex_event_turn_id(message);
            let matches_completed_turn = turn_id.is_none()
                && completed_turn_id.is_some()
                && match event_turn_id {
                    Some(event) => completed_turn_id.as_deref() == Some(event),
                    None => true,
                };
            if !matches_completed_turn
                && !shared_codex_app_server_event_matches_active_turn(
                turn_id.as_deref(),
                *turn_started,
                event_turn_id,
            ) {
                return Ok(());
            }
            let Some(delta) = message.pointer("/params/delta").and_then(Value::as_str) else {
                return Ok(());
            };
            let Some(item_id) = message.pointer("/params/itemId").and_then(Value::as_str) else {
                return Ok(());
            };
            record_codex_agent_message_delta(
                turn_state, recorder, state, session_id, item_id, delta,
            )?;
        }
        "model/rerouted" => {
            handle_shared_codex_model_rerouted(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "thread/compacted" => {
            handle_shared_codex_thread_compacted(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "thread/status/changed"
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
        "error" => {
            *turn_id = None;
            *completed_turn_id = None;
            *turn_started = false;
            *pending_turn_start_request_id = None;
            clear_codex_turn_state(turn_state);
            recorder.reset_turn_state()?;
            let payload = message.get("params").unwrap_or(message);
            let detail = summarize_error(payload);

            if is_retryable_connectivity_error(payload) {
                state.note_turn_retry_if_runtime_matches(session_id, runtime_token, &detail)?;
            } else {
                state.fail_turn_if_runtime_matches(session_id, runtime_token, &detail)?;
            }
        }
        "codex/event/item_completed" => {
            handle_shared_codex_event_item_completed(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "codex/event/agent_message_content_delta" => {
            handle_shared_codex_event_agent_message_content_delta(
                message,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
                state,
                session_id,
            )?;
        }
        "codex/event/agent_message" => {
            handle_shared_codex_event_agent_message(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                completed_turn_id.as_deref(),
                turn_state,
                recorder,
            )?;
        }
        "codex/event/task_complete" => {
            handle_shared_codex_task_complete(
                message,
                state,
                session_id,
                turn_id.as_deref(),
                turn_state,
            )?;
        }
        _ if method.starts_with("codex/event/") => {}
        _ => {
            log_unhandled_codex_event(
                &format!("unhandled shared Codex app-server notification `{method}`"),
                message,
            );
        }
    }

    Ok(())
}

/// Records a subagent `task_complete` summary. Drops events whose turn
/// id does not match the active turn. If the main assistant output has
/// already started (a visible assistant message id is anchored), the
/// summary is inserted *before* that anchor so subagent results appear
/// above the final assistant reply; otherwise it is buffered and flushed
/// later by `flush_pending_codex_subagent_results`.
fn handle_shared_codex_task_complete(
    message: &Value,
    state: &AppState,
    session_id: &str,
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
    if trimmed.is_empty() {
        return Ok(());
    }

    let conversation_id = message
        .pointer("/params/conversationId")
        .and_then(Value::as_str);
    let turn_id = shared_codex_event_turn_id(message);
    if current_turn_id.is_none() {
        return Ok(());
    }
    if !shared_codex_event_matches_active_turn(current_turn_id, turn_id) {
        return Ok(());
    }

    if let Some(anchor_message_id) = turn_state.first_visible_assistant_message_id.as_deref() {
        state.insert_message_before(
            session_id,
            anchor_message_id,
            Message::SubagentResult {
                id: state.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Subagent completed".to_owned(),
                summary: trimmed.to_owned(),
                conversation_id: conversation_id.map(str::to_owned),
                turn_id: turn_id.map(str::to_owned),
            },
        )?;
        return Ok(());
    }

    buffer_codex_subagent_result(
        turn_state,
        "Subagent completed",
        trimmed,
        conversation_id,
        turn_id,
    );
    Ok(())
}

/// Extracts the turn id from a Codex event by probing the several
/// pointer locations Codex uses across protocol versions (`msg.turn_id`,
/// `turnId`, `turn_id`, `id`, `turn.id`).
fn shared_codex_event_turn_id<'a>(message: &'a Value) -> Option<&'a str> {
    message
        .pointer("/params/msg/turn_id")
        .and_then(Value::as_str)
        .or_else(|| message.pointer("/params/turnId").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn_id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/id").and_then(Value::as_str))
        .or_else(|| message.pointer("/params/turn/id").and_then(Value::as_str))
}

/// Returns `true` when an event belongs to the currently in-flight
/// turn. Events without a turn id ride along with the active turn;
/// events with a turn id must match exactly. Used to drop stale events
/// that Codex sometimes emits after a turn has ended.
fn shared_codex_event_matches_active_turn(
    current_turn_id: Option<&str>,
    event_turn_id: Option<&str>,
) -> bool {
    match current_turn_id {
        Some(current) => {
            event_turn_id.is_none() || matches!(event_turn_id, Some(event) if current == event)
        }
        None => false,
    }
}

/// Matches shared Codex final-output events against either the active turn or
/// the most recently completed turn.
fn shared_codex_event_matches_visible_turn(
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

/// Variant of `shared_codex_event_matches_active_turn` for
/// method-bearing app-server traffic: events without a turn id are
/// accepted only once `turn/started` has already arrived, preventing
/// events from leaking in before the turn is officially open.
fn shared_codex_app_server_event_matches_active_turn(
    current_turn_id: Option<&str>,
    turn_started: bool,
    event_turn_id: Option<&str>,
) -> bool {
    match current_turn_id {
        Some(current) => match event_turn_id {
            Some(event) => current == event,
            None => turn_started,
        },
        None => false,
    }
}

/// Inserts an assistant-authored notice message into the session,
/// placing it above the final assistant reply if the first visible
/// assistant message has already been anchored; otherwise appends at
/// the tail.
fn push_shared_codex_turn_notice(
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
    text: &str,
) -> Result<()> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let message = Message::Text {
        attachments: Vec::new(),
        id: state.allocate_message_id(),
        timestamp: stamp_now(),
        author: Author::Assistant,
        text: trimmed.to_owned(),
        expanded_text: None,
    };

    if let Some(anchor_message_id) = turn_state.first_visible_assistant_message_id.as_deref() {
        state.insert_message_before(session_id, anchor_message_id, message)?;
        return Ok(());
    }

    state.push_message(session_id, message)
}

/// Surfaces a `model/rerouted` event as an inline notice explaining
/// that Codex switched models mid-turn (including the cyber-activity
/// reason when reported). Drops events whose turn id does not match
/// the active turn and silently ignores reroutes where the source and
/// destination models are identical.
fn handle_shared_codex_model_rerouted(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    _recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
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
    let notice = format!("Codex rerouted this turn from `{from_model}` to `{to_model}`{reason}.");
    push_shared_codex_turn_notice(state, session_id, turn_state, &notice)
}

/// Surfaces a `thread/compacted` event as an inline notice so users
/// see that Codex reduced the thread's context for the current turn.
/// Drops events whose turn id does not match the active turn.
fn handle_shared_codex_thread_compacted(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    _recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_active_turn(current_turn_id, event_turn_id) {
        return Ok(());
    }

    push_shared_codex_turn_notice(
        state,
        session_id,
        turn_state,
        "Codex compacted the thread context for this turn.",
    )
}

/// Reconciles a fully-delivered agent-message item against whatever
/// has already been streamed for the same `item_id`. If nothing was
/// streamed, the trimmed text is pushed wholesale; otherwise the
/// delta-suffix dedup logic computes append/replace/no-change against
/// the already-seen stream content. Finalizes any prior streaming text
/// when switching to a new `item_id`.
fn record_completed_codex_agent_message(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
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
        begin_codex_assistant_output(turn_state, recorder)?;
        recorder.push_text(trimmed)?;
        return remember_codex_first_assistant_message_id(state, session_id, turn_state);
    }

    let entry = turn_state
        .streamed_agent_message_text_by_item_id
        .entry(item_id.to_owned())
        .or_default();
    let update = next_completed_codex_text_update(entry, trimmed);
    if matches!(update, CompletedTextUpdate::NoChange) {
        return Ok(());
    }

    begin_codex_assistant_output(turn_state, recorder)?;
    match update {
        CompletedTextUpdate::NoChange => Ok(()),
        CompletedTextUpdate::Append(unseen_suffix) => recorder.text_delta(&unseen_suffix),
        CompletedTextUpdate::Replace(replacement_text) => {
            recorder.replace_streaming_text(&replacement_text)
        }
    }?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Handles the Codex-specific `codex/event/item_completed` payload
/// (agent message or command execution). Accepts events from either
/// the active turn or the most recently completed turn so late-arriving
/// finalizations still land correctly.
fn handle_shared_codex_event_item_completed(
    message: &Value,
    state: &AppState,
    session_id: &str,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        event_turn_id,
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
                record_completed_codex_agent_message(
                    turn_state, recorder, state, session_id, item_id, text,
                )?;
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

/// Concatenates the `Text`-typed parts of a multi-part Codex content
/// array into a single string, skipping other part types. Returns
/// `None` when no text parts were found.
fn concatenate_codex_text_parts(content: &[Value]) -> Option<String> {
    let mut combined = String::new();

    for part in content {
        if part.get("type").and_then(Value::as_str) != Some("Text") {
            continue;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        combined.push_str(text);
    }

    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}



