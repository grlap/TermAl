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

/// Intercepts top-level `configWarning` / `deprecationNotice` events
/// that are not scoped to a thread and records them as session-wide
/// notices. Returns `true` when the event was a recognized global
/// notice so the caller can short-circuit further dispatch.
fn handle_shared_codex_global_notice(
    method: &str,
    message: &Value,
    state: &AppState,
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

    if let Some(notice) = notice {
        state.note_codex_notice(notice)?;
    } else {
        log_unhandled_codex_event(
            &format!("failed to parse shared Codex global notice `{method}`"),
            message,
        );
    }

    Ok(true)
}

/// Builds a runtime-level notice for unknown method-bearing events that
/// lack a thread id — used as a fallback so diagnostics from Codex do
/// not get silently dropped.
fn build_shared_codex_runtime_notice(method: &str, message: &Value) -> Option<CodexNotice> {
    build_shared_codex_global_notice(
        CodexNoticeKind::RuntimeNotice,
        infer_shared_codex_notice_level(method, message),
        &format!("Codex notice: {method}"),
        message,
    )
}

/// Picks an info/warning severity for a runtime notice: prefers an
/// explicit `level`/`severity` field in the payload, otherwise infers
/// from keywords in the method name (warning/error/auth/maintenance
/// escalate to warning; everything else stays info).
fn infer_shared_codex_notice_level(method: &str, message: &Value) -> CodexNoticeLevel {
    let payload = message.get("params").unwrap_or(message);
    let severity = payload
        .get("level")
        .or_else(|| payload.get("severity"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase());

    match severity.as_deref() {
        Some("warning") | Some("warn") | Some("error") => CodexNoticeLevel::Warning,
        Some("info") | Some("notice") => CodexNoticeLevel::Info,
        _ => {
            let normalized = method.to_ascii_lowercase();
            if normalized.contains("warning")
                || normalized.contains("error")
                || normalized.contains("auth")
                || normalized.contains("maintenance")
            {
                CodexNoticeLevel::Warning
            } else {
                CodexNoticeLevel::Info
            }
        }
    }
}

/// Assembles a `CodexNotice` from a payload by probing a set of
/// pointer paths for code/title/detail. Returns `None` if none of the
/// pointers yielded enough text to form a notice worth surfacing.
fn build_shared_codex_global_notice(
    kind: CodexNoticeKind,
    level: CodexNoticeLevel,
    default_title: &str,
    message: &Value,
) -> Option<CodexNotice> {
    let payload = message.get("params").unwrap_or(message);
    let code = extract_shared_codex_notice_text(
        payload,
        &[
            "/code",
            "/id",
            "/warningCode",
            "/warning/code",
            "/deprecationId",
            "/deprecation/id",
        ],
    );
    let title = extract_shared_codex_notice_text(
        payload,
        &["/title", "/name", "/warning/title", "/deprecation/title"],
    );
    let detail = extract_shared_codex_notice_text(
        payload,
        &[
            "/detail",
            "/message",
            "/description",
            "/text",
            "/warning/message",
            "/warning/detail",
            "/deprecation/message",
            "/deprecation/detail",
        ],
    );

    let (title, detail) = match (title, detail, code.clone()) {
        (Some(title), Some(detail), _) => (title, detail),
        (Some(title), None, _) if title != default_title => (default_title.to_owned(), title),
        (None, Some(detail), _) => (default_title.to_owned(), detail),
        (None, None, Some(code)) => (default_title.to_owned(), format!("Code: `{code}`")),
        _ => return None,
    };

    Some(CodexNotice {
        kind,
        level,
        title,
        detail,
        timestamp: stamp_now(),
        code,
    })
}

/// Probes `payload` at each JSON pointer in order and returns the
/// first non-empty trimmed string found.
fn extract_shared_codex_notice_text(payload: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
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
            recorder.finish_streaming_text()?;
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

/// Resets the per-turn scratch state so nothing leaks into the next
/// turn: current agent-message id, streamed text/item-id caches,
/// buffered subagent results, the assistant-output-started flag, and
/// the visible-assistant-message anchor.
fn clear_codex_turn_state(turn_state: &mut CodexTurnState) {
    turn_state.current_agent_message_id = None;
    turn_state.streamed_agent_message_text_by_item_id.clear();
    turn_state.streamed_agent_message_item_ids.clear();
    turn_state.pending_subagent_results.clear();
    turn_state.assistant_output_started = false;
    turn_state.first_visible_assistant_message_id = None;
}

/// Clears shared Codex recorder state that should not leak across turns.
fn clear_shared_codex_turn_recorder_state(recorder_state: &mut SessionRecorderState) {
    reset_recorder_state_fields(recorder_state);
}

/// Spawns a background thread that waits on delayed cleanup notices
/// and clears completed-turn state once their due time passes. Holds
/// only a weak reference to the session map so the worker exits cleanly
/// when the runtime is torn down.
fn spawn_shared_codex_completed_turn_cleanup_worker(
    sessions: &SharedCodexSessionMap,
    cleanup_rx: mpsc::Receiver<SharedCodexCompletedTurnCleanup>,
) {
    let weak_sessions = Arc::downgrade(sessions);
    std::thread::Builder::new()
        .name("termal-codex-cleanup".to_owned())
        .spawn(move || {
            let mut pending = Vec::<SharedCodexCompletedTurnCleanup>::new();
            loop {
                if !run_due_shared_codex_completed_turn_cleanups(&weak_sessions, &mut pending) {
                    break;
                }

                let next_timeout = pending
                    .iter()
                    .map(|cleanup| cleanup.due_at)
                    .min()
                    .map(|due_at| due_at.saturating_duration_since(std::time::Instant::now()));

                let next_cleanup = match next_timeout {
                    Some(timeout) => match cleanup_rx.recv_timeout(timeout) {
                        Ok(cleanup) => Some(cleanup),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => match cleanup_rx.recv() {
                        Ok(cleanup) => Some(cleanup),
                        Err(_) => break,
                    },
                };

                if let Some(cleanup) = next_cleanup {
                    pending.push(cleanup);
                }
            }
        })
        .expect("failed to spawn shared Codex cleanup worker");
}

/// Processes every pending cleanup whose due time has passed, clearing
/// the associated session's completed-turn fields unless a new turn has
/// started in the meantime. Returns `false` when the session map has
/// been dropped so the worker loop can exit.
fn run_due_shared_codex_completed_turn_cleanups(
    weak_sessions: &std::sync::Weak<SharedCodexSessions>,
    pending: &mut Vec<SharedCodexCompletedTurnCleanup>,
) -> bool {
    let now = std::time::Instant::now();
    if !pending.iter().any(|cleanup| cleanup.due_at <= now) {
        return true;
    }

    let Some(sessions) = weak_sessions.upgrade() else {
        return false;
    };
    let mut sessions = sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let mut index = 0usize;
    while index < pending.len() {
        if pending[index].due_at > now {
            index += 1;
            continue;
        }

        let cleanup = pending.swap_remove(index);
        let Some(session_state) = sessions.get_mut(&cleanup.session_id) else {
            continue;
        };
        if session_state.turn_id.is_some()
            || session_state.completed_turn_id.as_deref() != Some(&cleanup.completed_turn_id)
        {
            continue;
        }
        clear_shared_codex_completed_turn_state_fields(
            &mut session_state.completed_turn_id,
            &mut session_state.turn_state,
            &mut session_state.recorder,
        );
    }
    true
}

/// Reads one newline-delimited line from a child stdout reader with a
/// byte cap. Lines over the cap are drained to the next newline and
/// reported via stderr rather than torn down, because legitimate
/// payloads (e.g. large `aggregatedOutput` from long command runs) can
/// exceed the cap but should not crash the runtime.
fn read_capped_child_stdout_line(
    reader: &mut impl BufRead,
    line_buf: &mut Vec<u8>,
    max_bytes: usize,
    stream_label: &str,
) -> io::Result<usize> {
    line_buf.clear();
    let mut total_read = 0usize;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(total_read);
        }

        let newline_index = available.iter().position(|byte| *byte == b'\n');
        let consume_len = newline_index.map_or(available.len(), |index| index + 1);
        if total_read + consume_len > max_bytes {
            // The line exceeds the safety cap.  Drain the remainder so the
            // reader stays aligned with the next newline-delimited message,
            // but discard the content instead of tearing down the runtime.
            // Legitimate large messages (e.g. aggregatedOutput from long
            // command executions) can exceed the cap.
            reader.consume(consume_len);
            total_read += consume_len;
            if newline_index.is_none() {
                loop {
                    let buf = reader.fill_buf()?;
                    if buf.is_empty() {
                        break;
                    }
                    let nl = buf.iter().position(|b| *b == b'\n');
                    let n = nl.map_or(buf.len(), |i| i + 1);
                    reader.consume(n);
                    total_read += n;
                    if nl.is_some() {
                        break;
                    }
                }
            }
            eprintln!(
                "[termal] skipping oversized {stream_label} line \
                 ({total_read} bytes, cap {max_bytes} bytes)"
            );
            line_buf.clear();
            return Ok(total_read);
        }

        line_buf.extend_from_slice(&available[..consume_len]);
        reader.consume(consume_len);
        total_read += consume_len;

        if newline_index.is_some() {
            return Ok(total_read);
        }
    }
}

/// Truncates a child-stdout line to `max_chars` characters, appending
/// an ellipsis if the limit was hit. Used when echoing stdout to the
/// TermAl log so oversized lines do not flood the console.
fn truncate_child_stdout_log_line(line: &str, max_chars: usize) -> String {
    let mut truncated = String::new();
    for (index, ch) in line.chars().enumerate() {
        if index == max_chars {
            truncated.push_str("...");
            return truncated;
        }
        truncated.push(ch);
    }
    truncated
}

/// Returns a user-facing failure detail when the child has emitted
/// enough consecutive non-JSON stdout lines to trip the streak limit.
/// Returns `None` below the threshold so a single bad line does not
/// fail the runtime. The raw line is deliberately omitted since it is
/// already logged per-line to stderr.
fn shared_codex_bad_json_streak_failure_detail(
    consecutive_bad_json_lines: usize,
    _line: &str,
) -> Option<String> {
    if consecutive_bad_json_lines < SHARED_CODEX_MAX_CONSECUTIVE_BAD_JSON_LINES {
        return None;
    }

    // Use a generic message for the user-facing failure detail.  Raw child
    // stdout content is already logged to stderr per-line as it arrives and
    // should not leak into persisted session state or SSE updates.
    Some(format!(
        "shared Codex app-server produced {consecutive_bad_json_lines} consecutive non-JSON stdout lines"
    ))
}

/// Clears the completed-turn id, per-turn scratch state, and recorder
/// state together — used both by the cleanup worker and before a new
/// turn starts.
fn clear_shared_codex_completed_turn_state_fields(
    completed_turn_id: &mut Option<String>,
    turn_state: &mut CodexTurnState,
    recorder_state: &mut SessionRecorderState,
) {
    *completed_turn_id = None;
    clear_codex_turn_state(turn_state);
    clear_shared_codex_turn_recorder_state(recorder_state);
}

/// Clears every shared-session per-turn field (pending turn-start
/// request id, active turn id, turn-started flag, plus completed-turn
/// state) so the next turn starts from a clean slate.
fn clear_shared_codex_turn_session_state(session_state: &mut SharedCodexSessionState) {
    session_state.pending_turn_start_request_id = None;
    session_state.turn_id = None;
    session_state.turn_started = false;
    clear_shared_codex_completed_turn_state_fields(
        &mut session_state.completed_turn_id,
        &mut session_state.turn_state,
        &mut session_state.recorder,
    );
}

/// Queues a completed-turn cleanup entry so the background worker can
/// clear the turn state after a short grace period.
fn schedule_shared_codex_completed_turn_cleanup(
    sessions: &SharedCodexSessionMap,
    session_id: &str,
    completed_turn_id: &str,
) {
    sessions.schedule_completed_turn_cleanup(session_id, completed_turn_id);
}

/// Returns `true` when an error chain contains a `session ... not
/// found` message — signalling that the session has already been torn
/// down so the dispatch failure is benign and should not surface as a
/// runtime error.
fn shared_codex_app_server_error_is_stale_session(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let message = cause.to_string();
        let Some(session_id) = message
            .strip_prefix("session `")
            .and_then(|message| message.strip_suffix("` not found"))
        else {
            return false;
        };

        !session_id.is_empty() && !session_id.contains('`')
    })
}

