/*
Telegram relay
Telegram Bot API <-> telegram.rs <-> local TermAl project digest/actions
Poll updates
  -> link chat / parse command / forward free text
  -> GET project digest or POST project action
  -> render digest + inline keyboard
  -> persist chat binding and digest cursor
This adapter runs as a separate CLI mode. It reuses the same backend project
action contract instead of exposing a second transport-specific control path.
*/

const TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";
const TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS: u64 = 5;
const TELEGRAM_ERROR_RETRY_DELAY: Duration = Duration::from_secs(2);
const TELEGRAM_GET_UPDATES_LIMIT: i64 = 25;

/// Runs Telegram bot.
fn run_telegram_bot() -> Result<()> {
    let cwd_path = std::env::current_dir().context("failed to resolve current directory")?;
    let cwd = cwd_path
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();
    let mut config = TelegramBotConfig::from_env(&cwd)?;
    let termal = TermalApiClient::new(&config.api_base_url)?;
    let telegram = TelegramApiClient::new(&config.bot_token, config.poll_timeout_secs)?;
    let bot = telegram.get_me().map_err(|err| {
        anyhow!(
            "failed to resolve Telegram bot username: {}",
            sanitize_telegram_log_detail(&err.to_string())
        )
    })?;
    config.bot_username = Some(
        bot.username
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("Telegram getMe response did not include a bot username"))?,
    );
    let mut state = load_telegram_bot_state(&config.state_path)
        .context("failed to load Telegram bot state")?;
    let mut dirty = false;
    if let Some(chat_id) = config.chat_id {
        if state.chat_id != Some(chat_id) {
            state.chat_id = Some(chat_id);
            dirty = true;
        }
    }
    if dirty {
        persist_telegram_bot_state(&config.state_path, &state)?;
    }

    println!("TermAl Telegram adapter");
    println!("api: {}", config.api_base_url);
    println!("project: {}", config.project_id);
    match effective_telegram_chat_id(&config, &state) {
        Some(chat_id) => println!("chat: {chat_id}"),
        None => println!(
            "chat: not linked; set TERMAL_TELEGRAM_CHAT_ID or use the Settings link flow when it is enabled"
        ),
    }

    loop {
        let updates = match telegram.get_updates(state.next_update_id, config.poll_timeout_secs) {
            Ok(updates) => updates,
            Err(err) => {
                log_telegram_error("failed to poll updates", &err);
                std::thread::sleep(TELEGRAM_ERROR_RETRY_DELAY);
                continue;
            }
        };

        let mut dirty = false;
        for update in updates {
            let next_update_id = update.update_id.saturating_add(1);
            if state.next_update_id != Some(next_update_id) {
                state.next_update_id = Some(next_update_id);
                dirty = true;
            }

            match handle_telegram_update(&telegram, &termal, &config, &mut state, update) {
                Ok(changed) => dirty |= changed,
                Err(err) => log_telegram_error("failed to handle update", &err),
            }
        }

        if let Some(chat_id) = effective_telegram_chat_id(&config, &state) {
            match sync_telegram_digest(&telegram, &termal, &config, &mut state, chat_id) {
                Ok(changed) => dirty |= changed,
                Err(err) => log_telegram_error("failed to sync digest", &err),
            }
        }

        if dirty {
            persist_telegram_bot_state(&config.state_path, &state)?;
        }
    }
}

/// Holds Telegram bot configuration.
#[derive(Clone)]
struct TelegramBotConfig {
    api_base_url: String,
    bot_username: Option<String>,
    bot_token: String,
    chat_id: Option<i64>,
    poll_timeout_secs: u64,
    project_id: String,
    public_base_url: Option<String>,
    state_path: PathBuf,
}

impl TelegramBotConfig {
    /// Builds the value from environment.
    fn from_env(default_workdir: &str) -> Result<Self> {
        let bot_token = required_env_var("TERMAL_TELEGRAM_BOT_TOKEN")?;
        let project_id = required_env_var("TERMAL_TELEGRAM_PROJECT_ID")?;
        let api_base_url = std::env::var("TERMAL_TELEGRAM_API_BASE_URL")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(default_termal_api_base_url);
        let public_base_url = std::env::var("TERMAL_TELEGRAM_PUBLIC_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());
        let chat_id = parse_optional_i64_env("TERMAL_TELEGRAM_CHAT_ID")?;
        let poll_timeout_secs = std::env::var("TERMAL_TELEGRAM_POLL_TIMEOUT_SECS")
            .ok()
            .map(|value| {
                value.parse::<u64>().with_context(|| {
                    format!("TERMAL_TELEGRAM_POLL_TIMEOUT_SECS is not a valid integer: {value}")
                })
            })
            .transpose()?
            .unwrap_or(TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS)
            .max(1);
        let state_path = resolve_termal_data_dir(default_workdir).join("telegram-bot.json");

        Ok(Self {
            api_base_url,
            bot_username: None,
            bot_token,
            chat_id,
            poll_timeout_secs,
            project_id,
            public_base_url,
            state_path,
        })
    }
}

/// Tracks Telegram bot state.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TelegramBotState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_id: Option<i64>,
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
    /// Session id whose next settled assistant text should be forwarded even
    /// when there was no previous assistant message to baseline against. Set
    /// immediately before accepting a Telegram-originated prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forward_next_assistant_message_session_id: Option<String>,
}

