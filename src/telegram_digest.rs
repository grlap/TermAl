/*
Telegram digest rendering, project/session selection, command parsing, and
user-facing error text.

This include! fragment owns the presentation layer for compact project digests
and slash-command responses.
*/

const TELEGRAM_DIGEST_FIELD_MAX_CHARS: usize = 120;

/// Handles send fresh Telegram digest.
fn send_fresh_telegram_digest(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let (project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
    let digest = termal.get_project_digest(&project_id)?;
    dirty |= send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &digest)?;
    Ok(dirty)
}

fn send_telegram_projects(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let (_, dirty) = resolve_telegram_active_project_id(config, state);
    let app_state = termal.get_state_sessions()?;
    let text = render_telegram_projects(config, state, &app_state);
    for chunk in chunk_telegram_message_text(&text) {
        telegram.send_message(chat_id, &chunk, None)?;
    }
    Ok(dirty)
}

fn send_telegram_project_sessions(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    bot_state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let (project_id, dirty) = resolve_telegram_active_project_id(config, bot_state);
    let state = termal.get_state_sessions()?;
    let text = render_telegram_project_sessions(&project_id, &state);
    for chunk in chunk_telegram_message_text(&text) {
        telegram.send_message(chat_id, &chunk, None)?;
    }
    Ok(dirty)
}

fn select_telegram_project(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    args: &str,
) -> Result<bool> {
    let (current_project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
    let mut parts = args.split_whitespace();
    let Some(raw_project_id) = parts.next() else {
        telegram.send_message(
            chat_id,
            &format!(
                "Current Telegram project: `{current_project_id}`.\nSend /project <project-id> to switch, /project clear to return to the default, or /projects to list ids."
            ),
            None,
        )?;
        return Ok(dirty);
    };
    if parts.next().is_some() {
        telegram.send_message(
            chat_id,
            "Use /project <project-id>, or /project clear to return to the default project.",
            None,
        )?;
        return Ok(dirty);
    }

    if matches!(raw_project_id, "clear" | "default" | "auto") {
        let previous_project_id = current_project_id;
        let selected_changed = state.selected_project_id.take().is_some();
        let active_changed = previous_project_id != config.project_id;
        if active_changed {
            dirty |= clear_telegram_project_scoped_state(state);
        }
        dirty |= selected_changed || active_changed;
        telegram.send_message(
            chat_id,
            &format!(
                "Telegram project target reset to the default project.\nid: {}\nSend /sessions to list sessions for it.",
                config.project_id
            ),
            None,
        )?;
        return Ok(dirty);
    }

    if !telegram_project_is_subscribed(config, raw_project_id) {
        telegram.send_message(
            chat_id,
            &format!(
                "Project `{raw_project_id}` is not subscribed to this Telegram relay. Send /projects to list available ids."
            ),
            None,
        )?;
        return Ok(dirty);
    }

    let app_state = termal.get_state_sessions()?;
    let Some(project) = find_telegram_project(&app_state, raw_project_id) else {
        telegram.send_message(
            chat_id,
            &format!(
                "I couldn't find project `{raw_project_id}` in TermAl. Send /projects to list available ids."
            ),
            None,
        )?;
        return Ok(dirty);
    };

    let previous_project_id = current_project_id;
    if raw_project_id == config.project_id {
        state.selected_project_id = None;
    } else {
        state.selected_project_id = Some(raw_project_id.to_owned());
    }
    if previous_project_id != raw_project_id {
        clear_telegram_project_scoped_state(state);
        dirty = true;
    }

    let label = telegram_project_label(project);
    telegram.send_message(
        chat_id,
        &format!(
            "Telegram project target set to {label}.\nid: {}\nSend /sessions to list sessions for it.",
            project.id
        ),
        None,
    )?;
    Ok(dirty)
}

fn select_telegram_project_session(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramPromptClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    args: &str,
) -> Result<bool> {
    let (project_id, mut dirty) = resolve_telegram_active_project_id(config, state);
    let raw_session_ref = args.trim();
    if raw_session_ref.is_empty() {
        let text = match state.selected_session_id.as_deref() {
            Some(session_id) => {
                let sessions = termal.get_state_sessions()?;
                let label = find_telegram_project_session(&sessions, &project_id, session_id)
                    .map(telegram_session_label)
                    .unwrap_or(session_id);
                format!(
                    "Current Telegram session target: {label}.\nSend /session <session name> to switch, /session clear to use the current project session, or /sessions to list sessions."
                )
            }
            None => "No Telegram session target is selected. Send /session <session name> to switch, or /sessions to list sessions.".to_owned(),
        };
        telegram.send_message(chat_id, &text, None)?;
        return Ok(dirty);
    };

    if matches!(raw_session_ref, "clear" | "default" | "auto") {
        if let Some(session_id) = state.selected_session_id.take() {
            dirty = true;
            dirty |= clear_forward_next_assistant_message_session_id(state, &session_id);
        }
        telegram.send_message(
            chat_id,
            "Telegram session target cleared. Free text will use the current project session.",
            None,
        )?;
        return Ok(dirty);
    }

    let sessions = termal.get_state_sessions()?;
    let Some(session) = find_telegram_project_session(&sessions, &project_id, raw_session_ref)
    else {
        telegram.send_message(
            chat_id,
            &format!(
                "I couldn't find session `{raw_session_ref}` in project `{}`. Send /sessions to list available sessions.",
                project_id
            ),
            None,
        )?;
        return Ok(dirty);
    };
    let changed = state.selected_session_id.as_deref() != Some(session.id.as_str());
    dirty |= changed;
    state.selected_session_id = Some(session.id.clone());
    match prepare_assistant_forwarding_for_telegram_prompt(termal, &session.id) {
        Ok(plan) => dirty |= apply_assistant_forwarding_plan(state, plan),
        Err(err) => log_telegram_error("failed to baseline selected Telegram session", &err),
    }
    let label = telegram_session_label(session);
    telegram.send_message(
        chat_id,
        &format!(
            "Telegram session target set to {label}.\nFree text will go to this session. Send /session clear to use the current project session."
        ),
        None,
    )?;
    Ok(dirty)
}

fn find_telegram_project_session<'a>(
    state: &'a TelegramStateSessionsResponse,
    project_id: &str,
    session_ref: &str,
) -> Option<&'a TelegramStateSession> {
    let project_sessions = state
        .sessions
        .iter()
        .filter(|session| telegram_session_is_project_root(session, project_id));
    project_sessions
        .clone()
        .find(|session| session.id == session_ref)
        .or_else(|| {
            let needle = session_ref.trim();
            let mut matches = project_sessions
                .filter(|session| telegram_session_label(session) == needle)
                .collect::<Vec<_>>();
            (matches.len() == 1).then(|| matches.remove(0))
        })
}

