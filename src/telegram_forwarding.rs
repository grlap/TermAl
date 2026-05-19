/*
Telegram assistant-message forwarding state machine.

Tracks assistant text cursors, chunk retry state, footer delivery, and selected
session baselines for Telegram-originated prompts.
*/

/// Syncs Telegram digest.
fn sync_telegram_digest(
    telegram: &impl TelegramDigestMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let (project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
    let digest = termal.get_project_digest(&project_id)?;
    let digest_hash = telegram_digest_hash(&digest, config.public_base_url.as_deref())?;
    let (selected_session_id, selected_session_dirty) =
        resolve_telegram_selected_project_session(termal, &project_id, state)?;
    dirty |= selected_session_dirty;

    if state.last_digest_hash.as_deref() != Some(digest_hash.as_str()) {
        let message_id = edit_or_send_telegram_digest(
            telegram,
            config,
            chat_id,
            state.last_digest_message_id,
            &digest,
        )?;
        if remember_telegram_digest(
            state,
            &digest,
            config.public_base_url.as_deref(),
            message_id,
        )? {
            dirty = true;
        }
    }

    // Forward assistant text on every poll, not only when the compact digest
    // changes. The forwarder has its own id+char-count dedupe, so this catches
    // fresh replies whose truncated digest preview stayed byte-identical.
    dirty |= forward_relevant_assistant_messages(
        telegram,
        termal,
        state,
        chat_id,
        selected_session_id
            .as_deref()
            .or(digest.primary_session_id.as_deref()),
    );

    Ok(dirty)
}

fn resolve_telegram_selected_project_session(
    termal: &impl TelegramPromptClient,
    project_id: &str,
    state: &mut TelegramBotState,
) -> Result<(Option<String>, bool)> {
    let Some(session_id) = state.selected_session_id.clone() else {
        return Ok((None, false));
    };
    let sessions = termal.get_state_sessions()?;
    if find_telegram_project_session(&sessions, project_id, &session_id).is_some() {
        Ok((Some(session_id), false))
    } else {
        state.selected_session_id = None;
        clear_forward_next_assistant_message_session_id(state, &session_id);
        Ok((None, true))
    }
}

fn ensure_selected_session_forwarding_baseline(
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    session_id: &str,
) -> Result<bool> {
    if is_forward_next_assistant_message_session(state, session_id)
        || state.assistant_forwarding_cursors.contains_key(session_id)
    {
        return Ok(false);
    }
    let plan = prepare_assistant_forwarding_for_telegram_prompt(termal, session_id)?;
    Ok(apply_assistant_forwarding_plan(state, plan))
}

fn forward_relevant_assistant_messages(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    chat_id: i64,
    primary_session_id: Option<&str>,
) -> bool {
    let mut dirty = false;
    // Suppress digest-primary forwarding when an armed session either sent
    // visible content or hit a Telegram delivery failure in this poll.
    // Baseline-only state changes should still allow the primary digest
    // session to speak.
    let mut suppress_digest_primary = false;
    let mut checked_session_ids = BTreeSet::new();
    let armed_session_ids = forward_next_assistant_message_session_ids(state);

    for session_id in armed_session_ids {
        checked_session_ids.insert(session_id.clone());
        match forward_new_assistant_message_outcome(telegram, termal, state, chat_id, &session_id) {
            Ok(outcome) => {
                outcome.debug_assert_invariants();
                dirty |= outcome.dirty;
                suppress_digest_primary |= outcome.sent_visible_content || outcome.delivery_failed;
            }
            Err(err) => {
                dirty = true;
                log_telegram_error("failed to forward assistant message", &err);
            }
        }
    }

    if let Some(session_id) = primary_session_id
        .filter(|id| !suppress_digest_primary && !checked_session_ids.contains(*id))
    {
        merge_assistant_forward_result(
            &mut dirty,
            forward_new_assistant_message_if_any(telegram, termal, state, chat_id, session_id),
        );
    }

    dirty
}

fn merge_assistant_forward_result(dirty: &mut bool, result: Result<bool>) {
    match result {
        Ok(changed) => *dirty |= changed,
        Err(err) => {
            // `forward_new_assistant_message_if_any` records progress after each
            // successful message send. A later send can fail after mutating the
            // cursor, so persist the partial progress instead of replaying it.
            *dirty = true;
            log_telegram_error("failed to forward assistant message", &err);
        }
    }
}

fn latest_assistant_text_cursor(
    messages: &[TelegramSessionFetchMessage],
) -> Option<(String, usize)> {
    messages.iter().rev().find_map(|message| match message {
        TelegramSessionFetchMessage::Text { id, author, text } if author == "assistant" => {
            Some((id.clone(), text.chars().count()))
        }
        _ => None,
    })
}

fn telegram_assistant_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn resolve_assistant_forwarding_cursor(
    state: &TelegramBotState,
    session_id: &str,
    messages: &[TelegramSessionFetchMessage],
) -> TelegramAssistantForwardingCursor {
    if let Some(cursor) = state.assistant_forwarding_cursors.get(session_id) {
        return cursor.clone();
    }

    if let Some(legacy_id) = state.last_forwarded_assistant_message_id.as_deref() {
        if messages.iter().any(|message| {
            matches!(
                message,
                TelegramSessionFetchMessage::Text { id, author, .. }
                    if id == legacy_id && author == "assistant"
            )
        }) {
            return TelegramAssistantForwardingCursor {
                message_id: state.last_forwarded_assistant_message_id.clone(),
                text_chars: state.last_forwarded_assistant_message_text_chars,
                text_hash: None,
                text_start_chars: None,
                resend_if_grown: state.last_forwarded_assistant_message_text_chars.is_none(),
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            };
        }
    }

    TelegramAssistantForwardingCursor::default()
}

fn forward_next_assistant_message_session_ids(state: &TelegramBotState) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut session_ids = Vec::new();
    for session_id in &state.forward_next_assistant_message_session_ids {
        if seen.insert(session_id.clone()) {
            session_ids.push(session_id.clone());
        }
    }
    if let Some(session_id) = state.forward_next_assistant_message_session_id.as_ref() {
        if seen.insert(session_id.clone()) {
            session_ids.push(session_id.clone());
        }
    }
    for session_id in state
        .assistant_forwarding_cursors
        .iter()
        .filter_map(|(session_id, cursor)| cursor.footer_pending.then_some(session_id))
        .collect::<BTreeSet<_>>()
    {
        if seen.insert(session_id.to_owned()) {
            session_ids.push(session_id.to_owned());
        }
    }
    session_ids
}

