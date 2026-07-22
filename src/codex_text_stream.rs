// Codex agent-message text stream assembly + subagent-result buffering.
//
// The shared Codex runtime streams assistant text to TermAl through
// the typed v2 `item/agentMessage/delta` notification followed by an
// authoritative `item/completed`. Typed deltas are ordered suffixes
// and must be concatenated verbatim for each item id; attempting to
// infer retransmissions from their content corrupts repetitive output
// such as Markdown tables.
//
// - `append_codex_agent_message_delta` — appends one typed delta
//   verbatim to the accumulated item text.
// - `next_completed_codex_text_update` — runs at `item/completed`
//   completion time: reconciles the final text against what we
//   already streamed, returning `NoChange` / `Append(suffix)` /
//   `Replace(full)` so the recorder performs the minimum work to
//   land on the canonical value. This is the cleanup that absorbs
//   any divergence between the stream and completed item.
// - `record_codex_agent_message_delta` — records a typed delta through
//   the recorder, finalizing any prior streaming text when the
//   item id changes.
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

/// Appends one typed v2 agent-message delta verbatim. Finalizes any
/// prior streaming text when the `item_id` changes.
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
    if !append_codex_agent_message_delta(entry, delta) {
        return Ok(());
    }

    begin_codex_assistant_output(turn_state, recorder)?;
    turn_state
        .streamed_agent_message_item_ids
        .insert(item_id.to_owned());
    recorder.text_delta(delta)?;
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

/// Appends one typed `item/agentMessage/delta` exactly as delivered.
/// The v2 protocol defines these as ordered incremental chunks, so
/// repeated text is content, not evidence of retransmission.
fn append_codex_agent_message_delta(existing: &mut String, delta: &str) -> bool {
    if delta.is_empty() {
        return false;
    }

    existing.push_str(delta);
    true
}