/// Handles send fresh Telegram digest from response.
fn send_fresh_telegram_digest_from_response(
    telegram: &impl TelegramMessageSender,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    digest: &ProjectDigestResponse,
) -> Result<bool> {
    let (text, format) = render_telegram_digest_message(digest, config.public_base_url.as_deref());
    let keyboard = build_telegram_digest_keyboard(digest)?;
    let sent = telegram.send_message_with_format(chat_id, &text, keyboard.as_ref(), format)?;
    remember_telegram_digest(
        state,
        digest,
        config.public_base_url.as_deref(),
        sent.message_id,
    )
}

/// Handles send or edit Telegram digest from response.
fn send_or_edit_telegram_digest_from_response(
    telegram: &impl TelegramDigestMessageSender,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    message_id: Option<i64>,
    digest: &ProjectDigestResponse,
) -> Result<bool> {
    let sent_message_id =
        edit_or_send_telegram_digest(telegram, config, chat_id, message_id, digest)?;
    remember_telegram_digest(
        state,
        digest,
        config.public_base_url.as_deref(),
        sent_message_id,
    )
}

/// Handles edit or send Telegram digest.
fn edit_or_send_telegram_digest(
    telegram: &impl TelegramDigestMessageSender,
    config: &TelegramBotConfig,
    chat_id: i64,
    message_id: Option<i64>,
    digest: &ProjectDigestResponse,
) -> Result<i64> {
    let (text, format) = render_telegram_digest_message(digest, config.public_base_url.as_deref());
    let keyboard = build_telegram_digest_keyboard(digest)?;
    if let Some(message_id) = message_id {
        match telegram.edit_message_with_format(
            chat_id,
            message_id,
            &text,
            keyboard.as_ref(),
            format,
        ) {
            Ok(message_id) => return Ok(message_id),
            Err(err) => {
                log_telegram_error("failed to edit digest message", &err);
            }
        }
    }

    let sent = telegram.send_message_with_format(chat_id, &text, keyboard.as_ref(), format)?;
    Ok(sent.message_id)
}

