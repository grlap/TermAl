// Terminal notifications and turn/start replies travel on independent channels.
// A candidate cannot establish the identity that authorizes its own effects.
const CODEX_DEFERRED_TERMINAL_LIMIT: usize = 4;
const CODEX_DEFERRED_TERMINAL_BYTES: usize = 64 * 1024;
const CODEX_RETIRED_TURN_LIMIT: usize = 32;

struct DeferredCodexTerminal {
    runtime: RuntimeToken,
    thread_id: Option<String>,
    request_id: String,
    generation: Option<u64>,
    turn_id: String,
    message: Value,
    bytes: usize,
}

const CODEX_DEFERRED_COMMAND_LIMIT: usize = 16;
const CODEX_DEFERRED_COMMAND_BYTES: usize = 512 * 1024;

struct DeferredCodexCommandCompletion {
    runtime: RuntimeToken,
    thread_id: String,
    request_id: String,
    generation: u64,
    turn_id: Option<String>,
    key: String,
    message: Value,
    bytes: usize,
}

fn shared_codex_command_item<'a>(method: &str, message: &'a Value) -> Option<&'a Value> {
    let (item, kind) = match method {
        "item/started" | "item/completed" => (message.pointer("/params/item")?, "commandExecution"),
        "codex/event/item_completed" => (message.pointer("/params/msg/item")?, "CommandExecution"),
        _ => return None,
    };
    (item.get("type").and_then(Value::as_str) == Some(kind)).then_some(item)
}

fn shared_codex_proven_command_turn(shared: &SharedCodexSessionState) -> bool {
    shared.turn_started
        && !shared.turn_id_provisional
        && !shared.terminal_identity_unresolved
        && (shared.turn_id.is_some()
            || (!shared.turn_started_before_response
                && shared.pending_turn_start_request_id.is_none()))
}

