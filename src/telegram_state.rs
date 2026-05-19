/*
Telegram relay persisted state and log redaction helpers.

Owns telegram-bot.json state shape, merge persistence, corrupt-file backup,
and token redaction used by runtime/client error paths.
*/

/// Tracks Telegram bot state.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TelegramBotState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_digest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_digest_message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_update_id: Option<i64>,
    /// Most recently forwarded assistant `Text` message id. Used by
    /// `forward_new_assistant_message_if_any` to dedupe full-content
    /// forwards: the digest poll runs every iteration, but the latest
    /// assistant message only needs to be delivered to Telegram once.
    /// Older state files that predate this field deserialize as
    /// `None`, which is correctly interpreted as "nothing forwarded
    /// yet" — the next sync will (re-)forward whatever the latest
    /// assistant message is at that moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_forwarded_assistant_message_id: Option<String>,
    /// Character count of the last forwarded assistant `Text`
    /// message. Paired with `last_forwarded_assistant_message_id`
    /// to detect the case where a forward landed mid-stream (the
    /// id stays stable while the text grows): on the next sync the
    /// same id is observed with a strictly-greater char count, and
    /// the relay re-forwards the now-settled text. Without this,
    /// the per-id dedupe would silently swallow the rest of any
    /// reply that started forwarding before the turn settled.
    /// Older state files that predate this field deserialize as
    /// `None` and the relay treats them as "unknown length", which
    /// triggers a one-time re-forward when the latest message is
    /// next observed (acceptable cost for self-healing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_forwarded_assistant_message_text_chars: Option<usize>,
    /// Session-keyed forwarding cursors. This is the authoritative cursor set.
    /// The two legacy fields above are read as a compatibility fallback for
    /// older state files, but new cursor writes stay session-scoped. On disk
    /// this is `assistantForwardingCursors: { "<session-id>": { messageId,
    /// textChars, resendIfGrown?, sentChunks?, failedChunkSendAttempts?,
    /// footerPending?, baselineWhileActive? } }`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    assistant_forwarding_cursors: HashMap<String, TelegramAssistantForwardingCursor>,
    /// Session ids whose next settled assistant reply should be forwarded even
    /// when no prior assistant text exists for that session. This ordered list
    /// is authoritative; the legacy singleton below mirrors the latest touched
    /// id for older state readers. On disk this is
    /// `forwardNextAssistantMessageSessionIds: ["<session-id>", ...]`, in the
    /// same order Telegram prompts armed their sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    forward_next_assistant_message_session_ids: Vec<String>,
    /// Session id whose next settled assistant text should be forwarded even
    /// when there was no previous assistant message to baseline against. Set
    /// immediately before accepting a Telegram-originated prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forward_next_assistant_message_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TelegramAssistantForwardingCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_start_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "is_false")]
    resend_if_grown: bool,
    /// Number of chunks already delivered for `message_id` when a long
    /// assistant message failed mid-message. `None` means the message is
    /// complete, not partially delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_chunks: Option<usize>,
    /// Failed delivery attempts for the next chunk after `sent_chunks`.
    /// This lets the relay bound retries for chunks Telegram repeatedly
    /// rejects before any visible content can be delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failed_chunk_send_attempts: Option<usize>,
    /// True when assistant content was delivered but the final footer marker
    /// failed. The next poll retries the footer without replaying content.
    #[serde(default, skip_serializing_if = "is_false")]
    footer_pending: bool,
    /// Holds the boundary for a Telegram prompt queued behind an already
    /// running or approval-paused local turn. Older binaries ignore this field;
    /// after a downgrade, one settled old-turn reply can be misattributed.
    #[serde(default, skip_serializing_if = "is_false")]
    baseline_while_active: bool,
}

impl TelegramAssistantForwardingCursor {
    fn from_latest(latest: Option<(String, usize)>, resend_if_grown: bool) -> Self {
        latest
            .map(|(message_id, text_chars)| Self {
                message_id: Some(message_id),
                text_chars: Some(text_chars),
                text_hash: None,
                text_start_chars: None,
                resend_if_grown,
                sent_chunks: None,
                failed_chunk_send_attempts: None,
                footer_pending: false,
                baseline_while_active: false,
            })
            .unwrap_or_default()
    }

