/*
Telegram relay
Telegram Bot API <-> TermAl project digest/actions
Poll updates
  -> link chat / parse command / forward free text
  -> GET project digest or POST project action
  -> render digest + inline keyboard after updates have been drained
  -> persist chat binding and digest cursor
This adapter runs as a separate CLI mode. It reuses the same backend project
action contract instead of exposing a second transport-specific control path.
*/

/// Handles Telegram update.
#[derive(Clone, Copy, Debug, Default)]
struct TelegramUpdateHandlingOutcome {
    dirty: bool,
    final_sync_satisfied: bool,
}

impl TelegramUpdateHandlingOutcome {
    fn unsynced(dirty: bool) -> Self {
        Self {
            dirty,
            final_sync_satisfied: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TelegramPromptForwardOutcome {
    dirty: bool,
    final_sync_satisfied: bool,
}

fn telegram_prompt_exceeds_byte_limit(text: &str) -> bool {
    text.len() > MAX_DELEGATION_PROMPT_BYTES
}

fn handle_telegram_update(
    telegram: &impl TelegramCallbackResponder,
    termal: &(impl TelegramPromptClient + TelegramActionClient),
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    update: TelegramUpdate,
) -> Result<TelegramUpdateHandlingOutcome> {
    if let Some(callback_query) = update.callback_query {
        return handle_telegram_callback_query(telegram, termal, config, state, callback_query)
            .map(TelegramUpdateHandlingOutcome::unsynced);
    }
    if let Some(message) = update.message {
        return handle_telegram_message_for_relay(telegram, termal, config, state, message);
    }
    Ok(TelegramUpdateHandlingOutcome::default())
}

/// Handles Telegram message.
#[cfg(test)]
fn handle_telegram_message(
    telegram: &impl TelegramMessageSender,
    termal: &(impl TelegramPromptClient + TelegramActionClient),
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    message: TelegramChatMessage,
) -> Result<bool> {
    Ok(handle_telegram_message_for_relay(telegram, termal, config, state, message)?.dirty)
}

fn handle_telegram_message_for_relay(
    telegram: &impl TelegramMessageSender,
    termal: &(impl TelegramPromptClient + TelegramActionClient),
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    message: TelegramChatMessage,
) -> Result<TelegramUpdateHandlingOutcome> {
    let Some(text) = message
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return Ok(TelegramUpdateHandlingOutcome::default());
    };
    let chat_id = message.chat.id;

    if effective_telegram_chat_id(config, state).is_none() {
        if telegram_command_mentions_other_bot(text, config.bot_username.as_deref()) {
            return Ok(TelegramUpdateHandlingOutcome::default());
        }
        if matches!(
            parse_telegram_command_for_bot(text, config.bot_username.as_deref())
                .map(|command| command.command),
            Some(TelegramIncomingCommand::Start | TelegramIncomingCommand::Help)
        ) {
            state.chat_id = Some(chat_id);
            telegram.send_message(chat_id, &telegram_help_text(config, state), None)?;
            return Ok(TelegramUpdateHandlingOutcome::unsynced(true));
        }
        return Ok(TelegramUpdateHandlingOutcome::default());
    }

    if effective_telegram_chat_id(config, state) != Some(chat_id) {
        return Ok(TelegramUpdateHandlingOutcome::default());
    }

    if text.starts_with('/') {
        if telegram_command_mentions_other_bot(text, config.bot_username.as_deref()) {
            return Ok(TelegramUpdateHandlingOutcome::default());
        }
        let Some(command) = parse_telegram_command_for_bot(text, config.bot_username.as_deref())
        else {
            telegram.send_message(chat_id, &telegram_help_text(config, state), None)?;
            return Ok(TelegramUpdateHandlingOutcome::default());
        };

        return match command.command {
            TelegramIncomingCommand::Start | TelegramIncomingCommand::Help => {
                telegram.send_message(chat_id, &telegram_help_text(config, state), None)?;
                Ok(TelegramUpdateHandlingOutcome::default())
            }
            TelegramIncomingCommand::Status => {
                send_fresh_telegram_digest(telegram, termal, config, state, chat_id)
                    .map(TelegramUpdateHandlingOutcome::unsynced)
            }
            TelegramIncomingCommand::Projects => {
                send_telegram_projects(telegram, termal, config, state, chat_id)
                    .map(TelegramUpdateHandlingOutcome::unsynced)
            }
            TelegramIncomingCommand::Project => {
                select_telegram_project(telegram, termal, config, state, chat_id, command.args)
                    .map(TelegramUpdateHandlingOutcome::unsynced)
            }
            TelegramIncomingCommand::Sessions => {
                send_telegram_project_sessions(telegram, termal, config, state, chat_id)
                    .map(TelegramUpdateHandlingOutcome::unsynced)
            }
            TelegramIncomingCommand::Session => select_telegram_project_session(
                telegram,
                termal,
                config,
                state,
                chat_id,
                command.args,
            )
            .map(TelegramUpdateHandlingOutcome::unsynced),
            TelegramIncomingCommand::Action(action_id) => {
                let (project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
                match termal.dispatch_project_action(&project_id, action_id.as_str()) {
                    Ok(digest) => {
                        dirty |= send_fresh_telegram_digest_from_response(
                            telegram, config, state, chat_id, &digest,
                        )?;
                        Ok(TelegramUpdateHandlingOutcome::unsynced(dirty))
                    }
                    Err(err) => {
                        log_telegram_error("failed to dispatch Telegram action", &err);
                        telegram.send_message(
                            chat_id,
                            &telegram_action_error_text(action_id, &err),
                            None,
                        )?;
                        Ok(TelegramUpdateHandlingOutcome::unsynced(dirty))
                    }
                }
            }
        };
    }

    if telegram_prompt_exceeds_byte_limit(text) {
        telegram.send_message(
            chat_id,
            &format!(
                "That prompt is too large for TermAl. Keep Telegram prompts at or below {} bytes.",
                MAX_DELEGATION_PROMPT_BYTES
            ),
            None,
        )?;
        return Ok(TelegramUpdateHandlingOutcome::default());
    }

    match forward_telegram_text_to_project_for_relay(telegram, termal, config, state, chat_id, text)
    {
        Ok(outcome) => Ok(TelegramUpdateHandlingOutcome {
            dirty: outcome.dirty,
            final_sync_satisfied: outcome.final_sync_satisfied,
        }),
        Err(err) => {
            log_telegram_error("failed to forward Telegram prompt", &err);
            telegram.send_message(chat_id, &telegram_prompt_error_text(&err), None)?;
            Ok(TelegramUpdateHandlingOutcome::default())
        }
    }
}

/// Handles Telegram callback query.
fn handle_telegram_callback_query(
    telegram: &impl TelegramCallbackResponder,
    termal: &impl TelegramActionClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    callback_query: TelegramCallbackQuery,
) -> Result<bool> {
    let Some(message) = callback_query.message else {
        return Ok(false);
    };
    let chat_id = message.chat.id;
    if effective_telegram_chat_id(config, state) != Some(chat_id) {
        let _ = telegram.answer_callback_query(&callback_query.id, "This chat is not linked.");
        return Ok(false);
    }

    let Some(raw_callback_data) = callback_query
        .data
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        let _ = telegram.answer_callback_query(&callback_query.id, "That action is empty.");
        return Ok(false);
    };
    let Some((project_token, raw_action_id)) =
        parse_telegram_digest_callback_data(raw_callback_data)
    else {
        let text = if ProjectActionId::parse(raw_callback_data).is_ok() {
            "That action is from an older digest. Send /status to refresh."
        } else {
            "Unknown action."
        };
        let _ = telegram.answer_callback_query(&callback_query.id, text);
        return Ok(false);
    };
    let Some(project_id) = resolve_telegram_digest_callback_project(config, &project_token) else {
        let _ = telegram.answer_callback_query(
            &callback_query.id,
            "That project is no longer available to this relay.",
        );
        return Ok(false);
    };
    let action_id = match ProjectActionId::parse(&raw_action_id) {
        Ok(action_id) => action_id,
        Err(_) => {
            let _ = telegram.answer_callback_query(&callback_query.id, "Unknown action.");
            return Ok(false);
        }
    };
    let mut dirty = false;
    let digest = match termal.dispatch_project_action(&project_id, action_id.as_str()) {
        Ok(digest) => digest,
        Err(err) => {
            log_telegram_error("failed to dispatch Telegram callback action", &err);
            // Telegram requires callback queries to be answered promptly. Send
            // the toast first, then try the longer chat explanation; if the
            // chat send fails, the caller should still log that delivery error.
            let _ = telegram.answer_callback_query(
                &callback_query.id,
                &telegram_callback_action_error_text(action_id, &err),
            );
            telegram.send_message(chat_id, &telegram_action_error_text(action_id, &err), None)?;
            return Ok(dirty);
        }
    };
    let _ = telegram.answer_callback_query(&callback_query.id, action_id.label());
    if project_id == telegram_active_project_id(config, state) {
        dirty |= send_or_edit_telegram_digest_from_response(
            telegram,
            config,
            state,
            chat_id,
            Some(message.message_id),
            &digest,
        )?;
    } else {
        edit_telegram_digest_message(telegram, config, chat_id, message.message_id, &digest)?;
    }
    Ok(dirty)
}

/// Handles forward Telegram text to project.
#[cfg(test)]
fn forward_telegram_text_to_project(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    text: &str,
) -> Result<bool> {
    Ok(
        forward_telegram_text_to_project_for_relay(telegram, termal, config, state, chat_id, text)?
            .dirty,
    )
}

fn forward_telegram_text_to_project_for_relay(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    text: &str,
) -> Result<TelegramPromptForwardOutcome> {
    let (project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
    let digest = termal.get_project_digest(&project_id)?;
    let (selected_session_id, selected_session_dirty) =
        resolve_telegram_selected_project_session(termal, &project_id, state)?;
    dirty |= selected_session_dirty;
    if let Some(session_id) = selected_session_id.as_deref() {
        match ensure_selected_session_forwarding_baseline(termal, state, session_id) {
            Ok(changed) => dirty |= changed,
            Err(err) => log_telegram_error("failed to baseline selected Telegram session", &err),
        }
    }
    let session_id = selected_session_id
        .as_deref()
        .or(digest.primary_session_id.as_deref());
    let Some(session_id) = session_id else {
        telegram.send_message(
            chat_id,
            "No active project session is available yet. Start one in TermAl first.",
            None,
        )?;
        return Ok(TelegramPromptForwardOutcome {
            dirty,
            final_sync_satisfied: false,
        });
    };

    let pre_send_assistant_forwarding_plan =
        prepare_assistant_forwarding_for_telegram_prompt(termal, session_id)?;
    termal.send_session_message(session_id, text)?;
    let assistant_forwarding_baseline_changed =
        apply_assistant_forwarding_plan(state, pre_send_assistant_forwarding_plan);
    dirty |= assistant_forwarding_baseline_changed;
    let next_digest = match termal.get_project_digest(&project_id) {
        Ok(digest) => digest,
        Err(err) => {
            log_telegram_error("failed to refresh digest after Telegram prompt", &err);
            return Ok(TelegramPromptForwardOutcome {
                dirty,
                final_sync_satisfied: false,
            });
        }
    };
    match send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &next_digest) {
        Ok(changed) => dirty |= changed,
        Err(err) => {
            log_telegram_error("failed to send digest after Telegram prompt", &err);
            return Ok(TelegramPromptForwardOutcome {
                dirty,
                final_sync_satisfied: false,
            });
        }
    }
    // The agent's reply usually hasn't landed by the time this
    // immediate digest fetch fires (the agent is still working), so
    // this branch normally finds nothing to forward and the next
    // `sync_telegram_digest` poll iteration delivers the reply
    // instead. Calling it here is still useful: it covers the rare
    // case where the agent finishes synchronously, and it keeps the
    // forward-once contract centralized at the few places digests
    // are sent.
    dirty |=
        forward_relevant_assistant_messages(telegram, termal, state, chat_id, Some(session_id));
    Ok(TelegramPromptForwardOutcome {
        dirty,
        final_sync_satisfied: true,
    })
}