/// Queues a subagent result for later flushing. Subagent results must
/// appear *before* the final assistant reply, but arrive mid-turn
/// before TermAl knows which reply will be the final one; buffering
/// here lets `flush_pending_codex_subagent_results` drain them in order
/// at the right moment.
fn buffer_codex_subagent_result(
    turn_state: &mut CodexTurnState,
    title: &str,
    summary: &str,
    conversation_id: Option<&str>,
    turn_id: Option<&str>,
) {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return;
    }

    turn_state
        .pending_subagent_results
        .push(PendingSubagentResult {
            title: title.to_owned(),
            summary: trimmed.to_owned(),
            conversation_id: conversation_id.map(str::to_owned),
            turn_id: turn_id.map(str::to_owned),
        });
}

/// Drains every queued subagent result through the recorder in
/// insertion order. Called both from `begin_codex_assistant_output`
/// (just before the first visible assistant text) and on `turn/completed`
/// so unflushed results from silent turns still reach the session.
fn flush_pending_codex_subagent_results(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    for pending in std::mem::take(&mut turn_state.pending_subagent_results) {
        recorder.push_subagent_result(
            &pending.title,
            &pending.summary,
            pending.conversation_id.as_deref(),
            pending.turn_id.as_deref(),
        )?;
    }

    Ok(())
}