fn edit_telegram_digest_message(
    telegram: &impl TelegramDigestMessageSender,
    config: &TelegramBotConfig,
    chat_id: i64,
    message_id: i64,
    digest: &ProjectDigestResponse,
) -> Result<()> {
    let (text, format) = render_telegram_digest_message(digest, config.public_base_url.as_deref());
    let keyboard = build_telegram_digest_keyboard(digest)?;
    if let Err(err) =
        telegram.edit_message_with_format(chat_id, message_id, &text, keyboard.as_ref(), format)
    {
        log_telegram_error("failed to edit non-active project digest message", &err);
    }
    Ok(())
}

/// Remembers Telegram digest.
fn remember_telegram_digest(
    state: &mut TelegramBotState,
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
    message_id: i64,
) -> Result<bool> {
    let digest_hash = telegram_digest_hash(digest, public_base_url)?;
    let changed = state.last_digest_hash.as_deref() != Some(digest_hash.as_str())
        || state.last_digest_message_id != Some(message_id);
    state.last_digest_hash = Some(digest_hash);
    state.last_digest_message_id = Some(message_id);
    Ok(changed)
}

/// Handles Telegram digest hash.
fn telegram_digest_hash(
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
) -> Result<String> {
    let (text, format) = render_telegram_digest_message(digest, public_base_url);
    // Include the rendered format so changing Telegram presentation forces the
    // cached digest message to be edited once rather than leaving stale markup.
    let payload = json!({
        "format": format.parse_mode(),
        "callbackScheme": 1,
        "projectId": digest.project_id.as_str(),
        "text": text,
        "actions": digest
            .proposed_actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<Vec<_>>(),
    });
    Ok(stable_text_hash(
        &serde_json::to_string(&payload).context("failed to encode Telegram digest")?,
    ))
}