/// Loads Telegram bot state.
fn load_telegram_bot_state(path: &FsPath) -> Result<TelegramBotState> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(TelegramBotState::default());
        }
        Err(err) => return Err(err).with_context(|| format!("failed to read `{}`", path.display())),
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
                backup_corrupt_telegram_bot_file(path, &err)?;
                TelegramBotFile::default()
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => TelegramBotFile::default(),
        Err(err) => return Err(err).with_context(|| format!("failed to read `{}`", path.display())),
    };
    file.state = state.clone();

    let encoded =
        serde_json::to_vec_pretty(&file).context("failed to serialize telegram bot state")?;
    write_telegram_bot_file(path, &encoded)
        .with_context(|| format!("failed to write `{}`", path.display()))
}

fn backup_corrupt_telegram_bot_file(path: &FsPath, err: impl std::fmt::Display) -> Result<()> {
    let backup_path = corrupt_telegram_bot_file_backup_path(path);
    let err = sanitize_telegram_log_detail(&err.to_string());
    eprintln!(
        "telegram> failed to parse `{}`: {err}; moving corrupt file to `{}`",
        path.display(),
        backup_path.display()
    );
    if let Err(err) = harden_telegram_bot_file_permissions(path) {
        eprintln!(
            "telegram> failed to pre-harden corrupt `{}` before quarantine: {}; continuing",
            path.display(),
            sanitize_telegram_log_detail(&err.to_string())
        );
    }
    match fs::rename(path, &backup_path) {
        Ok(()) => {
            harden_telegram_backup_file_or_remove(&backup_path)?;
            Ok(())
        }
        Err(rename_err) => {
            fs::copy(path, &backup_path).with_context(|| {
                format!(
                    "failed to copy corrupt `{}` to `{}` after rename failed: {rename_err}",
                    path.display(),
                    backup_path.display()
                )
            })?;
            harden_telegram_backup_file_or_remove(&backup_path)?;
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

fn log_telegram_error(context: &str, err: &anyhow::Error) {
    eprintln!("telegram> {context}: {}", sanitize_telegram_log_detail(&err.to_string()));
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
    if start > 0 && telegram_token_boundary_byte(bytes[start - 1]) {
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
    while index < bytes.len() && telegram_token_secret_byte(bytes[index]) {
        index += 1;
    }
    if index - secret_start < MIN_TOKEN_SECRET_CHARS {
        return None;
    }

    if index < bytes.len() && telegram_token_boundary_byte(bytes[index]) {
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

    let key_end = trim_telegram_token_quote_left(bytes, trim_ascii_whitespace_left(bytes, cursor - 1));

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

    haystack
        .windows(needle.len())
        .enumerate()
        .any(|(index, candidate)| {
            ascii_word_boundary_at(haystack, index)
                && ascii_word_boundary_after(haystack, index + needle.len())
                && candidate
                    .iter()
                    .zip(needle.iter())
                    .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

fn ascii_word_boundary_at(value: &[u8], index: usize) -> bool {
    value
        .get(index.wrapping_sub(1))
        .is_none_or(|byte| !telegram_token_key_byte(*byte))
        || index == 0
}

fn ascii_word_boundary_after(value: &[u8], index: usize) -> bool {
    value
        .get(index)
        .is_none_or(|byte| !telegram_token_key_byte(*byte))
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
        && (word_start == 0 || !telegram_token_boundary_byte(bytes[word_start - 1]))
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

fn telegram_token_boundary_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn telegram_token_secret_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn telegram_prompt_exceeds_byte_limit(text: &str) -> bool {
    text.len() > MAX_DELEGATION_PROMPT_BYTES
}

#[derive(Debug)]
struct TelegramApiError {
    method: String,
    status: StatusCode,
    error_code: Option<i64>,
    description: String,
}

impl std::fmt::Display for TelegramApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Telegram `{}` failed", self.method)?;
        if let Some(error_code) = self.error_code {
            write!(f, " with API code {error_code}")?;
        } else {
            write!(f, " with HTTP {}", self.status.as_u16())?;
        }
        write!(f, ": {}", self.description)
    }
}

impl std::error::Error for TelegramApiError {}

/// Returns `true` when an `editMessageText` error is Telegram's
/// "message is not modified" rejection — a benign no-op response we
/// see whenever the digest re-sync loop tries to edit a message whose
/// text + reply_markup are byte-identical to what the chat already
/// shows.
fn telegram_error_is_message_not_modified(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<TelegramApiError>()
            .is_some_and(|error| {
                error.error_code == Some(400)
                    && error.description.contains("message is not modified")
            })
    })
}

/// Represents Telegram API client.
struct TelegramApiClient {
    api_base_url: String,
    client: BlockingHttpClient,
}

impl TelegramApiClient {
    /// Creates a new instance.
    fn new(bot_token: &str, poll_timeout_secs: u64) -> Result<Self> {
        let client = BlockingHttpClient::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(poll_timeout_secs.saturating_add(10)))
            .build()
            .context("failed to build Telegram HTTP client")?;
        Ok(Self {
            api_base_url: format!("{TELEGRAM_API_BASE_URL}/bot{bot_token}"),
            client,
        })
    }

    /// Gets updates.
    fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
    ) -> Result<Vec<TelegramUpdate>> {
        let mut body = serde_json::Map::new();
        body.insert("timeout".to_owned(), json!(timeout_secs));
        body.insert("limit".to_owned(), json!(TELEGRAM_GET_UPDATES_LIMIT));
        body.insert(
            "allowed_updates".to_owned(),
            json!(["message", "callback_query"]),
        );
        if let Some(offset) = offset {
            body.insert("offset".to_owned(), json!(offset));
        }

        self.request_json("getUpdates", Some(Value::Object(body)))
    }

    fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramChatMessage> {
        // Telegram rejects `reply_markup: null` with
        // `Bad Request: object expected as reply markup` — the field
        // must either contain a markup object or be omitted entirely.
        // Build the body so absent markup is dropped instead of
        // serialized as JSON null.
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(keyboard) = reply_markup {
            body["reply_markup"] = serde_json::to_value(keyboard)
                .context("failed to serialize Telegram reply_markup")?;
        }
        self.request_json("sendMessage", Some(body))
    }

    fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<i64> {
        // Same `reply_markup` omission discipline as `send_message`
        // above — Telegram rejects an explicit JSON null.
        let mut body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Some(keyboard) = reply_markup {
            body["reply_markup"] = serde_json::to_value(keyboard)
                .context("failed to serialize Telegram reply_markup")?;
        }

        let outcome: Result<Value> = self.request_json("editMessageText", Some(body));
        match outcome {
            Ok(result) => Ok(result
                .get("message_id")
                .and_then(Value::as_i64)
                .unwrap_or(message_id)),
            // Telegram returns "Bad Request: message is not modified..."
            // when an edit's text + reply_markup are byte-identical to
            // what the chat already shows. That happens routinely
            // during the digest re-sync loop (the project state has
            // not changed since the last edit) and is harmless — the
            // existing message already reflects the desired content.
            // Treat it as a successful no-op so the caller doesn't
            // fall back to sending a duplicate message.
            Err(err) if telegram_error_is_message_not_modified(&err) => Ok(message_id),
            Err(err) => Err(err),
        }
    }

    fn answer_callback_query(&self, callback_query_id: &str, text: &str) -> Result<()> {
        let _: bool = self.request_json(
            "answerCallbackQuery",
            Some(json!({
                "callback_query_id": callback_query_id,
                "text": text,
                "show_alert": false,
            })),
        )?;
        Ok(())
    }

    fn get_me(&self) -> Result<TelegramBotUser> {
        self.request_json("getMe", None)
    }

    fn request_json<T: DeserializeOwned>(&self, method: &str, body: Option<Value>) -> Result<T> {
        let url = format!("{}/{}", self.api_base_url, method);
        let request = match body {
            Some(body) => self.client.post(&url).json(&body),
            None => self.client.post(&url),
        };
        let response = request
            .send()
            .with_context(|| format!("failed to call Telegram `{method}`"))?;
        let status = response.status();
        let payload = response
            .bytes()
            .with_context(|| format!("failed to read Telegram `{method}` response"))?;
        let envelope: TelegramApiEnvelope<T> = serde_json::from_slice(&payload)
            .with_context(|| format!("failed to decode Telegram `{method}` response"))?;
        if !status.is_success() || !envelope.ok {
            let description = envelope
                .description
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "Telegram `{method}` failed with HTTP {}",
                        status.as_u16()
                    )
                });
            return Err(TelegramApiError {
                method: method.to_owned(),
                status,
                error_code: envelope.error_code,
                description,
            }
            .into());
        }
        envelope.result.ok_or_else(|| {
            anyhow!("Telegram `{method}` succeeded without returning a result payload")
        })
    }
}