/// Marks the start of assistant output for this turn, flushing any
/// buffered subagent results first so they land above the assistant's
/// first visible message. Idempotent: subsequent calls within the same
/// turn are no-ops.
fn begin_codex_assistant_output(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    if !turn_state.assistant_output_started {
        flush_pending_codex_subagent_results(turn_state, recorder)?;
        turn_state.assistant_output_started = true;
    }

    Ok(())
}

/// Captures the id of the first visible assistant message of the turn
/// so subsequent notices (model rerouted, thread compacted, subagent
/// results) can be inserted *before* it rather than appended after.
fn remember_codex_first_assistant_message_id(
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
) -> Result<()> {
    if turn_state.first_visible_assistant_message_id.is_none() {
        turn_state.first_visible_assistant_message_id = state.last_message_id(session_id)?;
    }
    Ok(())
}

/// Handles `codex/event/agent_message_content_delta`, streaming the
/// delta into the recorder via `record_codex_agent_message_delta`.
/// Accepts events from either the active turn or the most recently
/// completed turn to handle late-arriving chunks.
fn handle_shared_codex_event_agent_message_content_delta(
    message: &Value,
    current_turn_id: Option<&str>,
    completed_turn_id: Option<&str>,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
) -> Result<()> {
    let event_turn_id = shared_codex_event_turn_id(message);
    if !shared_codex_event_matches_visible_turn(
        current_turn_id,
        completed_turn_id,
        event_turn_id,
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

    record_codex_agent_message_delta(turn_state, recorder, state, session_id, item_id, delta)
}

/// Handles `codex/event/agent_message` (the full assistant message).
/// When an `item_id` was already being streamed, reconciles with dedup;
/// otherwise pushes the text wholesale. Accepts events from either the
/// active turn or the most recently completed turn.
fn handle_shared_codex_event_agent_message(
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
        return record_completed_codex_agent_message(
            turn_state, recorder, state, session_id, &item_id, trimmed,
        );
    }

    begin_codex_assistant_output(turn_state, recorder)?;
    recorder.push_text(trimmed)?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Appends a streaming agent-message delta through the recorder after
/// running it past the delta-suffix dedup helper. Finalizes any prior
/// streaming text when the `item_id` changes.
fn record_codex_agent_message_delta(
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
    state: &AppState,
    session_id: &str,
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

    begin_codex_assistant_output(turn_state, recorder)?;
    turn_state
        .streamed_agent_message_item_ids
        .insert(item_id.to_owned());
    recorder.text_delta(&unseen_suffix)?;
    remember_codex_first_assistant_message_id(state, session_id, turn_state)
}

/// Defines the completed text update variants.
enum CompletedTextUpdate {
    NoChange,
    Append(String),
    Replace(String),
}

/// Reconciles the final `incoming` agent-message text against whatever
/// was already streamed in `existing`: returns `NoChange` when the
/// text is already fully seen, `Append` when the incoming value only
/// adds a suffix, and `Replace` when the text diverged and must be
/// rewritten in full.
fn next_completed_codex_text_update(existing: &mut String, incoming: &str) -> CompletedTextUpdate {
    if incoming.is_empty() {
        return CompletedTextUpdate::NoChange;
    }

    if existing.is_empty() {
        existing.push_str(incoming);
        return CompletedTextUpdate::Replace(incoming.to_owned());
    }

    if incoming == existing {
        return CompletedTextUpdate::NoChange;
    }

    if incoming.starts_with(existing.as_str()) {
        let split = existing.len();
        debug_assert!(incoming.is_char_boundary(split));
        let suffix = incoming[split..].to_owned();
        existing.clear();
        existing.push_str(incoming);
        return if suffix.is_empty() {
            CompletedTextUpdate::NoChange
        } else {
            CompletedTextUpdate::Append(suffix)
        };
    }

    if existing.ends_with(incoming) {
        return CompletedTextUpdate::NoChange;
    }

    existing.clear();
    existing.push_str(incoming);
    CompletedTextUpdate::Replace(incoming.to_owned())
}

/// Dedups a streaming delta: returns the portion of `incoming` not
/// already present in `existing`, advancing `existing` by the new
/// content. Handles exact-duplicate chunks, suffix-of-existing chunks,
/// and partial-overlap chunks (so retransmitted prefixes do not
/// duplicate on the wire).
fn next_codex_delta_suffix(existing: &mut String, incoming: &str) -> Option<String> {
    if incoming.is_empty() {
        return None;
    }

    if existing.is_empty() {
        existing.push_str(incoming);
        return Some(incoming.to_owned());
    }

    if incoming == existing {
        return None;
    }

    if incoming.starts_with(existing.as_str()) {
        let split = existing.len();
        debug_assert!(incoming.is_char_boundary(split));
        let suffix = incoming[split..].to_owned();
        existing.clear();
        existing.push_str(incoming);
        return if suffix.is_empty() {
            None
        } else {
            Some(suffix)
        };
    }

    if existing.ends_with(incoming) {
        return None;
    }

    let overlap = longest_codex_delta_overlap(existing, incoming);
    let suffix = incoming[overlap..].to_owned();
    existing.push_str(&suffix);
    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

/// Finds the longest prefix of `incoming` that matches a suffix of
/// `existing`, walking down from the largest possible overlap at char
/// boundaries for UTF-8 safety.
fn longest_codex_delta_overlap(existing: &str, incoming: &str) -> usize {
    let max_overlap = existing.len().min(incoming.len());
    for overlap in (1..=max_overlap).rev() {
        if incoming.is_char_boundary(overlap) && existing.ends_with(&incoming[..overlap]) {
            return overlap;
        }
    }

    0
}

/// Dispatches an inbound Codex server request that expects a response
/// back. Recognizes four purpose-built kinds — command-execution
/// approval, file-change approval, permissions approval, user-input
/// request, and MCP elicitation — and falls back to a generic
/// `CodexPendingAppRequest` for any other method. Each branch writes
/// a `Message` through the recorder describing the request for the
/// UI sidebar; the request id is retained so the later approval/response
/// plumbing can match the reply back to Codex.
fn handle_codex_app_server_request(
    method: &str,
    message: &Value,
    recorder: &mut impl CodexTurnRecorder,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("Codex app-server request missing id"))?;
    let params = message
        .get("params")
        .ok_or_else(|| anyhow!("Codex app-server request missing params"))?;

    match method {
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

            recorder.push_codex_approval(
                "Codex needs approval",
                command,
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::CommandExecution,
                    request_id,
                },
            )?;
        }
        "item/fileChange/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if reason.is_empty() {
                "Codex requested approval to apply file changes.".to_owned()
            } else {
                format!("Codex requested approval to apply file changes. Reason: {reason}")
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Apply file changes",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::FileChange,
                    request_id,
                },
            )?;
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

            recorder.push_codex_approval(
                "Codex needs approval",
                "Grant additional permissions",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::Permissions {
                        requested_permissions: params
                            .get("permissions")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    },
                    request_id,
                },
            )?;
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

            recorder.push_codex_user_input_request(
                "Codex needs input",
                &detail,
                questions.clone(),
                CodexPendingUserInput {
                    questions,
                    request_id,
                },
            )?;
        }
        "mcpServer/elicitation/request" => {
            let request: McpElicitationRequestPayload = serde_json::from_value(params.clone())
                .context("failed to parse Codex MCP elicitation request")?;
            let detail = describe_codex_mcp_elicitation_request(&request);

            recorder.push_codex_mcp_elicitation_request(
                "Codex needs MCP input",
                &detail,
                request.clone(),
                CodexPendingMcpElicitation {
                    request,
                    request_id,
                },
            )?;
        }
        _ => {
            let (title, detail) = describe_codex_app_server_request(method, params);
            recorder.push_codex_app_request(
                &title,
                &detail,
                method,
                params.clone(),
                CodexPendingAppRequest { request_id },
            )?;
        }
    }

    Ok(())
}