/// Builds Telegram digest keyboard.
fn build_telegram_digest_keyboard(
    digest: &ProjectDigestResponse,
) -> Result<Option<TelegramInlineKeyboardMarkup>> {
    if digest.proposed_actions.is_empty() {
        return Ok(None);
    }

    let mut rows = Vec::new();
    let mut current_row = Vec::new();
    for action in &digest.proposed_actions {
        current_row.push(TelegramInlineKeyboardButton {
            text: action.label.clone(),
            callback_data: telegram_digest_callback_data(&digest.project_id, &action.id)?,
        });
        if current_row.len() == 2 {
            rows.push(current_row);
            current_row = Vec::new();
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    Ok(Some(TelegramInlineKeyboardMarkup {
        inline_keyboard: rows,
    }))
}

fn telegram_digest_callback_data(project_id: &str, action_id: &str) -> Result<String> {
    let callback_data = format!(
        "p:{}:{action_id}",
        telegram_digest_project_token(project_id)
    );
    if callback_data.len() > TELEGRAM_CALLBACK_DATA_MAX_BYTES {
        bail!(
            "Telegram callback_data for action `{action_id}` exceeds {TELEGRAM_CALLBACK_DATA_MAX_BYTES} bytes"
        );
    }
    Ok(callback_data)
}

fn parse_telegram_digest_callback_data(value: &str) -> Option<(String, String)> {
    let payload = value.strip_prefix("p:")?;
    let (project_token, action_id) = payload.split_once(':')?;
    if project_token.len() != 16
        || !project_token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || action_id.is_empty()
    {
        return None;
    }
    Some((project_token.to_owned(), action_id.to_owned()))
}

fn telegram_digest_project_token(project_id: &str) -> String {
    stable_text_hash(project_id)
}

fn resolve_telegram_digest_callback_project(
    config: &TelegramBotConfig,
    project_token: &str,
) -> Option<String> {
    // The token is only a compact routing hint to stay under Telegram's
    // callback_data cap. It is not an auth secret; dispatch still requires
    // the project to be available in this relay's subscribed/default set.
    let mut matched_project_id = None;
    for project_id in config
        .subscribed_project_ids
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(config.project_id.as_str()))
    {
        if telegram_digest_project_token(project_id) != project_token {
            continue;
        }
        if matched_project_id.as_deref() == Some(project_id) {
            continue;
        }
        if matched_project_id.is_some() {
            log_telegram_error(
                "Telegram callback project token collision",
                &anyhow!("multiple subscribed projects matched token `{project_token}`"),
            );
            return None;
        }
        matched_project_id = Some(project_id.to_owned());
    }
    matched_project_id
}

fn render_telegram_digest_message(
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
) -> (String, TelegramTextFormat) {
    (
        render_telegram_digest_html(digest, public_base_url),
        TelegramTextFormat::Html,
    )
}

fn telegram_html_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn render_telegram_digest_html(
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
) -> String {
    let mut rows = vec![
        ("Project", telegram_digest_table_value(&digest.headline)),
        (
            "Status",
            telegram_digest_table_value(&digest.current_status),
        ),
        ("Done", telegram_digest_table_value(&digest.done_summary)),
    ];

    if !digest.proposed_actions.is_empty() {
        rows.push((
            "Next",
            telegram_digest_table_value(
                &digest
                    .proposed_actions
                    .iter()
                    .map(|action| action.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ));
    }

    let open_link = telegram_deep_link_url(digest, public_base_url).map(|url| {
        format!(
            "\n<a href=\"{}\">Open in TermAl</a>",
            telegram_html_escape(&url)
        )
    });

    format!(
        "<b>Project digest</b>\n<pre>{}</pre>{}",
        telegram_html_escape(&render_telegram_preformatted_table(&rows)),
        open_link.unwrap_or_default()
    )
}

fn telegram_digest_table_value(value: &str) -> String {
    let collapsed = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    truncate_telegram_text_chars(&collapsed, TELEGRAM_DIGEST_FIELD_MAX_CHARS)
}

fn truncate_telegram_text_chars(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

// Emits plain text only. Callers must HTML-escape the returned table before
// splicing it into a Telegram HTML message.
fn render_telegram_preformatted_table(rows: &[(&str, String)]) -> String {
    let label_width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .chain(std::iter::once("Field".chars().count()))
        .max()
        .unwrap_or(0);
    let mut lines = vec![
        format!("{:<label_width$}  Value", "Field"),
        format!("{:-<label_width$}  -----", ""),
    ];

    for (label, value) in rows {
        lines.push(format!("{label:<label_width$}  {value}"));
    }

    lines.join("\n")
}

/// Handles Telegram deep link URL.
fn telegram_deep_link_url(
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
) -> Option<String> {
    let deep_link = digest.deep_link.as_deref()?.trim();
    if deep_link.is_empty() {
        return None;
    }
    if deep_link.starts_with("http://") || deep_link.starts_with("https://") {
        return Some(deep_link.to_owned());
    }
    let base = public_base_url?.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}{deep_link}"))
}

fn render_telegram_projects(
    config: &TelegramBotConfig,
    bot_state: &TelegramBotState,
    state: &TelegramStateSessionsResponse,
) -> String {
    let active_project_id = telegram_active_project_id(config, bot_state);
    let mut lines = vec!["Telegram projects:".to_owned()];
    for project_id in &config.subscribed_project_ids {
        let project = find_telegram_project(state, project_id);
        let label = project
            .map(telegram_project_label)
            .unwrap_or_else(|| project_id.as_str());
        let session_count = state
            .sessions
            .iter()
            .filter(|session| session.project_id.as_deref() == Some(project_id.as_str()))
            .count();
        let sessions = match session_count {
            0 => "0 sessions".to_owned(),
            1 => "1 session".to_owned(),
            count => format!("{count} sessions"),
        };
        let marker = if project_id == active_project_id {
            "*"
        } else {
            "-"
        };
        lines.push(format!("{marker} {label} ({sessions})"));
        lines.push(format!("  id: {project_id}"));
    }
    lines.push("Send /project <project-id> to switch.".to_owned());
    lines.join("\n")
}

fn find_telegram_project<'a>(
    state: &'a TelegramStateSessionsResponse,
    project_id: &str,
) -> Option<&'a TelegramStateProject> {
    state
        .projects
        .iter()
        .find(|project| project.id == project_id)
}

fn telegram_project_label(project: &TelegramStateProject) -> &str {
    let name = project.name.trim();
    if name.is_empty() {
        project.id.as_str()
    } else {
        name
    }
}

fn telegram_session_label(session: &TelegramStateSession) -> &str {
    let name = session.name.trim();
    if name.is_empty() {
        session.id.as_str()
    } else {
        name
    }
}

fn telegram_session_is_project_root(session: &TelegramStateSession, project_id: &str) -> bool {
    session.project_id.as_deref() == Some(project_id) && session.parent_delegation_id.is_none()
}

fn telegram_session_status_sort_rank(status: &TelegramSessionStatus) -> u8 {
    match status {
        TelegramSessionStatus::Active | TelegramSessionStatus::Approval => 0,
        TelegramSessionStatus::Idle => 1,
        TelegramSessionStatus::Error | TelegramSessionStatus::Unknown => 2,
    }
}

fn render_telegram_project_sessions(
    project_id: &str,
    state: &TelegramStateSessionsResponse,
) -> String {
    let project_label = state
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .map(telegram_project_label)
        .unwrap_or(project_id);
    let mut sessions = state
        .sessions
        .iter()
        .filter(|session| telegram_session_is_project_root(session, project_id))
        .collect::<Vec<_>>();
    let has_more_sessions = sessions.len() > 12;
    sessions.sort_by(|left, right| {
        telegram_session_status_sort_rank(&left.status)
            .cmp(&telegram_session_status_sort_rank(&right.status))
            .then_with(|| {
                right
                    .session_mutation_stamp
                    .unwrap_or_default()
                    .cmp(&left.session_mutation_stamp.unwrap_or_default())
            })
            .then_with(|| right.message_count.cmp(&left.message_count))
            .then_with(|| telegram_session_label(left).cmp(telegram_session_label(right)))
            .then_with(|| left.id.cmp(&right.id))
    });

    if sessions.is_empty() {
        return format!(
            "No sessions are attached to project `{project_label}` yet. Start one in TermAl first."
        );
    }

    let mut lines = vec![format!("Sessions for {project_label}:")];
    for session in sessions.iter().take(12) {
        let status = telegram_state_session_status_label(&session.status);
        let message_count = match session.message_count {
            0 => "0 messages".to_owned(),
            1 => "1 message".to_owned(),
            count => format!("{count} messages"),
        };
        lines.push(format!(
            "- {} ({status}, {message_count})",
            telegram_session_label(session)
        ));
    }
    if has_more_sessions {
        lines.push("More sessions exist in TermAl.".to_owned());
    }
    lines.join("\n")
}

fn telegram_state_session_status_label(status: &TelegramSessionStatus) -> &'static str {
    match status {
        TelegramSessionStatus::Active => "active",
        TelegramSessionStatus::Idle => "idle",
        TelegramSessionStatus::Approval => "approval",
        TelegramSessionStatus::Error => "error",
        TelegramSessionStatus::Unknown => "unknown",
    }
}