trait TelegramMessageSender {
    fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramChatMessage>;
}

impl TelegramMessageSender for TelegramApiClient {
    fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramChatMessage> {
        TelegramApiClient::send_message(self, chat_id, text, reply_markup)
    }
}

/// Represents TermAl API client.
struct TermalApiClient {
    api_base_url: String,
    client: BlockingHttpClient,
}

impl TermalApiClient {
    /// Creates a new instance.
    fn new(api_base_url: &str) -> Result<Self> {
        let client = BlockingHttpClient::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build TermAl HTTP client")?;
        Ok(Self {
            api_base_url: api_base_url.trim_end_matches('/').to_owned(),
            client,
        })
    }

    /// Gets project digest.
    fn get_project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse> {
        self.request_json(
            Method::GET,
            &format!("/api/projects/{}/digest", encode_uri_component(project_id)),
            None,
        )
    }

    /// Dispatches project action.
    fn dispatch_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse> {
        self.request_json(
            Method::POST,
            &format!(
                "/api/projects/{}/actions/{}",
                encode_uri_component(project_id),
                encode_uri_component(action_id)
            ),
            None,
        )
    }

    fn send_session_message(&self, session_id: &str, text: &str) -> Result<StateResponse> {
        self.request_json(
            Method::POST,
            &format!("/api/sessions/{}/messages", encode_uri_component(session_id)),
            Some(json!({
                "text": text,
                "attachments": [],
            })),
        )
    }