    fn active_baseline(latest: Option<(String, usize)>) -> Self {
        Self {
            baseline_while_active: true,
            ..Self::from_latest(latest, false)
        }
    }

    fn is_empty(&self) -> bool {
        self.message_id.is_none()
            && self.text_chars.is_none()
            && self.text_hash.is_none()
            && self.text_start_chars.is_none()
            && !self.resend_if_grown
            && self.sent_chunks.is_none()
            && self.failed_chunk_send_attempts.is_none()
            && !self.footer_pending
            && !self.baseline_while_active
    }
}

/// Loads Telegram bot state.
fn load_telegram_bot_state(path: &FsPath) -> Result<TelegramBotState> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(TelegramBotState::default());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };
    match serde_json::from_slice(&raw) {
        Ok(state) => Ok(state),
        Err(err) => {
            backup_corrupt_telegram_bot_file(path, &err)?;
            Ok(TelegramBotState::default())
        }
    }
}

/// Persists Telegram bot state.
fn persist_telegram_bot_state(path: &FsPath, state: &TelegramBotState) -> Result<()> {
    let _guard = telegram_settings_file_guard();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let mut file = match fs::read(path) {
        Ok(raw) => match serde_json::from_slice::<TelegramBotFile>(&raw) {
            Ok(file) => file,
            Err(err) => {
                bail!(
                    "failed to parse existing Telegram bot file `{}` before merging relay state: {err}",
                    path.display()
                );
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => TelegramBotFile::default(),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };
    file.state = state.clone();

    let encoded =
        serde_json::to_vec_pretty(&file).context("failed to serialize telegram bot state")?;
    write_telegram_bot_file(path, &encoded)
        .with_context(|| format!("failed to write `{}`", path.display()))
}

fn persist_dirty_telegram_state_after_poll_error(
    path: &FsPath,
    state: &TelegramBotState,
    dirty: bool,
) -> bool {
    if !dirty {
        return true;
    }

    match persist_telegram_bot_state(path, state) {
        Ok(()) => true,
        Err(err) => {
            log_telegram_error("failed to persist Telegram state after poll error", &err);
            false
        }
    }
}

fn persist_telegram_update_cursor_after_update(path: &FsPath, state: &TelegramBotState) {
    if let Err(err) = persist_telegram_bot_state(path, state) {
        log_telegram_error("failed to persist Telegram update cursor", &err);
    }
}

fn backup_corrupt_telegram_bot_file(path: &FsPath, err: impl std::fmt::Display) -> Result<()> {
    let backup_path = corrupt_telegram_bot_file_backup_path(path);
    let err = sanitize_telegram_log_detail(&err.to_string());
    eprintln!(
        "telegram> failed to parse `{}`: {err}; moving corrupt file to `{}`",
        path.display(),
        backup_path.display()
    );
    backup_corrupt_telegram_bot_file_with_rename(path, &backup_path, |from, to| {
        fs::rename(from, to)
    })
}

fn backup_corrupt_telegram_bot_file_with_rename(
    path: &FsPath,
    backup_path: &FsPath,
    rename_fn: impl FnOnce(&FsPath, &FsPath) -> io::Result<()>,
) -> Result<()> {
    if let Err(err) = harden_telegram_bot_file_permissions(path) {
        eprintln!(
            "telegram> failed to pre-harden corrupt `{}` before quarantine: {}; continuing",
            path.display(),
            sanitize_telegram_log_detail(&err.to_string())
        );
    }
    match rename_fn(path, backup_path) {
        Ok(()) => {
            harden_telegram_backup_file_or_remove(backup_path)?;
            Ok(())
        }
        Err(rename_err) => {
            fs::copy(path, backup_path).with_context(|| {
                format!(
                    "failed to copy corrupt `{}` to `{}` after rename failed: {rename_err}",
                    path.display(),
                    backup_path.display()
                )
            })?;
            harden_telegram_backup_file_or_remove(backup_path)?;
            fs::remove_file(path)
                .with_context(|| format!("failed to remove corrupt `{}`", path.display()))?;
            Ok(())
        }
    }
}

fn harden_telegram_backup_file_or_remove(path: &FsPath) -> io::Result<()> {
    if let Err(err) = harden_telegram_bot_file_permissions(path) {
        if let Err(remove_err) = fs::remove_file(path) {
            eprintln!(
                "telegram> failed to remove non-hardened backup `{}`: {}",
                path.display(),
                sanitize_telegram_log_detail(&remove_err.to_string())
            );
        }
        return Err(err);
    }
    Ok(())
}

fn corrupt_telegram_bot_file_backup_path(path: &FsPath) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| FsPath::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("telegram-bot.json");
    parent.join(format!("{file_name}.corrupt-{}.json", Uuid::new_v4()))
}

/// Computes the effective Telegram chat ID.
fn effective_telegram_chat_id(config: &TelegramBotConfig, state: &TelegramBotState) -> Option<i64> {
    config.chat_id.or(state.chat_id)
}

fn telegram_project_is_subscribed(config: &TelegramBotConfig, project_id: &str) -> bool {
    config
        .subscribed_project_ids
        .iter()
        .any(|subscribed_project_id| subscribed_project_id == project_id)
        || config.project_id == project_id
}

fn telegram_active_project_id<'a>(
    config: &'a TelegramBotConfig,
    state: &'a TelegramBotState,
) -> &'a str {
    state
        .selected_project_id
        .as_deref()
        .filter(|project_id| telegram_project_is_subscribed(config, project_id))
        .unwrap_or(config.project_id.as_str())
}