fn prepare_shared_codex_command_frame(
    method: &str,
    message: &Value,
    runtime: &RuntimeToken,
    shared: &mut SharedCodexSessionState,
) {
    let event_turn = shared_codex_event_turn_id(message);
    let thread = shared_codex_session_thread_id(method, message);
    let authenticated = thread.is_some() && thread == shared.thread_id.as_deref();
    let routed = shared.turn_started && event_turn == shared.turn_id.as_deref() && authenticated;
    let proven = routed && shared_codex_proven_command_turn(shared);
    let generation = shared.active_turn_generation;
    let native_item = matches!(
        method,
        "item/started" | "item/completed" | "codex/event/item_completed"
    );
    shared.recorder.native_tool_provenance =
        generation
            .filter(|_| proven && native_item)
            .map(|turn_generation| EngramObservationProvenance::Runtime {
                token: runtime.clone(),
                turn_generation,
            });
    shared.recorder.codex_command_start = None;
    shared.recorder.codex_command_completion = None;
    let Some(item) = shared_codex_command_item(method, message) else {
        return;
    };
    let digest = event_turn.map(|turn| sha256_hex(turn.as_bytes()));
    if method == "item/started" {
        shared.recorder.codex_command_start = Some(CodexCommandTurn { digest, proven });
        return;
    }
    let key = item.get("id").and_then(Value::as_str);
    shared.recorder.codex_command_completion =
        Some(if proven && generation.is_some() && key.is_some() {
            CodexCommandCompletion::Proven(digest)
        } else {
            CodexCommandCompletion::TranscriptOnly
        });
    // Keep the transcript on the normal path. Only a real terminal for the
    // pending handshake gets a bounded candidate for later settlement.
    if !routed
        || proven
        || !shared.turn_started_before_response
        || EngramHost::codex_command_exit(item) == EngramCommandExit::NotFinished
    {
        return;
    }
    let (Some(key), Some(thread), Some(request), Some(generation)) = (
        key,
        thread,
        shared.pending_turn_start_request_id.as_ref(),
        generation,
    ) else {
        return;
    };
    let pending = &mut shared.deferred_command_completions;
    if !shared.recorder.command_messages.contains_key(key)
        || pending.len() >= CODEX_DEFERRED_COMMAND_LIMIT
        || pending.iter().any(|candidate| candidate.key == key)
    {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(message).map(|serialized| serialized.len()) else {
        return;
    };
    if bytes
        + pending
            .iter()
            .map(|candidate| candidate.bytes)
            .sum::<usize>()
        > CODEX_DEFERRED_COMMAND_BYTES
    {
        return;
    }
    pending.push(DeferredCodexCommandCompletion {
        runtime: runtime.clone(),
        thread_id: thread.to_owned(),
        request_id: request.clone(),
        generation,
        turn_id: event_turn.map(str::to_owned),
        key: key.to_owned(),
        message: message.clone(),
        bytes,
    });
}

fn reconcile_shared_codex_command_turn(
    state: &AppState,
    session_id: &str,
    runtime: &RuntimeToken,
    request_id: &str,
    thread_sessions: &SharedCodexThreadMap,
    shared: &mut SharedCodexSessionState,
) -> Result<()> {
    if !shared_codex_proven_command_turn(shared) {
        return Ok(());
    }
    let Some(generation) = shared.active_turn_generation else {
        return Ok(());
    };
    let proven_digest = shared
        .turn_id
        .as_ref()
        .map(|turn| sha256_hex(turn.as_bytes()));
    state.confirm_codex_check_openings(session_id, runtime, generation, proven_digest.as_deref());
    let candidates = std::mem::take(&mut shared.deferred_command_completions);
    for candidate in candidates {
        if candidate.runtime != *runtime
            || candidate.generation != generation
            || candidate.request_id != request_id
            || Some(candidate.thread_id.as_str()) != shared.thread_id.as_deref()
        {
            continue;
        }
        if candidate.turn_id != shared.turn_id {
            let digest = candidate
                .turn_id
                .as_ref()
                .map(|turn| sha256_hex(turn.as_bytes()));
            state.reject_provisional_codex_completion(
                session_id,
                runtime,
                generation,
                &candidate.key,
                digest.as_deref(),
            );
            continue;
        }
        let method = candidate
            .message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("");
        shared.recorder.codex_replaying_command_completion = true;
        let applied = apply_owned_shared_codex_notification(
            method,
            &candidate.message,
            state,
            session_id,
            runtime,
            thread_sessions,
            shared,
        );
        shared.recorder.codex_replaying_command_completion = false;
        applied?;
    }
    Ok(())
}

fn shared_codex_error_is_retryable(payload: &Value) -> bool {
    let error = payload.get("error").unwrap_or(payload);
    is_retryable_connectivity_error(payload)
        || (is_typed_provider_capacity(error) && codex_error_will_retry(payload))
}

fn codex_terminal_notification(method: &str, message: &Value) -> bool {
    method == "turn/completed"
        || (method == "error"
            && !shared_codex_error_is_retryable(message.get("params").unwrap_or(message)))
}

fn codex_turn_is_retired(shared: &SharedCodexSessionState, turn_id: &str) -> bool {
    shared
        .retired_turn_ids
        .iter()
        .any(|(thread, id)| Some(thread.as_str()) == shared.thread_id.as_deref() && id == turn_id)
}

fn retire_codex_terminal_identity(shared: &mut SharedCodexSessionState, turn_id: &str) {
    let Some(thread_id) = shared.thread_id.clone() else {
        return;
    };
    if !codex_turn_is_retired(shared, turn_id) {
        shared
            .retired_turn_ids
            .push_back((thread_id, turn_id.to_owned()));
        if shared.retired_turn_ids.len() > CODEX_RETIRED_TURN_LIMIT {
            shared.retired_turn_ids.pop_front();
        }
    }
}

fn hold_unresolved_codex_terminal(
    state: &AppState,
    session_id: &str,
    runtime: &RuntimeToken,
    shared: &mut SharedCodexSessionState,
) -> Result<()> {
    shared.deferred_terminals.clear();
    shared.deferred_command_completions.clear();
    shared.terminal_identity_unresolved = true;
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let Some(index) = inner.find_session_index(session_id) else {
        return Ok(());
    };
    let record = &mut inner.sessions[index];
    if record.runtime.matches_runtime_token(runtime)
        && shared.active_turn_generation == Some(record.active_turn_generation)
        && !record.runtime_stop_in_progress
    {
        clear_provider_capacity_retry(record);
        record.codex_terminal_identity_unresolved = true;
        record.set_auto_dispatch_blocked(true);
        // The provider request remains unresolved; this is not a terminal
        // failure or an operator pause, and its accepted receipt stays accepted.
        record.session.status = SessionStatus::Active;
        record.session.preview =
            "Paused: Codex terminal identity unresolved; Stop before resuming".to_owned();
        inner.stamp_session_at_index(index);
        state.commit_locked(&mut inner)?;
    }
    Ok(())
}

/// Called under the shared-session lock, before generic event-id adoption.
fn codex_terminal_is_owned_or_deferred(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime: &RuntimeToken,
    shared: &mut SharedCodexSessionState,
) -> Result<bool> {
    if !codex_terminal_notification(method, message) {
        return Ok(true);
    }
    if shared.terminal_identity_unresolved {
        return Ok(false);
    }
    if !state.provider_capacity_event_is_current(session_id, shared.active_turn_generation) {
        return Ok(false);
    }
    let event_id = shared_codex_event_turn_id(message);
    if event_id.is_some_and(|id| codex_turn_is_retired(shared, id)) {
        return Ok(false);
    }
    if !shared.turn_id_provisional && shared.turn_id.is_some() {
        return Ok(event_id.is_none_or(|id| Some(id) == shared.turn_id.as_deref()));
    }
    if event_id.is_none() && shared.turn_started {
        return Ok(true); // Established fully id-less compatibility.
    }
    let Some(request_id) = shared.pending_turn_start_request_id.clone() else {
        // A real fully id-less handshake may already have reconciled a reply
        // without an id. An explicit terminal still cannot identify itself;
        // surface the unresolved attribution instead of silently stranding it.
        if shared.turn_started && shared.active_turn_generation.is_some() {
            hold_unresolved_codex_terminal(state, session_id, runtime, shared)?;
        }
        return Ok(false);
    };
    let Some(event_id) = event_id else {
        hold_unresolved_codex_terminal(state, session_id, runtime, shared)?;
        return Ok(false);
    };
    if shared
        .deferred_terminals
        .iter()
        .any(|candidate| candidate.message == *message)
    {
        return Ok(false);
    }
    let bytes = serde_json::to_vec(message)?.len();
    let conflict = shared.deferred_terminals.iter().any(|candidate| {
        candidate.turn_id == event_id
            && candidate.message.get("method") == message.get("method")
            && candidate.message != *message
    });
    if conflict
        || shared.deferred_terminals.len() >= CODEX_DEFERRED_TERMINAL_LIMIT
        || bytes
            + shared
                .deferred_terminals
                .iter()
                .map(|candidate| candidate.bytes)
                .sum::<usize>()
            > CODEX_DEFERRED_TERMINAL_BYTES
    {
        hold_unresolved_codex_terminal(state, session_id, runtime, shared)?;
        return Ok(false);
    }
    shared.deferred_terminals.push(DeferredCodexTerminal {
        runtime: runtime.clone(),
        thread_id: shared.thread_id.clone(),
        request_id,
        generation: shared.active_turn_generation,
        turn_id: event_id.to_owned(),
        message: message.clone(),
        bytes,
    });
    Ok(false)
}

/// Remove the handshake's candidates atomically before applying at most one.
/// The returned id is independent proof, never the candidate's own id.
fn take_proven_codex_terminal(
    shared: &mut SharedCodexSessionState,
    runtime: &RuntimeToken,
    request_id: &str,
    generation: Option<u64>,
    proven_id: Option<&str>,
) -> Option<Value> {
    let candidates = std::mem::take(&mut shared.deferred_terminals);
    if shared.terminal_identity_unresolved {
        return None;
    }
    candidates
        .into_iter()
        .find(|candidate| {
            candidate.runtime == *runtime
                && candidate.request_id == request_id
                && candidate.generation == generation
                && generation == shared.active_turn_generation
                && candidate.thread_id == shared.thread_id
                && proven_id == Some(candidate.turn_id.as_str())
                && !codex_turn_is_retired(shared, &candidate.turn_id)
        })
        .map(|candidate| candidate.message)
}

fn apply_owned_shared_codex_notification(
    method: &str,
    message: &Value,
    state: &AppState,
    session_id: &str,
    runtime: &RuntimeToken,
    thread_sessions: &SharedCodexThreadMap,
    shared: &mut SharedCodexSessionState,
) -> Result<Option<SharedCodexTerminalEffect>> {
    let terminal = codex_terminal_notification(method, message);
    let identity = terminal
        .then(|| SharedCodexTerminalIdentity::capture(state, session_id, runtime, shared))
        .flatten();
    if terminal {
        let id = shared_codex_event_turn_id(message)
            .map(str::to_owned)
            .or_else(|| shared.turn_id.clone());
        if let Some(id) = id {
            retire_codex_terminal_identity(shared, &id);
        }
    }
    prepare_shared_codex_command_frame(method, message, runtime, shared);
    let SharedCodexSessionState {
        pending_turn_start_request_id,
        turn_started_watchdog_cancel_tx,
        turn_started_before_response,
        turn_id_provisional,
        active_turn_generation,
        recorder,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        turn_state,
        ..
    } = shared;
    let mut recorder = BorrowedSessionRecorder::new(state, session_id, recorder);
    let mut effect = None;
    handle_shared_codex_app_server_notification(
        method,
        message,
        state,
        session_id,
        runtime,
        *active_turn_generation,
        thread_id,
        turn_id,
        completed_turn_id,
        turn_started,
        pending_turn_start_request_id,
        turn_started_watchdog_cancel_tx,
        turn_started_before_response,
        *turn_id_provisional,
        turn_state,
        thread_sessions,
        &mut recorder,
        &mut effect,
    )?;
    match (identity, effect) {
        (Some(owner), Some(outcome)) => Ok(Some(SharedCodexTerminalEffect { owner, outcome })),
        (None, Some(_)) => {
            eprintln!(
                "runtime state warning> refusing Codex terminal continuation for session `{session_id}`: runtime or turn owner changed"
            );
            Ok(None)
        }
        (_, None) => Ok(None),
    }
}