    /// Fetches a session by id. The relay only needs the message
    /// list (specifically the latest assistant `Text` message), so
    /// `TelegramSessionFetchResponse` deliberately deserializes a
    /// narrow subset of the `/api/sessions/{id}` payload — fields
    /// it doesn't care about are ignored by serde's default
    /// "ignore unknown fields" behavior.
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        self.request_json(
            Method::GET,
            &format!("/api/sessions/{}", encode_uri_component(session_id)),
            None,
        )
    }

    fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T> {
        let url = format!("{}{}", self.api_base_url, path);
        let request = match body {
            Some(body) => self.client.request(method, &url).json(&body),
            None => self.client.request(method, &url),
        };
        let response = request
            .send()
            .with_context(|| format!("failed to call TermAl `{path}`"))?;
        let status = response.status();
        let payload = response
            .bytes()
            .with_context(|| format!("failed to read TermAl `{path}` response"))?;
        if !status.is_success() {
            if let Ok(error) = serde_json::from_slice::<ErrorResponse>(&payload) {
                bail!("{}", error.error);
            }
            let detail = String::from_utf8_lossy(&payload).trim().to_owned();
            if detail.is_empty() {
                bail!("TermAl `{path}` failed with HTTP {}", status.as_u16());
            }
            bail!("{detail}");
        }

        serde_json::from_slice(&payload)
            .with_context(|| format!("failed to decode TermAl `{path}` response"))
    }
}

trait TelegramSessionReader {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse>;
}

impl TelegramSessionReader for TermalApiClient {
    fn get_session(&self, session_id: &str) -> Result<TelegramSessionFetchResponse> {
        TermalApiClient::get_session(self, session_id)
    }
}

/// Represents Telegram API envelope.
#[derive(Deserialize)]
struct TelegramApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<i64>,
    description: Option<String>,
}

/// Represents Telegram's `getMe` user shape.
#[derive(Clone, Debug, Deserialize)]
struct TelegramBotUser {
    first_name: String,
    #[serde(default)]
    username: Option<String>,
}

/// Represents Telegram update.
///
/// Field names match Telegram's Bot API exactly (snake_case). Earlier
/// revisions used `#[serde(rename_all = "camelCase")]` here, which made
/// serde look for `updateId` / `callbackQuery` / `messageId` — none of
/// which Telegram sends — so `getUpdates` failed to decode the moment
/// any real update arrived.
#[derive(Clone, Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    callback_query: Option<TelegramCallbackQuery>,
    #[serde(default)]
    message: Option<TelegramChatMessage>,
}

/// Represents Telegram callback query.
#[derive(Clone, Debug, Deserialize)]
struct TelegramCallbackQuery {
    id: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message: Option<TelegramChatMessage>,
}

/// Represents Telegram chat message.
#[derive(Clone, Debug, Deserialize)]
struct TelegramChatMessage {
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
}

/// Represents Telegram chat.
#[derive(Clone, Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    _kind: String,
}