/// Formats the sidebar title + detail for a generic app-server request
/// (anything that is not a built-in approval/user-input/MCP flow).
/// `item/tool/call` gets a dedicated copy mentioning the tool and
/// server names; everything else gets a generic "needs a JSON result"
/// placeholder.
fn describe_codex_app_server_request(method: &str, params: &Value) -> (String, String) {
    if method == "item/tool/call" {
        let tool = params
            .get("tool")
            .or_else(|| params.get("toolName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("tool");
        let server = params
            .get("server")
            .or_else(|| params.get("serverName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let scope = server
            .map(|server_name| format!(" from `{server_name}`"))
            .unwrap_or_default();
        return (
            "Codex needs a tool result".to_owned(),
            format!(
                "Codex requested a result for `{tool}`{scope}. Review the request payload and submit the JSON result to continue."
            ),
        );
    }

    (
        "Codex needs a response".to_owned(),
        format!(
            "Codex sent an app-server request `{method}` that needs a JSON result before it can continue."
        ),
    )
}

/// Surfaces `item/started` events by type: agent messages finalize
/// any in-flight streaming text; command executions and web searches
/// register a running entry in the recorder so the UI can show a
/// spinner until `item/completed` arrives.
fn handle_codex_app_server_item_started(
    item: &Value,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            recorder.finish_streaming_text()?;
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                recorder.command_started(key, command)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            recorder.command_started(key, &command)?;
        }
        _ => {}
    }

    Ok(())
}

/// Formats a human-readable summary of the permission scopes Codex is
/// requesting (file-system read/write paths, network, macOS
/// accessibility/calendar/preferences/automations) for display in the
/// approval prompt sidebar.
fn describe_codex_permission_request(permissions: &Value) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(read_paths) = permissions
        .pointer("/fileSystem/read")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = read_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("read access to `{joined}`"));
        }
    }

    if let Some(write_paths) = permissions
        .pointer("/fileSystem/write")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = write_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("write access to `{joined}`"));
        }
    }

    if permissions
        .pointer("/network/enabled")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("network access".to_owned());
    }

    if permissions
        .pointer("/macos/accessibility")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS accessibility access".to_owned());
    }

    if permissions
        .pointer("/macos/calendar")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS calendar access".to_owned());
    }

    if let Some(preferences) = permissions
        .pointer("/macos/preferences")
        .and_then(Value::as_str)
        .filter(|value| *value != "none")
    {
        parts.push(format!("macOS preferences access ({preferences})"));
    }

    if let Some(automations) = permissions.pointer("/macos/automations") {
        if let Some(scope) = automations.as_str() {
            if scope == "all" {
                parts.push("macOS automation access".to_owned());
            }
        } else if let Some(bundle_ids) = automations.get("bundle_ids").and_then(Value::as_array) {
            let joined = bundle_ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if !joined.is_empty() {
                parts.push(format!("macOS automation access for `{joined}`"));
            }
        }
    }

    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Formats the sidebar detail for a user-input request: names the