fn is_forward_next_assistant_message_session(state: &TelegramBotState, session_id: &str) -> bool {
    state
        .forward_next_assistant_message_session_ids
        .iter()
        .any(|armed_session_id| armed_session_id == session_id)
        || state.forward_next_assistant_message_session_id.as_deref() == Some(session_id)
}

fn remember_assistant_forwarding_cursor(
    state: &mut TelegramBotState,
    session_id: &str,
    cursor: TelegramAssistantForwardingCursor,
) -> bool {
    let mut changed = false;

    if cursor.is_empty() {
        changed |= state
            .assistant_forwarding_cursors
            .remove(session_id)
            .is_some();
    } else if state.assistant_forwarding_cursors.get(session_id) != Some(&cursor) {
        state
            .assistant_forwarding_cursors
            .insert(session_id.to_owned(), cursor.clone());
        changed = true;
    }

    changed
}

fn remember_assistant_forwarding_footer_pending(
    state: &mut TelegramBotState,
    session_id: &str,
    pending: bool,
) -> bool {
    let mut cursor = state
        .assistant_forwarding_cursors
        .get(session_id)
        .cloned()
        .unwrap_or_default();
    cursor.footer_pending = pending;
    if !pending {
        cursor.failed_chunk_send_attempts = None;
    }
    remember_assistant_forwarding_cursor(state, session_id, cursor)
}

fn clear_forward_next_assistant_message_session_id(
    state: &mut TelegramBotState,
    session_id: &str,
) -> bool {
    let mut changed = state
        .forward_next_assistant_message_session_ids
        .iter()
        .any(|armed_session_id| armed_session_id == session_id);
    state
        .forward_next_assistant_message_session_ids
        .retain(|armed_session_id| armed_session_id != session_id);
    if state.forward_next_assistant_message_session_id.as_deref() == Some(session_id) {
        state.forward_next_assistant_message_session_id = state
            .forward_next_assistant_message_session_ids
            .first()
            .cloned();
        changed = true;
    }
    changed
}

