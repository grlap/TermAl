// Codex turn cleanup, stdout log capping, and stale-session error
// detection.
//
// Two categories of cleanup work, both sharing a common "after a
// turn finishes" theme:
//
// - **Per-turn scratch reset**: `clear_codex_turn_state` and
//   `clear_shared_codex_turn_recorder_state` wipe the streaming
//   text caches, subagent-result buffer, current-agent-message-id,
//   and assistant-output-started flag so the next turn starts
//   fresh.
// - **Deferred completed-turn bookkeeping**: Codex sometimes pushes
//   trailing events *after* it signals turn completion (token
//   counts, late notices). We can't drop the per-turn state
//   immediately or those late events would go astray. Instead,
//   `schedule_shared_codex_completed_turn_cleanup` records the turn
//   with a deadline; `spawn_shared_codex_completed_turn_cleanup_worker`
//   runs a dedicated thread that polls and
//   `run_due_shared_codex_completed_turn_cleanups` drains turns
//   whose deadline has passed. `clear_shared_codex_turn_session_state`
//   and `clear_shared_codex_completed_turn_state_fields` are the
//   actual clear routines invoked from the drain.
//
// Unrelated but co-located helpers:
//
// - `read_capped_child_stdout_line` + `truncate_child_stdout_log_line`
//   — read bytes from the Codex child's stdout with a hard cap so a
//   runaway line from Codex can't blow up memory. Log-only truncation
//   preserves start + end context.
// - `shared_codex_bad_json_streak_failure_detail` — formats the
//   error shown when Codex produces too many consecutive
//   non-parseable JSON lines (indicative of a protocol mismatch or
//   a crash-in-progress).
// - `shared_codex_app_server_error_is_stale_session` — pattern-
//   matches "session `X` not found" errors from the runtime so the
//   event dispatcher can silently drop them when a session has been
//   torn down mid-flight.


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