/// single question's header when there is only one, otherwise reports
/// the count.
fn describe_codex_user_input_request(questions: &[UserInputQuestion]) -> String {
    match questions.len() {
        0 => "Codex requested additional input.".to_owned(),
        1 => {
            let question = &questions[0];
            format!(
                "Codex requested additional input for \"{}\".",
                question.header.trim()
            )
        }
        count => format!("Codex requested additional input for {count} questions."),
    }
}

/// Formats the sidebar detail for an MCP elicitation request — the
/// structured-form flow shows the server-provided message; the URL
/// flow instructs the user to continue in a browser and includes the
/// URL.
fn describe_codex_mcp_elicitation_request(request: &McpElicitationRequestPayload) -> String {
    match &request.mode {
        McpElicitationRequestMode::Form { message, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested additional structured input.",
                    request.server_name
                )
            } else {
                format!(
                    "MCP server {} requested additional structured input. {}",
                    request.server_name, trimmed
                )
            }
        }
        McpElicitationRequestMode::Url { message, url, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested that you continue in a browser: {}",
                    request.server_name, url
                )
            } else {
                format!(
                    "MCP server {} requested that you continue in a browser. {} {}",
                    request.server_name, trimmed, url
                )
            }
        }
    }
}

/// Surfaces `item/completed` events by type: agent messages are
/// reconciled against any streamed text via dedup; command executions
/// and web searches finalize their recorder entries with the exit
/// status; file changes are pushed as diff messages annotated with
/// create/edit change type. Other item types are intentionally
/// ignored.
fn handle_codex_app_server_item_completed(
    item: &Value,
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                record_completed_codex_agent_message(
                    turn_state, recorder, state, session_id, item_id, text,
                )?;
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

