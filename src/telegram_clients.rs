/*
Telegram and TermAl HTTP clients plus narrow Telegram wire projections.

The relay only deserializes the fields it consumes from Telegram and TermAl
responses, keeping transport details separate from update handling.
*/

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TelegramTextFormat {
    Plain,
    Html,
}

impl TelegramTextFormat {
    fn parse_mode(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Html => Some("HTML"),
        }
    }
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
    fn get_updates(&self, offset: Option<i64>, timeout_secs: u64) -> Result<Vec<TelegramUpdate>> {
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

    fn send_message_with_format(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<TelegramChatMessage> {
        let body = telegram_send_message_body(chat_id, text, reply_markup, format)?;
        self.request_json("sendMessage", Some(body))
    }

    fn edit_message_with_format(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<i64> {
        let body = telegram_edit_message_body(chat_id, message_id, text, reply_markup, format)?;
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
                    format!("Telegram `{method}` failed with HTTP {}", status.as_u16())
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

fn telegram_send_message_body(
    chat_id: i64,
    text: &str,
    reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    format: TelegramTextFormat,
) -> Result<Value> {
    let mut body = json!({
        "chat_id": chat_id,
        "text": text,
        "disable_web_page_preview": true,
    });
    apply_telegram_text_message_options(&mut body, reply_markup, format)?;
    Ok(body)
}

fn telegram_edit_message_body(
    chat_id: i64,
    message_id: i64,
    text: &str,
    reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    format: TelegramTextFormat,
) -> Result<Value> {
    let mut body = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
        "disable_web_page_preview": true,
    });
    apply_telegram_text_message_options(&mut body, reply_markup, format)?;
    Ok(body)
}

fn apply_telegram_text_message_options(
    body: &mut Value,
    reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    format: TelegramTextFormat,
) -> Result<()> {
    if let Some(parse_mode) = format.parse_mode() {
        body["parse_mode"] = json!(parse_mode);
    }
    // Telegram rejects `reply_markup: null` with
    // `Bad Request: object expected as reply markup`; omit absent markup.
    if let Some(keyboard) = reply_markup {
        body["reply_markup"] =
            serde_json::to_value(keyboard).context("failed to serialize Telegram reply_markup")?;
    }
    Ok(())
}

trait TelegramMessageSender {
    fn send_message_with_format(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<TelegramChatMessage>;

    fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
    ) -> Result<TelegramChatMessage> {
        self.send_message_with_format(chat_id, text, reply_markup, TelegramTextFormat::Plain)
    }
}

impl TelegramMessageSender for TelegramApiClient {
    fn send_message_with_format(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<TelegramChatMessage> {
        TelegramApiClient::send_message_with_format(self, chat_id, text, reply_markup, format)
    }
}

trait TelegramDigestMessageSender: TelegramMessageSender {
    fn edit_message_with_format(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<i64>;
}

impl TelegramDigestMessageSender for TelegramApiClient {
    fn edit_message_with_format(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&TelegramInlineKeyboardMarkup>,
        format: TelegramTextFormat,
    ) -> Result<i64> {
        TelegramApiClient::edit_message_with_format(
            self,
            chat_id,
            message_id,
            text,
            reply_markup,
            format,
        )
    }
}

trait TelegramCallbackResponder: TelegramDigestMessageSender {
    fn answer_callback_query(&self, callback_query_id: &str, text: &str) -> Result<()>;
}

impl TelegramCallbackResponder for TelegramApiClient {
    fn answer_callback_query(&self, callback_query_id: &str, text: &str) -> Result<()> {
        TelegramApiClient::answer_callback_query(self, callback_query_id, text)
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

    /// Reads `/api/state` through a narrow Telegram projection. The relay uses
    /// the broad state endpoint intentionally so project and session selection
    /// commands share one consistent snapshot instead of racing separate
    /// project/session list calls.
    fn get_state_sessions(&self) -> Result<TelegramStateSessionsResponse> {
        self.request_json(Method::GET, "/api/state", None)
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
            &format!(
                "/api/sessions/{}/messages",
                encode_uri_component(session_id)
            ),
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

/// Test-seam client for forwarding a Telegram-originated prompt. This one
/// bound intentionally colocates the state/digest reads used to choose a
/// target with the prompt-send write, so `forward_telegram_text_to_project`
/// can be tested without standing up either HTTP client.
trait TelegramPromptClient: TelegramSessionReader {
    fn get_project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse>;
    fn get_state_sessions(&self) -> Result<TelegramStateSessionsResponse>;
    /// Only success/failure matters to the relay after sending a prompt; the
    /// full `SessionMessageResponse` would be ignored because subsequent
    /// forwarding uses fresh session reads for cursor baselining.
    fn send_session_message(&self, session_id: &str, text: &str) -> Result<()>;
}

impl TelegramPromptClient for TermalApiClient {
    fn get_project_digest(&self, project_id: &str) -> Result<ProjectDigestResponse> {
        TermalApiClient::get_project_digest(self, project_id)
    }

    fn get_state_sessions(&self) -> Result<TelegramStateSessionsResponse> {
        TermalApiClient::get_state_sessions(self)
    }

    fn send_session_message(&self, session_id: &str, text: &str) -> Result<()> {
        let _ = TermalApiClient::send_session_message(self, session_id, text)?;
        Ok(())
    }
}

trait TelegramActionClient {
    fn dispatch_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse>;
}

impl TelegramActionClient for TermalApiClient {
    fn dispatch_project_action(
        &self,
        project_id: &str,
        action_id: &str,
    ) -> Result<ProjectDigestResponse> {
        TermalApiClient::dispatch_project_action(self, project_id, action_id)
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

/// Narrow projection of `/api/state` used by Telegram project/session
/// selection commands. The relay needs project labels plus session ids,
/// project bindings, statuses, and message counts; the rest of the app
/// state response is intentionally ignored by serde. Field names follow
/// the camelCase wire contract exposed by the HTTP API.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramStateSessionsResponse {
    #[serde(default)]
    projects: Vec<TelegramStateProject>,
    #[serde(default)]
    sessions: Vec<TelegramStateSession>,
}

/// Project entry from the `/api/state` projection used to render Telegram
/// project labels.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramStateProject {
    id: String,
    name: String,
}

/// Session entry from the `/api/state` projection used to list and select
/// Telegram prompt targets within a project.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramStateSession {
    id: String,
    name: String,
    #[serde(default)]
    project_id: Option<String>,
    status: TelegramSessionStatus,
    #[serde(default)]
    message_count: u32,
    #[serde(default)]
    session_mutation_stamp: Option<u64>,
    #[serde(default)]
    parent_delegation_id: Option<String>,
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
    /// `Approval` deliberately belongs to both helper predicates: an approval
    /// pause is a settled state for normal assistant forwarding, but it can
    /// still be part of the pre-existing local turn when a Telegram prompt is
    /// queued behind it. `Unknown` keeps the boundary open until a known status
    /// arrives, avoiding a premature baseline across future status variants.
    fn can_forward_settled_assistant_text(&self) -> bool {
        matches!(self, Self::Idle | Self::Approval | Self::Error)
    }

    fn keeps_telegram_prompt_boundary_open(&self) -> bool {
        matches!(self, Self::Active | Self::Approval | Self::Unknown)
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
