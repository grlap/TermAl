// Codex agent-message text stream assembly + subagent-result buffering.
//
// The shared Codex runtime streams assistant text to TermAl as a
// series of `agentMessageContentDelta` events followed by a final
// `agentMessage` completion. Each delta carries a suffix of the full
// message-to-date; because Codex can retransmit overlapping chunks
// (especially around reconnects or batch flushes), naively appending
// every delta would produce duplicated text. The algorithms in this
// file perform the dedup and finalize pipeline:
//
// - `next_codex_delta_suffix` — given the accumulated text so far
//   and an incoming delta, return only the portion that is new.
//   Handles three unambiguous cases (exact-duplicate, cumulative
//   `incoming.starts_with(existing)`, suffix-of-existing
//   `existing.ends_with(incoming)`) and falls back to verbatim append
//   for everything else. See the function-level comment for why
//   partial-overlap detection is intentionally NOT applied — earlier
//   versions of this file included a `longest_codex_delta_overlap`
//   heuristic that proved unsound for repetitive content (Markdown
//   tables in particular) and is reconciled by the canonical
//   `agentMessage` event below instead.
// - `next_completed_codex_text_update` — runs at `agentMessage`
//   completion time: reconciles the final text against what we
//   already streamed, returning `NoChange` / `Append(suffix)` /
//   `Replace(full)` so the recorder performs the minimum work to
//   land on the canonical value. This is the cleanup that absorbs
//   any duplication produced by the verbatim-append fallback above.
// - `record_codex_agent_message_delta` — wraps the dedup into a
//   recorder append, finalizing any prior streaming text when the
//   item id changes.
// - `handle_shared_codex_event_agent_message_content_delta` /
//   `handle_shared_codex_event_agent_message` — top-level event
//   handlers called from the notification dispatcher in
//   `codex_events.rs`.
//
// Subagent-result buffering (`buffer_codex_subagent_result` +
// `flush_pending_codex_subagent_results`) exists because subagent
// results must appear *before* the final assistant reply in the UI,
// but arrive mid-turn before TermAl knows which reply will be the
// final one. Buffering + deferred flush preserves the ordering.
//
// `begin_codex_assistant_output` + `remember_codex_first_assistant_message_id`
// anchor the visible assistant message for the turn so the UI can
// scroll to it when it first appears.


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
/// text is already exactly seen, `Append` when the incoming value only
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

    existing.clear();
    existing.push_str(incoming);
    CompletedTextUpdate::Replace(incoming.to_owned())
}

/// Dedups a streaming delta: returns the portion of `incoming` not
/// already present in `existing`, advancing `existing` by the new
/// content.
///
/// Three unambiguous cases are handled exactly:
///   1. `incoming == existing` — pure exact retransmission.
///   2. `incoming.starts_with(existing)` — cumulative delta (the
///      agent re-sent the whole message-to-date with new bytes
///      appended). The agent-intent here is unambiguous: every byte
///      beyond `existing.len()` is new content.
///   3. `existing.ends_with(incoming)` — `incoming` is wholly a
///      suffix of what we already have (a small re-send of the tail).
///
/// All other shapes — including chunks that share a prefix with
/// `existing`'s suffix without `incoming.starts_with(existing)` — are
/// **appended verbatim**, even though that means a true partial-
/// overlap retransmission will duplicate the overlap region during
/// streaming. The reasoning:
///
///   - Without protocol-level metadata (sequence numbers, byte
///     offsets), we cannot reliably distinguish "agent re-sent the
///     last N bytes" from "agent's new chunk happens to start with
///     bytes coincidentally matching the existing tail". An overlap-
///     length heuristic is fundamentally unsound: repetitive content
///     (Markdown tables with `|`-bounded rows, fenced code blocks
///     with repeated indent prefixes, prose with repeated words like
///     `Backend prod` on consecutive rows) consistently produces
///     long coincidental matches that look identical to retransmissions.
///   - When the agent emits the final `agentMessage` event,
///     `next_completed_codex_text_update` reconciles streamed state
///     against the canonical text. Streamed duplication produced by
///     this function is wiped out and replaced (the divergence falls
///     through to the `Replace` branch) once the turn settles, and
///     reload-from-storage shows the canonical text.
///   - Duplication during streaming is a strictly less-bad failure
///     mode than dropping new characters: the user sees a few bytes
///     repeated, then a clean re-render at turn end. Dropped bytes
///     break GFM table recognition, fenced-code block boundaries,
///     and visibly corrupt the assistant's output for the entire
///     streaming window.
///
/// Earlier iterations of this function included a fourth branch that
/// applied `longest_codex_delta_overlap` to detect partial-overlap
/// retransmissions. That branch was the dominant source of streamed-
/// transcript corruption when the agent emitted Markdown tables and
/// other repetitive content, and is intentionally NOT reinstated.
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

    // Verbatim append. See the function-level comment above for why
    // partial-overlap detection is intentionally NOT applied here.
    existing.push_str(incoming);
    Some(incoming.to_owned())
}