/// Narrow projection of `/api/sessions/{id}` used for forwarding
/// the latest assistant message to Telegram. Only the fields the
/// relay actually consumes are deserialized; everything else in
/// the session payload (preview, status, model, etc.) is ignored
/// by serde's default behavior.
#[derive(Clone, Debug, Deserialize)]
struct TelegramSessionFetchResponse {
    session: TelegramSessionFetchSession,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum TelegramSessionStatus {
    Active,
    Idle,
    Approval,
    Error,
    #[serde(other)]
    Unknown,
}

impl Default for TelegramSessionStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

impl TelegramSessionStatus {
    fn can_forward_settled_assistant_text(&self) -> bool {
        matches!(self, Self::Idle | Self::Approval | Self::Error)
    }
}

/// Inner session record subset (see `TelegramSessionFetchResponse`).
///
/// The `status` field is consulted before any forwarding so the
/// relay only ships SETTLED assistant text. The agent's text
/// message is mutated in place as new tokens stream in (the
/// message id stays stable while the text grows). If the relay
/// forwarded mid-stream, the dedupe-by-id contract would mark a
/// truncated snapshot as "already delivered" and the rest of the
/// reply would silently never reach Telegram. Unknown future
/// statuses are treated as not safe to forward; the next poll can
/// retry once the backend reports a known settled state.
#[derive(Clone, Debug, Deserialize)]
struct TelegramSessionFetchSession {
    #[serde(default)]
    status: TelegramSessionStatus,
    #[serde(default)]
    messages: Vec<TelegramSessionFetchMessage>,
}

/// Subset of the message variants TermAl emits — only `text`
/// matters for the assistant-content forward; every other variant
/// (`thinking`, `command`, `diff`, `markdown`, `approval`, etc.)
/// collapses into `Other` and is ignored. The variants use the
/// same `tag = "type"` discriminator and `lowercase` rename
/// convention as the canonical `Message` enum in
/// `wire_messages.rs` so this projection stays compatible without
/// importing the full type.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum TelegramSessionFetchMessage {
    Text {
        id: String,
        author: String,
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Other,
}

/// Represents Telegram inline keyboard markup.
#[derive(Clone, Debug, Serialize)]
struct TelegramInlineKeyboardMarkup {
    inline_keyboard: Vec<Vec<TelegramInlineKeyboardButton>>,
}

/// Represents Telegram inline keyboard button.
#[derive(Clone, Debug, Serialize)]
struct TelegramInlineKeyboardButton {
    text: String,
    callback_data: String,
}

/// Handles Telegram update.
fn handle_telegram_update(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    update: TelegramUpdate,
) -> Result<bool> {
    if let Some(callback_query) = update.callback_query {
        return handle_telegram_callback_query(telegram, termal, config, state, callback_query);
    }
    if let Some(message) = update.message {
        return handle_telegram_message(telegram, termal, config, state, message);
    }
    Ok(false)
}

/// Handles Telegram message.
fn handle_telegram_message(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    message: TelegramChatMessage,
) -> Result<bool> {
    let Some(text) = message.text.as_deref().map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(false);
    };
    let chat_id = message.chat.id;

    if effective_telegram_chat_id(config, state).is_none() {
        if telegram_command_mentions_other_bot(text, config.bot_username.as_deref()) {
            return Ok(false);
        }
        if matches!(
            parse_telegram_command_for_bot(text, config.bot_username.as_deref())
                .map(|command| command.command),
            Some(TelegramIncomingCommand::Start | TelegramIncomingCommand::Help)
        ) {
            telegram.send_message(
                chat_id,
                &format!(
                    "This TermAl relay is not linked. Set TERMAL_TELEGRAM_CHAT_ID={chat_id} before starting the relay."
                ),
                None,
            )?;
        }
        return Ok(false);
    }

    if effective_telegram_chat_id(config, state) != Some(chat_id) {
        return Ok(false);
    }

    if text.starts_with('/') {
        if telegram_command_mentions_other_bot(text, config.bot_username.as_deref()) {
            return Ok(false);
        }
        let Some(command) = parse_telegram_command_for_bot(text, config.bot_username.as_deref()) else {
            telegram.send_message(chat_id, &telegram_help_text(config), None)?;
            return Ok(false);
        };

        return match command.command {
            TelegramIncomingCommand::Start | TelegramIncomingCommand::Help => {
                telegram.send_message(chat_id, &telegram_help_text(config), None)?;
                Ok(false)
            }
            TelegramIncomingCommand::Status => {
                send_fresh_telegram_digest(telegram, termal, config, state, chat_id)
            }
            TelegramIncomingCommand::Action(action_id) => {
                let digest = termal.dispatch_project_action(&config.project_id, action_id.as_str())?;
                send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &digest)
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
        return Ok(false);
    }

    forward_telegram_text_to_project(telegram, termal, config, state, chat_id, text)
}

/// Handles Telegram callback query.
fn handle_telegram_callback_query(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
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

    let Some(raw_action_id) = callback_query
        .data
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        let _ = telegram.answer_callback_query(&callback_query.id, "That action is empty.");
        return Ok(false);
    };
    let action_id = match ProjectActionId::parse(raw_action_id) {
        Ok(action_id) => action_id,
        Err(_) => {
            let _ = telegram.answer_callback_query(&callback_query.id, "Unknown action.");
            return Ok(false);
        }
    };
    let digest = termal.dispatch_project_action(&config.project_id, action_id.as_str())?;
    let _ = telegram.answer_callback_query(&callback_query.id, action_id.label());
    send_or_edit_telegram_digest_from_response(
        telegram,
        config,
        state,
        chat_id,
        Some(message.message_id),
        &digest,
    )
}