/// Handles Telegram help text.
fn telegram_help_text(config: &TelegramBotConfig, state: &TelegramBotState) -> String {
    let active_project_id = telegram_active_project_id(config, state);
    [
        format!("TermAl Telegram relay for project `{active_project_id}`."),
        "Commands:".to_owned(),
        "/status, /projects, /project <id>, /sessions, /session <name>, /approve, /reject, /continue, /fix, /commit, /iterate, /stop, /review"
            .to_owned(),
        "Reply with free text to forward it into the selected or current project session."
            .to_owned(),
    ]
    .join("\n")
}

/// Formats a slash-command action failure sent as a normal chat message.
/// The message is multi-line and includes a `/status` hint because the user
/// can recover by refreshing the current action list.
fn telegram_action_error_text(action_id: ProjectActionId, err: &anyhow::Error) -> String {
    format!(
        "Could not run {}.\n{}\nSend /status to see the actions available right now.",
        action_id.label(),
        telegram_user_error_detail(err, TELEGRAM_USER_ERROR_MAX_CHARS)
    )
}

/// Formats an inline-button callback failure. Keep this one-line because
/// Telegram displays it near the tapped digest action and the digest already
/// carries the broader recovery context.
fn telegram_callback_action_error_text(action_id: ProjectActionId, err: &anyhow::Error) -> String {
    format!(
        "{} failed: {}",
        action_id.label(),
        telegram_user_error_detail(err, TELEGRAM_CALLBACK_ERROR_MAX_CHARS)
    )
}