fn resolve_telegram_active_project_id(
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
) -> (String, bool) {
    match state.selected_project_id.as_deref() {
        Some(project_id) if telegram_project_is_subscribed(config, project_id) => {
            (project_id.to_owned(), false)
        }
        Some(_) => {
            state.selected_project_id = None;
            clear_telegram_project_scoped_state(state);
            (config.project_id.clone(), true)
        }
        None => (config.project_id.clone(), false),
    }
}

fn clear_telegram_project_scoped_state(state: &mut TelegramBotState) -> bool {
    let mut dirty = false;
    if let Some(session_id) = state.selected_session_id.take() {
        dirty = true;
        dirty |= clear_forward_next_assistant_message_session_id(state, &session_id);
    }
    if state.last_digest_hash.take().is_some() {
        dirty = true;
    }
    if state.last_digest_message_id.take().is_some() {
        dirty = true;
    }
    dirty
}

fn log_telegram_error(context: &str, err: &anyhow::Error) {
    eprintln!(
        "telegram> {context}: {}",
        sanitize_telegram_log_detail(&err.to_string())
    );
}

fn sanitize_telegram_log_detail(detail: &str) -> String {
    const MAX_LOG_DETAIL_CHARS: usize = 256;
    let redacted = redact_telegram_bot_tokens(detail);
    let mut chars = redacted.chars();
    let truncated: String = chars.by_ref().take(MAX_LOG_DETAIL_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn redact_telegram_bot_tokens(detail: &str) -> String {
    let redacted_urls = redact_telegram_bot_url_tokens(detail);
    redact_standalone_telegram_bot_tokens(&redacted_urls)
}

fn redact_telegram_bot_url_tokens(detail: &str) -> String {
    let mut output = String::with_capacity(detail.len());
    let mut remainder = detail;
    while let Some(index) = remainder.find("/bot") {
        let (before, after_marker) = remainder.split_at(index + "/bot".len());
        output.push_str(before);
        let token_end = after_marker.find('/').unwrap_or(after_marker.len());
        if token_end == 0 {
            // `/bot/` is malformed for Telegram, but can appear in failed URL
            // logs. Advance by one byte so redaction cannot loop forever.
            if after_marker.is_empty() {
                remainder = after_marker;
                break;
            }
            output.push_str(&after_marker[..1]);
            remainder = &after_marker[1..];
            continue;
        }
        output.push_str("<redacted>");
        remainder = &after_marker[token_end..];
    }
    output.push_str(remainder);
    output
}

/// Redacts standalone Telegram-shaped tokens only when nearby context says the
/// value is likely a Telegram bot token. Unanchored digit/secret pairs, JSON
/// arrays without key names, free-prose mentions, code spans, and unrelated
/// token keys intentionally remain visible to avoid high-volume false positives
/// for benign `123456:abcdef...`-shaped values.
fn redact_standalone_telegram_bot_tokens(detail: &str) -> String {
    let bytes = detail.as_bytes();
    let mut output = String::with_capacity(detail.len());
    let mut last_copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() && (index == 0 || !bytes[index - 1].is_ascii_digit()) {
            if let Some(end) = standalone_telegram_bot_token_end(detail, index) {
                output.push_str(&detail[last_copied..index]);
                output.push_str("<redacted>");
                index = end;
                last_copied = end;
                continue;
            }
        }

        let ch = detail[index..]
            .chars()
            .next()
            .expect("index should stay within string bounds");
        index += ch.len_utf8();
    }
    output.push_str(&detail[last_copied..]);
    output
}

/// Returns the token end when a Telegram-shaped standalone token appears in a
/// known token-bearing context. Unanchored digit/secret pairs are intentionally
/// left visible to avoid redacting benign log fields like `12345:67890123`.
fn standalone_telegram_bot_token_end(detail: &str, start: usize) -> Option<usize> {
    const MIN_BOT_ID_DIGITS: usize = 6;
    const MIN_TOKEN_SECRET_CHARS: usize = 35;

    if !standalone_telegram_bot_token_has_context(detail, start) {
        return None;
    }

    let bytes = detail.as_bytes();
    if start > 0 && !telegram_token_boundary_byte(bytes[start - 1]) {
        return None;
    }

    let mut index = start;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index - start < MIN_BOT_ID_DIGITS || bytes.get(index) != Some(&b':') {
        return None;
    }

    index += 1;
    let secret_start = index;
    while index < bytes.len() && telegram_token_continuation_byte(bytes[index]) {
        index += 1;
    }
    if index - secret_start < MIN_TOKEN_SECRET_CHARS {
        return None;
    }

    if index < bytes.len() && !telegram_token_boundary_byte(bytes[index]) {
        return None;
    }

    Some(index)
}

/// Returns true when a standalone Telegram-shaped token is preceded by an
/// allowlisted Telegram token key, a bearer marker, or a generic `token` key
/// with nearby Telegram/bot word context. Extend the explicit key list for known
/// fields and the generic context gate for broader log shapes; unanchored tokens
/// are left visible to avoid redacting benign ids.
fn standalone_telegram_bot_token_has_context(detail: &str, start: usize) -> bool {
    let bytes = detail.as_bytes();
    let cursor = trim_telegram_token_quote_left(bytes, trim_ascii_whitespace_left(bytes, start));

    standalone_telegram_bot_token_has_key_context(detail, cursor)
        || standalone_telegram_bot_token_has_bearer_context(detail, cursor)
}

fn standalone_telegram_bot_token_has_key_context(detail: &str, cursor: usize) -> bool {
    let bytes = detail.as_bytes();
    if cursor == 0 || !matches!(bytes[cursor - 1], b'=' | b':') {
        return false;
    }

    let key_end =
        trim_telegram_token_quote_left(bytes, trim_ascii_whitespace_left(bytes, cursor - 1));

    let mut key_start = key_end;
    while key_start > 0 && telegram_token_key_byte(bytes[key_start - 1]) {
        key_start -= 1;
    }
    if key_start == key_end {
        return false;
    }

    let key = &detail[key_start..key_end];
    if key.eq_ignore_ascii_case("token") {
        return standalone_telegram_generic_token_has_context(detail, key_start);
    }

    // Authoritative key allowlist for standalone Telegram bot tokens.
    [
        "botToken",
        "bot_token",
        "bot-token",
        "telegramBotToken",
        "telegram_bot_token",
        "telegram-bot-token",
        "termal_telegram_bot_token",
    ]
    .iter()
    .any(|candidate| key.eq_ignore_ascii_case(candidate))
}

fn standalone_telegram_generic_token_has_context(detail: &str, key_start: usize) -> bool {
    const TOKEN_CONTEXT_WINDOW_BYTES: usize = 96;

    let context_start = key_start.saturating_sub(TOKEN_CONTEXT_WINDOW_BYTES);
    let context = &detail.as_bytes()[context_start..key_start];
    ascii_bytes_contains_word_ignore_case(context, b"telegram")
        || ascii_bytes_contains_word_ignore_case(context, b"bot")
}

fn ascii_bytes_contains_word_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }

    // This scans a capped context window (`TOKEN_CONTEXT_WINDOW_BYTES`, 96 bytes)
    // for two tiny ASCII needles, so the simple O(n*m) walk stays bounded.
    haystack
        .windows(needle.len())
        .enumerate()
        .any(|(index, candidate)| {
            let end_index = index + needle.len();
            let starts_on_boundary = ascii_word_boundary_between(
                index
                    .checked_sub(1)
                    .and_then(|index| haystack.get(index).copied()),
                haystack.get(index).copied(),
            );
            let ends_on_boundary = ascii_word_boundary_between(
                end_index
                    .checked_sub(1)
                    .and_then(|index| haystack.get(index).copied()),
                haystack.get(end_index).copied(),
            );

            starts_on_boundary
                && ends_on_boundary
                && candidate
                    .iter()
                    .zip(needle.iter())
                    .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

// Call sites pass the neighboring bytes around ASCII-alphanumeric words. A
// non-alphanumeric byte on either side is therefore a word boundary.
fn ascii_word_boundary_between(before: Option<u8>, after: Option<u8>) -> bool {
    match (before, after) {
        (None, _) | (_, None) => true,
        (Some(before), Some(after)) => {
            let separated = !before.is_ascii_alphanumeric() || !after.is_ascii_alphanumeric();
            // Treat conventional camelCase lower->upper transitions as word
            // boundaries. Upper->lower transitions stay joined so all-caps
            // prefixes do not turn values like `BOTanical` into a `bot`
            // context match.
            let camel_case_boundary = before.is_ascii_lowercase() && after.is_ascii_uppercase();
            separated || camel_case_boundary
        }
    }
}

fn standalone_telegram_bot_token_has_bearer_context(detail: &str, cursor: usize) -> bool {
    let bytes = detail.as_bytes();
    let cursor = if cursor > 0 && matches!(bytes[cursor - 1], b':' | b'=') {
        trim_ascii_whitespace_left(bytes, cursor - 1)
    } else {
        cursor
    };
    let mut word_start = cursor;
    while word_start > 0 && bytes[word_start - 1].is_ascii_alphabetic() {
        word_start -= 1;
    }
    let word = &detail[word_start..cursor];
    word.eq_ignore_ascii_case("bearer")
        && (word_start == 0 || !telegram_token_continuation_byte(bytes[word_start - 1]))
}

fn trim_telegram_token_quote_left(bytes: &[u8], mut end: usize) -> usize {
    if end > 0 && telegram_token_quote_byte(bytes[end - 1]) {
        end -= 1;
        if end > 0 && bytes[end - 1] == b'\\' {
            end -= 1;
        }
        trim_ascii_whitespace_left(bytes, end)
    } else {
        end
    }
}

fn trim_ascii_whitespace_left(bytes: &[u8], mut end: usize) -> usize {
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

fn telegram_token_quote_byte(byte: u8) -> bool {
    matches!(byte, b'"' | b'\'')
}

fn telegram_token_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn telegram_token_continuation_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Returns true for ASCII separators that may border a standalone token.
///
/// Non-ASCII bytes are deliberately not separators: otherwise token-shaped
/// substrings attached to non-ASCII words would be redacted as standalone bot
/// tokens.
fn telegram_token_boundary_byte(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || telegram_token_quote_byte(byte)
        || matches!(
            byte,
            b'\\' | b'=' | b':' | b',' | b'.' | b';' | b')' | b']' | b'}'
        )
}

fn telegram_prompt_exceeds_byte_limit(text: &str) -> bool {
    text.len() > MAX_DELEGATION_PROMPT_BYTES
}