/// Handles forward Telegram text to project.
fn forward_telegram_text_to_project(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    text: &str,
) -> Result<bool> {
    let digest = termal.get_project_digest(&config.project_id)?;
    let Some(session_id) = digest.primary_session_id.as_deref() else {
        telegram.send_message(
            chat_id,
            "No active project session is available yet. Start one in TermAl first.",
            None,
        )?;
        return Ok(false);
    };

    let assistant_forwarding_plan =
        prepare_assistant_forwarding_for_telegram_prompt(termal, session_id)?;
    let _ = termal.send_session_message(session_id, text)?;
    let assistant_forwarding_baseline_changed =
        apply_assistant_forwarding_plan(state, assistant_forwarding_plan);
    let mut dirty = assistant_forwarding_baseline_changed;
    let next_digest = match termal.get_project_digest(&config.project_id) {
        Ok(digest) => digest,
        Err(err) => {
            log_telegram_error("failed to refresh digest after Telegram prompt", &err);
            return Ok(dirty);
        }
    };
    match send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &next_digest) {
        Ok(changed) => dirty |= changed,
        Err(err) => {
            log_telegram_error("failed to send digest after Telegram prompt", &err);
            return Ok(dirty);
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
    dirty |= forward_relevant_assistant_messages(
        telegram,
        termal,
        state,
        chat_id,
        Some(session_id),
    );
    Ok(dirty)
}

/// Syncs Telegram digest.
fn sync_telegram_digest(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let digest = termal.get_project_digest(&config.project_id)?;
    let digest_hash = telegram_digest_hash(&digest, config.public_base_url.as_deref())?;
    let mut dirty = false;

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
        digest.primary_session_id.as_deref(),
    );

    Ok(dirty)
}