/// Formats a free-text prompt forwarding failure. This is multi-line but has
/// no `/status` hint because the user's next useful action is usually to edit
/// and resend the prompt or select a different session.
fn telegram_prompt_error_text(err: &anyhow::Error) -> String {
    format!(
        "Could not forward that message.\n{}",
        telegram_user_error_detail(err, TELEGRAM_USER_ERROR_MAX_CHARS)
    )
}

fn telegram_user_error_detail(_err: &anyhow::Error, max_chars: usize) -> String {
    truncate_telegram_user_error_detail(TELEGRAM_SAFE_USER_ERROR_DETAIL, max_chars)
}

fn truncate_telegram_user_error_detail(detail: &str, max_chars: usize) -> String {
    let trimmed = detail.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }

    if max_chars < 3 {
        return trimmed.chars().take(max_chars).collect();
    }

    let mut truncated = trimmed
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

/// Represents a Telegram parsed command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TelegramParsedCommand<'a> {
    args: &'a str,
    command: TelegramIncomingCommand,
}

/// Represents a Telegram incoming command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TelegramIncomingCommand {
    Start,
    Help,
    Status,
    Projects,
    Project,
    Sessions,
    Session,
    Action(ProjectActionId),
}

/// Parses Telegram command.
#[cfg(test)]
fn parse_telegram_command(text: &str) -> Option<TelegramParsedCommand<'_>> {
    parse_telegram_command_for_bot(text, None)
}

fn parse_telegram_command_for_bot<'a>(
    text: &'a str,
    bot_username: Option<&str>,
) -> Option<TelegramParsedCommand<'a>> {
    let trimmed = text.trim();
    let command_text = trimmed.strip_prefix('/')?;
    let (raw_name, args) = match command_text.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (command_text, ""),
    };
    let (name, suffix) = match raw_name.split_once('@') {
        Some((name, suffix)) => (name.trim(), Some(suffix.trim())),
        None => (raw_name.trim(), None),
    };
    if let Some(suffix) = suffix {
        let expected = bot_username?;
        if !suffix.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    let command = match name {
        "start" => TelegramIncomingCommand::Start,
        "help" => TelegramIncomingCommand::Help,
        "status" => TelegramIncomingCommand::Status,
        "projects" => TelegramIncomingCommand::Projects,
        "project" => TelegramIncomingCommand::Project,
        "sessions" => TelegramIncomingCommand::Sessions,
        "session" => TelegramIncomingCommand::Session,
        "approve" => TelegramIncomingCommand::Action(ProjectActionId::Approve),
        "reject" => TelegramIncomingCommand::Action(ProjectActionId::Reject),
        "continue" => TelegramIncomingCommand::Action(ProjectActionId::Continue),
        "fix" | "fixit" => TelegramIncomingCommand::Action(ProjectActionId::FixIt),
        "commit" => TelegramIncomingCommand::Action(ProjectActionId::AskAgentToCommit),
        "iterate" | "keepiterating" => {
            TelegramIncomingCommand::Action(ProjectActionId::KeepIterating)
        }
        "stop" => TelegramIncomingCommand::Action(ProjectActionId::Stop),
        "review" => TelegramIncomingCommand::Action(ProjectActionId::ReviewInTermal),
        _ => return None,
    };

    Some(TelegramParsedCommand { args, command })
}

fn telegram_command_mentions_other_bot(text: &str, bot_username: Option<&str>) -> bool {
    let Some(expected) = bot_username
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let trimmed = text.trim();
    let Some(command_text) = trimmed.strip_prefix('/') else {
        return false;
    };
    let raw_name = command_text
        .split_once(char::is_whitespace)
        .map(|(name, _)| name)
        .unwrap_or(command_text)
        .trim();
    let Some((_, suffix)) = raw_name.split_once('@') else {
        return false;
    };
    let suffix = suffix.trim();
    !suffix.is_empty() && !suffix.eq_ignore_ascii_case(expected)
}
