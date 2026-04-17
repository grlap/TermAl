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

/// Runs Telegram bot.
fn run_telegram_bot() -> Result<()> {
    let cwd_path = std::env::current_dir().context("failed to resolve current directory")?;
    let cwd = cwd_path
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();
    let config = TelegramBotConfig::from_env(&cwd)?;
    let termal = TermalApiClient::new(&config.api_base_url)?;
    let telegram = TelegramApiClient::new(&config.bot_token, config.poll_timeout_secs)?;
    let mut state = load_telegram_bot_state(&config.state_path).unwrap_or_default();
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
        None => println!("chat: not linked; send /start to the bot to link it"),
    }

    loop {
        let updates = match telegram.get_updates(state.next_update_id, config.poll_timeout_secs) {
            Ok(updates) => updates,
            Err(err) => {
                eprintln!("telegram> failed to poll updates: {err:#}");
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
                Err(err) => eprintln!("telegram> failed to handle update: {err:#}"),
            }
        }

        if let Some(chat_id) = effective_telegram_chat_id(&config, &state) {
            match sync_telegram_digest(&telegram, &termal, &config, &mut state, chat_id) {
                Ok(changed) => dirty |= changed,
                Err(err) => eprintln!("telegram> failed to sync digest: {err:#}"),
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
}

/// Loads Telegram bot state.
fn load_telegram_bot_state(path: &FsPath) -> Result<TelegramBotState> {
    if !path.exists() {
        return Ok(TelegramBotState::default());
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))
}

/// Persists Telegram bot state.
fn persist_telegram_bot_state(path: &FsPath, state: &TelegramBotState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(state).context("failed to serialize telegram bot state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Computes the effective Telegram chat ID.
fn effective_telegram_chat_id(config: &TelegramBotConfig, state: &TelegramBotState) -> Option<i64> {
    config.chat_id.or(state.chat_id)
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
        self.request_json(
            "sendMessage",
            Some(json!({
                "chat_id": chat_id,
                "text": text,
                "reply_markup": reply_markup,
                "disable_web_page_preview": true,
            })),
        )
    }

    fn edit_message(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<i64> {
        let result: Value = self.request_json(
            "editMessageText",
            Some(json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "text": text,
                "reply_markup": reply_markup,
                "disable_web_page_preview": true,
            })),
        )?;

        Ok(result
            .get("message_id")
            .and_then(Value::as_i64)
            .unwrap_or(message_id))
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
            let detail = envelope
                .description
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "Telegram `{method}` failed with HTTP {}",
                        status.as_u16()
                    )
                });
            bail!("{detail}");
        }
        envelope.result.ok_or_else(|| {
            anyhow!("Telegram `{method}` succeeded without returning a result payload")
        })
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

/// Represents Telegram API envelope.
#[derive(Deserialize)]
struct TelegramApiEnvelope<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

/// Represents Telegram update.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    callback_query: Option<TelegramCallbackQuery>,
    #[serde(default)]
    message: Option<TelegramChatMessage>,
}

/// Represents Telegram callback query.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramCallbackQuery {
    id: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message: Option<TelegramChatMessage>,
}

/// Represents Telegram chat message.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramChatMessage {
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
}

/// Represents Telegram chat.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    _kind: String,
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
        if matches!(
            parse_telegram_command(text).map(|command| command.command),
            Some(TelegramIncomingCommand::Start | TelegramIncomingCommand::Help)
        ) {
            state.chat_id = Some(chat_id);
            telegram.send_message(chat_id, &telegram_help_text(config), None)?;
            return send_fresh_telegram_digest(telegram, termal, config, state, chat_id);
        }
        return Ok(false);
    }

    if effective_telegram_chat_id(config, state) != Some(chat_id) {
        return Ok(false);
    }

    if text.starts_with('/') {
        let Some(command) = parse_telegram_command(text) else {
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

    let _ = termal.send_session_message(session_id, text)?;
    let next_digest = termal.get_project_digest(&config.project_id)?;
    send_fresh_telegram_digest_from_response(telegram, config, state, chat_id, &next_digest)
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
    if state.last_digest_hash.as_deref() == Some(digest_hash.as_str()) {
        return Ok(false);
    }

    let message_id = edit_or_send_telegram_digest(
        telegram,
        config,
        chat_id,
        state.last_digest_message_id,
        &digest,
    )?;
    Ok(remember_telegram_digest(
        state,
        &digest,
        config.public_base_url.as_deref(),
        message_id,
    )?)
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
                eprintln!("telegram> failed to edit digest message: {err:#}");
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
fn parse_telegram_command(text: &str) -> Option<TelegramParsedCommand<'_>> {
    let trimmed = text.trim();
    let command_text = trimmed.strip_prefix('/')?;
    let (raw_name, args) = match command_text.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (command_text, ""),
    };
    let name = raw_name.split('@').next().unwrap_or(raw_name).trim();
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