fn forward_relevant_assistant_messages(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    chat_id: i64,
    primary_session_id: Option<&str>,
) -> bool {
    let mut dirty = false;
    let armed_session_id = state.forward_next_assistant_message_session_id.clone();

    if let Some(session_id) = armed_session_id.as_deref() {
        merge_assistant_forward_result(
            &mut dirty,
            forward_new_assistant_message_if_any(telegram, termal, state, chat_id, session_id),
        );
        return dirty;
    }

    if let Some(session_id) = primary_session_id {
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

fn clear_forward_next_assistant_message_session_id(
    state: &mut TelegramBotState,
    session_id: &str,
) -> bool {
    if state.forward_next_assistant_message_session_id.as_deref() == Some(session_id) {
        state.forward_next_assistant_message_session_id = None;
        return true;
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TelegramAssistantForwardingPlan {
    Skip,
    Baseline {
        session_id: String,
        latest: Option<(String, usize)>,
    },
}

fn prepare_assistant_forwarding_for_telegram_prompt(
    termal: &impl TelegramSessionReader,
    session_id: &str,
) -> Result<TelegramAssistantForwardingPlan> {
    let response = termal.get_session(session_id)?;
    if !response.session.status.can_forward_settled_assistant_text() {
        return Ok(TelegramAssistantForwardingPlan::Skip);
    }

    Ok(TelegramAssistantForwardingPlan::Baseline {
        session_id: session_id.to_owned(),
        latest: latest_assistant_text_cursor(&response.session.messages),
    })
}

fn apply_assistant_forwarding_plan(
    state: &mut TelegramBotState,
    plan: TelegramAssistantForwardingPlan,
) -> bool {
    let TelegramAssistantForwardingPlan::Baseline { session_id, latest } = plan else {
        return false;
    };
    apply_assistant_forwarding_baseline(state, &session_id, latest)
}

fn apply_assistant_forwarding_baseline(
    state: &mut TelegramBotState,
    session_id: &str,
    latest: Option<(String, usize)>,
) -> bool {
    let mut changed = false;

    if let Some((id, char_count)) = latest {
        if state.last_forwarded_assistant_message_id.as_deref() != Some(id.as_str()) {
            state.last_forwarded_assistant_message_id = Some(id);
            changed = true;
        }
        if state.last_forwarded_assistant_message_text_chars != Some(char_count) {
            state.last_forwarded_assistant_message_text_chars = Some(char_count);
            changed = true;
        }
        changed |= clear_forward_next_assistant_message_session_id(state, session_id);
        return changed;
    }

    if state.last_forwarded_assistant_message_id.take().is_some() {
        changed = true;
    }
    if state
        .last_forwarded_assistant_message_text_chars
        .take()
        .is_some()
    {
        changed = true;
    }
    if state.forward_next_assistant_message_session_id.as_deref() != Some(session_id) {
        state.forward_next_assistant_message_session_id = Some(session_id.to_owned());
        changed = true;
    }

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
fn forward_new_assistant_message_if_any(
    telegram: &impl TelegramMessageSender,
    termal: &impl TelegramSessionReader,
    state: &mut TelegramBotState,
    chat_id: i64,
    session_id: &str,
) -> Result<bool> {
    let response = termal.get_session(session_id)?;

    // While the session is `active`, the latest assistant text
    // message is still being streamed: its `text` field grows as
    // text-deltas arrive but its `id` stays stable. If we forwarded
    // a mid-stream snapshot here, the per-id dedupe contract would
    // mark that truncated snapshot as "already delivered" and the
    // rest of the reply would silently never reach Telegram. Wait
    // for the turn to finish (status flips to `idle`/`approval`/
    // `error`) and forward the settled text on the next sync.
    if !response.session.status.can_forward_settled_assistant_text() {
        return Ok(false);
    }

    let messages = &response.session.messages;

    let forward_without_existing_baseline =
        state.forward_next_assistant_message_session_id.as_deref() == Some(session_id);

    let position_of_last = state
        .last_forwarded_assistant_message_id
        .as_deref()
        .and_then(|tracked| {
            messages.iter().position(|message| {
                matches!(
                    message,
                    TelegramSessionFetchMessage::Text { id, author, .. }
                        if id == tracked && author == "assistant"
                )
            })
        });

    // Detect the "previously forwarded message has grown" case: a
    // forward that started mid-stream stored an id + char count;
    // by the time we re-poll after settle, the same id is present
    // with strictly-greater length. Re-forward that message in
    // full so the user sees the complete settled text instead of
    // a permanently-truncated mid-stream snapshot.
    let needs_resend_truncated = position_of_last
        .and_then(|pos| match &messages[pos] {
            TelegramSessionFetchMessage::Text { author, text, .. } if author == "assistant" => {
                let last_chars = state.last_forwarded_assistant_message_text_chars;
                let current_chars = text.chars().count();
                match last_chars {
                    None => Some(pos),
                    Some(prev) if current_chars > prev => Some(pos),
                    _ => None,
                }
            }
            _ => None,
        });

    // Decide where to start forwarding from. If we have no record
    // OR the recorded id has scrolled off the session (cleared
    // session, switched session, etc.), re-baseline against the
    // current latest assistant message instead of replaying old
    // content.
    let needs_baseline = match (
        state.last_forwarded_assistant_message_id.as_deref(),
        position_of_last,
    ) {
        (_, None) if forward_without_existing_baseline => false,
        (None, _) => true,
        (Some(_), None) => true,
        (Some(_), Some(_)) => false,
    };
    if needs_baseline {
        let latest = latest_assistant_text_cursor(messages);
        let changed = state.last_forwarded_assistant_message_id
            != latest.as_ref().map(|(id, _)| id.clone());
        state.last_forwarded_assistant_message_id = latest.as_ref().map(|(id, _)| id.clone());
        state.last_forwarded_assistant_message_text_chars = latest.map(|(_, len)| len);
        let cleared = clear_forward_next_assistant_message_session_id(state, session_id);
        return Ok(changed || cleared);
    }

    // If the prior forward was truncated, restart at that
    // message's index so it gets re-forwarded as part of the
    // batch. Otherwise start strictly after the last forwarded
    // message.
    let start_index = if let Some(pos) = needs_resend_truncated {
        pos
    } else if forward_without_existing_baseline && position_of_last.is_none() {
        0
    } else {
        position_of_last.expect("position_of_last is Some when not baselining") + 1
    };
    let to_forward: Vec<(String, String)> = messages
        .iter()
        .skip(start_index)
        .filter_map(|message| match message {
            TelegramSessionFetchMessage::Text { id, author, text } if author == "assistant" => {
                Some((id.clone(), text.clone()))
            }
            _ => None,
        })
        .collect();

    if to_forward.is_empty() {
        let cleared = if forward_without_existing_baseline {
            clear_forward_next_assistant_message_session_id(state, session_id)
        } else {
            false
        };
        return Ok(cleared);
    }

    let mut sent_visible_content = false;
    let mut changed = false;
    for (id, text) in &to_forward {
        let trimmed = text.trim();
        // Empty messages still bump the baseline so the next sync
        // doesn't keep re-checking them; they just don't produce a
        // Telegram send.
        if !trimmed.is_empty() {
            for chunk in chunk_telegram_message_text(trimmed) {
                telegram.send_message(chat_id, &chunk, None)?;
            }
            sent_visible_content = true;
        }
        // Record progress per-message so a mid-batch send failure
        // still preserves the messages that DID make it. Capture
        // the char count alongside the id so a streaming-then-
        // settled re-send can be detected by length growth.
        state.last_forwarded_assistant_message_id = Some(id.clone());
        state.last_forwarded_assistant_message_text_chars = Some(text.chars().count());
        changed = true;
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
    if sent_visible_content {
        telegram.send_message(
            chat_id,
            telegram_turn_settled_footer(&response.session.status),
            None,
        )?;
    }

    Ok(changed)
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

/// Telegram's `sendMessage` rejects bodies over 4096 UTF-16 code
/// units. Stay below that limit using the same unit Telegram counts.
const TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS: usize = 3500;

/// Splits `text` into chunks no longer than
/// `TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS`, preferring to break at the
/// last newline within each chunk window so chunks read like
/// natural prose paragraphs rather than mid-sentence cuts.
/// Falls back to a hard UTF-16-unit split when a single
/// line exceeds the limit (e.g., a giant URL or one-line code
/// dump).
fn chunk_telegram_message_text(text: &str) -> Vec<String> {
    if text.encode_utf16().count() <= TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS {
        return vec![text.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = start;
        let mut units = 0;
        let mut last_newline_end = None;
        for (offset, ch) in text[start..].char_indices() {
            let char_start = start + offset;
            let char_end = char_start + ch.len_utf8();
            let char_units = ch.len_utf16();
            if units + char_units > TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS {
                break;
            }

            units += char_units;
            end = char_end;
            if ch == '\n' {
                last_newline_end = Some(char_end);
            }
        }

        if end == start {
            let ch = text[start..]
                .chars()
                .next()
                .expect("chunk start should point at a character");
            end = start + ch.len_utf8();
        }

        let break_at = if end < text.len() {
            last_newline_end
                .filter(|&candidate| candidate > start)
                .unwrap_or(end)
        } else {
            end
        };
        let chunk = &text[start..break_at];
        let trimmed = chunk.trim_end_matches('\n');
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_owned());
        } else if !chunk.is_empty() {
            // Preserve the chunk if all its content was newlines —
            // unusual but better than dropping content silently.
            chunks.push(chunk.to_owned());
        }
        start = break_at;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Handles send fresh Telegram digest.
fn send_fresh_telegram_digest(
    telegram: &TelegramApiClient,
    termal: &TermalApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
) -> Result<bool> {
    let digest = termal.get_project_digest(&config.project_id)?;
    send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &digest)
}

/// Handles send fresh Telegram digest from response.
fn send_fresh_telegram_digest_from_response(
    telegram: &TelegramApiClient,
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    chat_id: i64,
    digest: &ProjectDigestResponse,
) -> Result<bool> {
    let text = render_telegram_digest(digest, config.public_base_url.as_deref());
    let keyboard = build_telegram_digest_keyboard(digest);
    let sent = telegram.send_message(chat_id, &text, keyboard.as_ref())?;
    remember_telegram_digest(
        state,
        digest,
        config.public_base_url.as_deref(),
        sent.message_id,
    )
}

/// Handles send or edit Telegram digest from response.
fn send_or_edit_telegram_digest_from_response(
    telegram: &TelegramApiClient,
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
    telegram: &TelegramApiClient,
    config: &TelegramBotConfig,
    chat_id: i64,
    message_id: Option<i64>,
    digest: &ProjectDigestResponse,
) -> Result<i64> {
    let text = render_telegram_digest(digest, config.public_base_url.as_deref());
    let keyboard = build_telegram_digest_keyboard(digest);
    if let Some(message_id) = message_id {
        match telegram.edit_message(chat_id, message_id, &text, keyboard.as_ref()) {
            Ok(message_id) => return Ok(message_id),
            Err(err) => {
                log_telegram_error("failed to edit digest message", &err);
            }
        }
    }

    let sent = telegram.send_message(chat_id, &text, keyboard.as_ref())?;
    Ok(sent.message_id)
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
    let payload = json!({
        "text": render_telegram_digest(digest, public_base_url),
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
) -> Option<TelegramInlineKeyboardMarkup> {
    if digest.proposed_actions.is_empty() {
        return None;
    }

    let mut rows = Vec::new();
    let mut current_row = Vec::new();
    for action in &digest.proposed_actions {
        current_row.push(TelegramInlineKeyboardButton {
            text: action.label.clone(),
            callback_data: action.id.clone(),
        });
        if current_row.len() == 2 {
            rows.push(current_row);
            current_row = Vec::new();
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    Some(TelegramInlineKeyboardMarkup { inline_keyboard: rows })
}

/// Renders Telegram digest.
fn render_telegram_digest(
    digest: &ProjectDigestResponse,
    public_base_url: Option<&str>,
) -> String {
    let mut lines = vec![
        format!("Project: {}", digest.headline),
        format!("Status: {}", digest.current_status),
        format!("Done: {}", digest.done_summary),
    ];

    if !digest.proposed_actions.is_empty() {
        lines.push(format!(
            "Next: {}",
            digest
                .proposed_actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if let Some(url) = telegram_deep_link_url(digest, public_base_url) {
        lines.push(format!("Open: {url}"));
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

/// Handles Telegram help text.
fn telegram_help_text(config: &TelegramBotConfig) -> String {
    [
        format!("TermAl Telegram relay for project `{}`.", config.project_id),
        "Commands:".to_owned(),
        "/status, /approve, /reject, /continue, /fix, /commit, /iterate, /stop, /review"
            .to_owned(),
        "Reply with free text to forward it into the active project session.".to_owned(),
    ]
    .join("\n")
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
    let Some(expected) = bot_username.map(str::trim).filter(|value| !value.is_empty()) else {
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

fn required_env_var(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}

/// Parses optional i64 environment.
fn parse_optional_i64_env(key: &str) -> Result<Option<i64>> {
    std::env::var(key)
        .ok()
        .map(|value| {
            value.parse::<i64>()
                .with_context(|| format!("{key} is not a valid integer: {value}"))
        })
        .transpose()
}

/// Returns the default TermAl API base URL.
fn default_termal_api_base_url() -> String {
    let port = std::env::var("TERMAL_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8787);
    format!("http://127.0.0.1:{port}")
}