fn arm_forward_next_assistant_message_session_id(
    state: &mut TelegramBotState,
    session_id: &str,
) -> bool {
    let inserted = if state
        .forward_next_assistant_message_session_ids
        .iter()
        .any(|armed_session_id| armed_session_id == session_id)
    {
        false
    } else {
        state
            .forward_next_assistant_message_session_ids
            .push(session_id.to_owned());
        true
    };
    let changed =
        inserted || state.forward_next_assistant_message_session_id.as_deref() != Some(session_id);
    state.forward_next_assistant_message_session_id = Some(session_id.to_owned());
    changed
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TelegramAssistantForwardingPlan {
    session_id: String,
    cursor: TelegramAssistantForwardingCursor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TelegramAssistantForwardingOutcome {
    dirty: bool,
    sent_visible_content: bool,
    delivery_failed: bool,
}

impl TelegramAssistantForwardingOutcome {
    fn debug_assert_invariants(&self) {
        debug_assert!(
            !self.sent_visible_content || self.dirty,
            "visible Telegram forwarding progress must be persisted"
        );
        debug_assert!(
            !self.delivery_failed || self.dirty,
            "Telegram delivery failures must force state persistence"
        );
    }
}

fn prepare_assistant_forwarding_for_telegram_prompt(
    termal: &impl TelegramSessionReader,
    session_id: &str,
) -> Result<TelegramAssistantForwardingPlan> {
    let response = termal.get_session(session_id)?;
    let latest = latest_assistant_text_cursor(&response.session.messages);
    let cursor = if response
        .session
        .status
        .keeps_telegram_prompt_boundary_open()
    {
        TelegramAssistantForwardingCursor::active_baseline(latest)
    } else {
        TelegramAssistantForwardingCursor::from_latest(latest, false)
    };
    Ok(TelegramAssistantForwardingPlan {
        session_id: session_id.to_owned(),
        cursor,
    })
}

fn apply_assistant_forwarding_plan(
    state: &mut TelegramBotState,
    plan: TelegramAssistantForwardingPlan,
) -> bool {
    apply_assistant_forwarding_baseline(state, &plan.session_id, plan.cursor)
}

fn apply_assistant_forwarding_baseline(
    state: &mut TelegramBotState,
    session_id: &str,
    cursor: TelegramAssistantForwardingCursor,
) -> bool {
    let mut changed = false;
    changed |= remember_assistant_forwarding_cursor(state, session_id, cursor);
    changed |= arm_forward_next_assistant_message_session_id(state, session_id);

    changed
}

#[cfg(test)]
fn arm_assistant_forwarding_for_telegram_prompt(
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    session_id: &str,
) -> Result<bool> {
    let plan = prepare_assistant_forwarding_for_telegram_prompt(termal, session_id)?;
    Ok(apply_assistant_forwarding_plan(state, plan))
}

/// Forwards every assistant `Text` message that has appeared since
/// the last `state.last_forwarded_assistant_message_id` (in
/// chronological order, chunked to Telegram's per-message length
/// limit). Returns `true` when state changed.
///
/// Why every-new-since rather than just the latest: a single agent
/// turn often emits multiple text messages — a "Reading the file…"
/// preamble, the actual content, sometimes a closing summary. If
/// the relay only forwarded the latest, anything that landed
/// before the final message would never reach Telegram (the user
/// would see the closing line but not the actual list/answer).
/// Walking from the last-forwarded id forward and dispatching each
/// message preserves the in-chat ordering and guarantees the user
/// sees what the agent actually said.
///
/// First-run / id-not-found behavior: when the relay starts up
/// fresh (or finds its previously-tracked message id no longer in
/// the session — e.g., the session was reset), it does NOT replay
/// the full transcript. It marks the latest assistant text message
/// as the baseline and only forwards what arrives AFTER that.
/// This avoids spamming Telegram with old history the user already
/// saw in TermAl.
///
/// Why the project digest sent to Telegram is a 3-4 line summary
/// (status / done preview / next-action labels) derived from the
/// session, not the full assistant content: the digest is meant
/// for status + control. For reads that only land in the bubble —
/// bug lists, code samples, design notes — the digest's preview
/// truncates after ~80 chars. Tool messages, command output, and
/// thinking blocks are still deliberately excluded from this
/// forward (Telegram is the wrong format for those); only `Text`
/// messages from `assistant` are forwarded.
///
/// Thin dirty-state wrapper for callers that only need persistence progress.
/// Use `forward_new_assistant_message_outcome` when visible-content forwarding
/// affects control flow, such as digest-primary suppression.
fn forward_new_assistant_message_if_any(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    chat_id: i64,
    session_id: &str,
) -> Result<bool> {
    let outcome =
        forward_new_assistant_message_outcome(telegram, termal, state, chat_id, session_id)?;
    outcome.debug_assert_invariants();
    Ok(outcome.dirty)
}

fn forward_new_assistant_message_outcome(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    chat_id: i64,
    session_id: &str,
) -> Result<TelegramAssistantForwardingOutcome> {
    let response = termal.get_session(session_id)?;
    let messages = &response.session.messages;

    let forward_without_existing_baseline =
        is_forward_next_assistant_message_session(state, session_id);
    let mut cursor = resolve_assistant_forwarding_cursor(state, session_id, messages);

    // Assistant forwarding has two active-session modes:
    //
    // - `baseline_while_active=true` means a Telegram prompt was queued behind
    //   an already-active local turn. Until the session settles, every
    //   assistant message may still belong to that older turn, so the cursor is
    //   only a moving baseline and no visible text is sent to Telegram.
    // - `baseline_while_active=false` on an armed session means the Telegram
    //   prompt started from a settled baseline. Active assistant text can be
    //   forwarded because any new message/growth is attributable to that
    //   Telegram-originated prompt.
    //
    // Cursor progress must preserve message id, delivered char count/hash,
    // partial chunk state, and footer state. Once settled, hash divergence
    // triggers a full resend and pure length growth sends only the suffix.
    if forward_without_existing_baseline
        && cursor.baseline_while_active
        && response
            .session
            .status
            .keeps_telegram_prompt_boundary_open()
    {
        let latest = latest_assistant_text_cursor(messages);
        return Ok(TelegramAssistantForwardingOutcome {
            dirty: remember_assistant_forwarding_cursor(
                state,
                session_id,
                TelegramAssistantForwardingCursor::active_baseline(latest),
            ),
            sent_visible_content: false,
            delivery_failed: false,
        });
    }

    let session_is_settled = response.session.status.can_forward_settled_assistant_text();
    let allow_active_telegram_reply =
        forward_without_existing_baseline && !cursor.baseline_while_active;
    if !session_is_settled && !allow_active_telegram_reply {
        return Ok(TelegramAssistantForwardingOutcome::default());
    }

    let mut sent_visible_content = false;
    let mut pre_forward_dirty = false;
    if cursor.footer_pending {
        if let Err(err) = telegram.send_message(
            chat_id,
            telegram_turn_settled_footer(&response.session.status),
            None,
        ) {
            log_telegram_error("failed to retry assistant message footer", &err);
            return Ok(TelegramAssistantForwardingOutcome {
                dirty: true,
                sent_visible_content: false,
                delivery_failed: true,
            });
        }
        sent_visible_content = true;
        pre_forward_dirty |= remember_assistant_forwarding_footer_pending(state, session_id, false);
    }

    let mut position_of_last = cursor.message_id.as_deref().and_then(|tracked| {
        messages.iter().position(|message| {
            matches!(
                message,
                TelegramSessionFetchMessage::Text { id, author, .. }
                    if id == tracked && author == "assistant"
            )
        })
    });

    if forward_without_existing_baseline && cursor.baseline_while_active {
        if let Some(pos) = position_of_last {
            // First settled poll after queuing behind a local turn has no
            // stronger turn-boundary signal. If the tracked same message grew
            // before this poll, record its current length as the baseline and
            // wait for later growth or a later assistant message. Forwarding
            // the already-present suffix here could leak the previous turn.
            let text_chars = match &messages[pos] {
                TelegramSessionFetchMessage::Text { text, .. } => Some(text.chars().count()),
                _ => None,
            };
            let settled_cursor = TelegramAssistantForwardingCursor {
                baseline_while_active: false,
                resend_if_grown: true,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                text_chars,
                text_hash: None,
                text_start_chars: None,
                ..cursor.clone()
            };
            let dirty =
                remember_assistant_forwarding_cursor(state, session_id, settled_cursor.clone());
            pre_forward_dirty |= dirty;
            cursor = settled_cursor;
            position_of_last = Some(pos);
            if dirty && pos + 1 == messages.len() {
                return Ok(TelegramAssistantForwardingOutcome {
                    dirty,
                    sent_visible_content: false,
                    delivery_failed: false,
                });
            }
        } else {
            let latest = latest_assistant_text_cursor(messages);
            return Ok(TelegramAssistantForwardingOutcome {
                dirty: remember_assistant_forwarding_cursor(
                    state,
                    session_id,
                    TelegramAssistantForwardingCursor::from_latest(latest, false),
                ),
                sent_visible_content: false,
                delivery_failed: false,
            });
        }
    }

    // Detect the "previously forwarded message has grown" case. Telegram
    // already received the prefix, so forward only the appended suffix instead
    // of replaying the full message and duplicating content in chat.
    let needs_resend_full = if session_is_settled && cursor.resend_if_grown {
        position_of_last.and_then(|pos| match &messages[pos] {
            TelegramSessionFetchMessage::Text { author, text, .. } if author == "assistant" => {
                let previous_chars = cursor.text_chars?;
                let previous_hash = cursor.text_hash.as_deref()?;
                let current_chars = text.chars().count();
                if current_chars < previous_chars {
                    return Some(pos);
                }
                let comparison_text = if current_chars == previous_chars {
                    text.clone()
                } else {
                    text.chars().take(previous_chars).collect()
                };
                (telegram_assistant_text_hash(&comparison_text) != previous_hash).then_some(pos)
            }
            _ => None,
        })
    } else {
        None
    };

    let needs_resend_truncated = if session_is_settled
        && cursor.resend_if_grown
        && needs_resend_full.is_none()
    {
        position_of_last.and_then(|pos| match &messages[pos] {
            TelegramSessionFetchMessage::Text { author, text, .. } if author == "assistant" => {
                let last_chars = cursor.text_chars;
                let current_chars = text.chars().count();
                match last_chars {
                    None => Some(pos),
                    Some(prev) if current_chars > prev => Some(pos),
                    _ => None,
                }
            }
            _ => None,
        })
    } else {
        None
    };

    // Decide where to start forwarding from. If we have no record
    // OR the recorded id has scrolled off the session (cleared
    // session, switched session, etc.), re-baseline against the
    // current latest assistant message instead of replaying old
    // content.
    let needs_baseline = match (cursor.message_id.as_deref(), position_of_last) {
        (_, None) if forward_without_existing_baseline => false,
        (None, _) => true,
        (Some(_), None) => true,
        (Some(_), Some(_)) => false,
    };
    if needs_baseline {
        let latest = latest_assistant_text_cursor(messages);
        let changed = remember_assistant_forwarding_cursor(
            state,
            session_id,
            TelegramAssistantForwardingCursor::from_latest(latest, false),
        );
        let cleared = clear_forward_next_assistant_message_session_id(state, session_id);
        return Ok(TelegramAssistantForwardingOutcome {
            dirty: changed || cleared,
            sent_visible_content: false,
            delivery_failed: false,
        });
    }

    let partial_message_position = cursor.sent_chunks.and_then(|_| position_of_last);

    // If the prior forward stopped mid-message, restart at that same message
    // and skip only the already-sent chunks below. If the prior forward was
    // truncated, restart at that message's index so it gets re-forwarded as
    // part of the batch. Otherwise start strictly after the last forwarded
    // message.
    let start_index = if let Some(pos) = partial_message_position {
        pos
    } else if let Some(pos) = needs_resend_full {
        pos
    } else if let Some(pos) = needs_resend_truncated {
        pos
    } else if forward_without_existing_baseline && position_of_last.is_none() {
        0
    } else {
        position_of_last.expect("position_of_last is Some when not baselining") + 1
    };
    let to_forward: Vec<(String, String, usize, String, Option<usize>)> = messages
        .iter()
        .enumerate()
        .skip(start_index)
        .filter_map(|(position, message)| match message {
            TelegramSessionFetchMessage::Text { id, author, text } if author == "assistant" => {
                let full_text_chars = text.chars().count();
                let retry_start_chars = if Some(position) == partial_message_position {
                    cursor.text_start_chars
                } else {
                    None
                };
                let suffix_start_chars = retry_start_chars.or_else(|| {
                    (Some(position) == needs_resend_truncated)
                        .then_some(cursor.text_chars)
                        .flatten()
                });
                let text_to_send = if Some(position) == needs_resend_full {
                    text.clone()
                } else if let Some(start_chars) = suffix_start_chars {
                    text.chars().skip(start_chars).collect()
                } else {
                    text.clone()
                };
                Some((
                    id.clone(),
                    text_to_send,
                    full_text_chars,
                    telegram_assistant_text_hash(text),
                    suffix_start_chars,
                ))
            }
            _ => None,
        })
        .collect();

    if to_forward.is_empty() {
        let cleared = if forward_without_existing_baseline {
            if response.session.status == TelegramSessionStatus::Approval
                || cursor.message_id.is_some()
            {
                // Once a pre-existing turn has been baselined to a concrete
                // assistant message, keep the arm so the next settled assistant
                // reply is forwarded as the Telegram-originated response.
                false
            } else {
                clear_forward_next_assistant_message_session_id(state, session_id)
            }
        } else {
            false
        };
        return Ok(TelegramAssistantForwardingOutcome {
            dirty: pre_forward_dirty || cleared,
            sent_visible_content,
            delivery_failed: false,
        });
    }

    sent_visible_content |= cursor
        .sent_chunks
        .is_some_and(|sent_chunks| sent_chunks > 0);
    let mut changed = pre_forward_dirty;
    let mut delivery_failed = false;
    for (id, text, full_text_chars, full_text_hash, text_start_chars) in &to_forward {
        let trimmed = text.trim();
        // Empty messages still bump the baseline so the next sync
        // doesn't keep re-checking them; they just don't produce a
        // Telegram send.
        if !trimmed.is_empty() {
            let chunks = chunk_telegram_message_text(trimmed);
            let text_chars = *full_text_chars;
            let resume_sent_chunks = if cursor.message_id.as_deref() == Some(id.as_str())
                && cursor.text_chars == Some(text_chars)
            {
                cursor.sent_chunks.unwrap_or(0).min(chunks.len())
            } else {
                0
            };
            for (chunk_index, chunk) in chunks.iter().enumerate().skip(resume_sent_chunks) {
                if let Err(err) = telegram.send_message(chat_id, chunk, None) {
                    log_telegram_error("failed to forward assistant message", &err);
                    delivery_failed = true;
                    let failed_attempts = if cursor.message_id.as_deref() == Some(id.as_str())
                        && cursor.text_chars == Some(text_chars)
                        && cursor.sent_chunks == Some(chunk_index)
                    {
                        cursor.failed_chunk_send_attempts.unwrap_or(0) + 1
                    } else {
                        1
                    };
                    let failed_cursor = TelegramAssistantForwardingCursor {
                        message_id: Some(id.clone()),
                        text_chars: Some(text_chars),
                        text_hash: Some(full_text_hash.clone()),
                        text_start_chars: *text_start_chars,
                        resend_if_grown: true,
                        sent_chunks: Some(chunk_index),
                        failed_chunk_send_attempts: Some(failed_attempts),
                        footer_pending: false,
                        baseline_while_active: false,
                    };
                    changed |= remember_assistant_forwarding_cursor(
                        state,
                        session_id,
                        failed_cursor.clone(),
                    );

                    if failed_attempts < TELEGRAM_ASSISTANT_CHUNK_SEND_FAILURE_LIMIT {
                        return Ok(TelegramAssistantForwardingOutcome {
                            dirty: true,
                            sent_visible_content,
                            delivery_failed: true,
                        });
                    }

                    let notice = telegram_assistant_chunk_skipped_notice(
                        chunk_index,
                        chunks.len(),
                        failed_attempts,
                    );
                    if let Err(err) = telegram.send_message(chat_id, &notice, None) {
                        log_telegram_error("failed to forward assistant chunk skip notice", &err);
                    } else {
                        sent_visible_content = true;
                    }
                    let sent_chunks = chunk_index + 1;
                    let skipped_cursor = TelegramAssistantForwardingCursor {
                        message_id: Some(id.clone()),
                        text_chars: Some(text_chars),
                        text_hash: Some(full_text_hash.clone()),
                        text_start_chars: *text_start_chars,
                        resend_if_grown: true,
                        sent_chunks: Some(sent_chunks),
                        failed_chunk_send_attempts: None,
                        footer_pending: false,
                        baseline_while_active: false,
                    };
                    changed |= remember_assistant_forwarding_cursor(
                        state,
                        session_id,
                        skipped_cursor.clone(),
                    );
                    cursor = skipped_cursor;
                    continue;
                }
                sent_visible_content = true;
                let sent_chunks = chunk_index + 1;
                if sent_chunks < chunks.len() {
                    let chunk_cursor = TelegramAssistantForwardingCursor {
                        message_id: Some(id.clone()),
                        text_chars: Some(text_chars),
                        text_hash: Some(full_text_hash.clone()),
                        text_start_chars: *text_start_chars,
                        resend_if_grown: true,
                        sent_chunks: Some(sent_chunks),
                        failed_chunk_send_attempts: None,
                        footer_pending: false,
                        baseline_while_active: false,
                    };
                    changed |= remember_assistant_forwarding_cursor(
                        state,
                        session_id,
                        chunk_cursor.clone(),
                    );
                    cursor = chunk_cursor;
                }
            }
        }
        // Record complete progress per-message so a mid-batch send failure
        // still preserves the messages that DID make it. The chunk loop above
        // records in-flight progress after each successful non-final chunk, so
        // retrying a long message resumes without duplicating delivered chunks.
        // Capture the char count alongside the id so a streaming-then-settled
        // re-send can be detected by length growth.
        let complete_cursor = TelegramAssistantForwardingCursor {
            message_id: Some(id.clone()),
            text_chars: Some(*full_text_chars),
            text_hash: Some(full_text_hash.clone()),
            text_start_chars: None,
            resend_if_grown: true,
            sent_chunks: None,
            failed_chunk_send_attempts: None,
            footer_pending: false,
            baseline_while_active: false,
        };
        changed |= remember_assistant_forwarding_cursor(state, session_id, complete_cursor.clone());
        cursor = complete_cursor;
        changed |= clear_forward_next_assistant_message_session_id(state, session_id);
    }

    // Footer separator: a short marker line that visually closes
    // the forwarded batch in the Telegram chat. Only emitted when
    // the batch actually had user-visible content, so a
    // forward-pass that was all empty/baselining doesn't send a
    // dangling separator. Without this line the user has no easy
    // way to tell "is the agent still typing or done?" while
    // scrolling — the digest message that carries the action
    // buttons already drifted up off-screen by the time the long
    // forwarded reply finishes rendering.
    //
    // The footer text varies by session status (settled label):
    // a generic "turn complete" would be misleading on the
    // `approval` and `error` settled-states, where the agent has
    // stopped but not because the work is done. See
    // `telegram_turn_settled_footer`.
    if sent_visible_content && session_is_settled {
        changed |= remember_assistant_forwarding_footer_pending(state, session_id, true);
        if let Err(err) = telegram.send_message(
            chat_id,
            telegram_turn_settled_footer(&response.session.status),
            None,
        ) {
            log_telegram_error("failed to forward assistant message footer", &err);
            return Ok(TelegramAssistantForwardingOutcome {
                dirty: true,
                sent_visible_content: true,
                delivery_failed: true,
            });
        }
        changed |= remember_assistant_forwarding_footer_pending(state, session_id, false);
    }

    Ok(TelegramAssistantForwardingOutcome {
        dirty: changed,
        sent_visible_content,
        delivery_failed,
    })
}

/// Returns the footer line shown after a settled assistant-message
/// forward batch, varying by session status so the wording matches
/// reality:
///
/// - `idle`     -> "✓ turn complete" (default success case)
/// - `approval` -> "⏸ approval needed" (agent is paused waiting on
///                  the user to approve a tool call; the digest
///                  message above carries the approve/reject
///                  buttons)
/// - `error`    -> "⚠ stopped on error" (agent hit a runtime error
///                  and bailed; the assistant text above usually
///                  contains the error detail)
/// - anything else (forward-compat with future session statuses
///   added to TermAl after the relay was last built) -> the
///   generic "turn complete" so the user still gets a closing
///   marker rather than nothing.
///
/// `active` and `unknown` are intentionally NOT handled here — the caller
/// gates on known settled statuses before invoking this function, so these
/// arms should be unreachable in practice; we map them to the same fallback
/// footer for safety.
fn telegram_turn_settled_footer(status: &TelegramSessionStatus) -> &'static str {
    match status {
        TelegramSessionStatus::Approval => "─────────── ⏸ approval needed ───────────",
        TelegramSessionStatus::Error => "─────────── ⚠ stopped on error ───────────",
        _ => "─────────── ✓ turn complete ───────────",
    }
}
