/*
Telegram relay
Telegram Bot API <-> telegram.rs <-> local TermAl project digest/actions
Poll updates
  -> link chat / parse command / forward free text
  -> GET project digest or POST project action
  -> render digest + inline keyboard after updates have been drained
  -> persist chat binding and digest cursor
This adapter runs as a separate CLI mode. It reuses the same backend project
action contract instead of exposing a second transport-specific control path.
*/

const TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";
const TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS: u64 = 5;
const TELEGRAM_ERROR_RETRY_DELAY: Duration = Duration::from_secs(2);
const TELEGRAM_GET_UPDATES_LIMIT: i64 = 25;
const TELEGRAM_RELAY_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TELEGRAM_USER_ERROR_MAX_CHARS: usize = 240;
const TELEGRAM_CALLBACK_ERROR_MAX_CHARS: usize = 180;
const TELEGRAM_SAFE_USER_ERROR_DETAIL: &str = "Check TermAl for details, then try again.";
const TELEGRAM_CALLBACK_DATA_MAX_BYTES: usize = 64;

/// Runs Telegram bot.
fn run_telegram_bot() -> Result<()> {
    let cwd_path = std::env::current_dir().context("failed to resolve current directory")?;
    let cwd = cwd_path
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();
    let config = TelegramBotConfig::from_env(&cwd)?;
    run_telegram_bot_with_config(config, None)
}

fn run_telegram_bot_with_config(
    mut config: TelegramBotConfig,
    shutdown: Option<Arc<AtomicBool>>,
) -> Result<()> {
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
    let mut state =
        load_telegram_bot_state(&config.state_path).context("failed to load Telegram bot state")?;
    let mut dirty = false;
    if let Some(chat_id) = config.chat_id {
        if state.chat_id != Some(chat_id) {
            state.chat_id = Some(chat_id);
            dirty = true;
        }
    }
    let (_, selected_project_dirty) = resolve_telegram_active_project_id(&config, &mut state);
    dirty |= selected_project_dirty;
    if dirty {
        persist_telegram_bot_state(&config.state_path, &state)?;
    }

    println!("TermAl Telegram adapter");
    println!("api: {}", config.api_base_url);
    println!("project: {}", config.project_id);
    println!(
        "subscribed projects: {}",
        config.subscribed_project_ids.join(", ")
    );
    match effective_telegram_chat_id(&config, &state) {
        Some(chat_id) => println!("chat: {chat_id}"),
        None => println!(
            "chat: not linked; set TERMAL_TELEGRAM_CHAT_ID or use the Settings link flow when it is enabled"
        ),
    }

    while !telegram_relay_shutdown_requested(&shutdown) {
        let updates = match telegram.get_updates(state.next_update_id, config.poll_timeout_secs) {
            Ok(updates) => updates,
            Err(err) => {
                log_telegram_error("failed to poll updates", &err);
                persist_dirty_telegram_state_after_poll_error(&config.state_path, &state, false);
                telegram_relay_sleep(TELEGRAM_ERROR_RETRY_DELAY, &shutdown);
                continue;
            }
        };

        let dirty = drain_telegram_updates_then_sync_digest(
            &telegram, &termal, &config, &mut state, updates, &shutdown,
        );

        if dirty {
            persist_telegram_bot_state(&config.state_path, &state)?;
        }
    }
    Ok(())
}

fn telegram_relay_shutdown_requested(shutdown: &Option<Arc<AtomicBool>>) -> bool {
    shutdown
        .as_ref()
        .is_some_and(|value| value.load(Ordering::Relaxed))
}

fn telegram_relay_sleep(duration: Duration, shutdown: &Option<Arc<AtomicBool>>) {
    let mut remaining = duration;
    while !remaining.is_zero() && !telegram_relay_shutdown_requested(shutdown) {
        let chunk = remaining.min(TELEGRAM_RELAY_SHUTDOWN_POLL_INTERVAL);
        std::thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
}

fn drain_telegram_updates_then_sync_digest(
    telegram: &impl TelegramCallbackResponder,
    termal: &(impl TelegramPromptClient + TelegramActionClient),
    config: &TelegramBotConfig,
    state: &mut TelegramBotState,
    updates: Vec<TelegramUpdate>,
    shutdown: &Option<Arc<AtomicBool>>,
) -> bool {
    let mut dirty = false;
    let mut final_sync_satisfied = false;
    for update in updates {
        if telegram_relay_shutdown_requested(shutdown) {
            break;
        }
        let next_update_id = update.update_id.saturating_add(1);
        if state.next_update_id != Some(next_update_id) {
            state.next_update_id = Some(next_update_id);
            dirty = true;
        }

        match handle_telegram_update(telegram, termal, config, state, update) {
            Ok(outcome) => {
                dirty |= outcome.dirty;
                final_sync_satisfied = outcome.final_sync_satisfied;
            }
            Err(err) => {
                final_sync_satisfied = false;
                log_telegram_error("failed to handle update", &err);
            }
        }
    }

    if !final_sync_satisfied && !telegram_relay_shutdown_requested(shutdown) {
        if let Some(chat_id) = effective_telegram_chat_id(config, state) {
            match sync_telegram_digest(telegram, termal, config, state, chat_id) {
                Ok(changed) => dirty |= changed,
                Err(err) => log_telegram_error("failed to sync digest", &err),
            }
        }
    }

    dirty
}

/// Holds Telegram bot configuration.
#[derive(Clone, Debug)]
struct TelegramBotConfig {
    api_base_url: String,
    bot_username: Option<String>,
    bot_token: String,
    chat_id: Option<i64>,
    poll_timeout_secs: u64,
    project_id: String,
    public_base_url: Option<String>,
    state_path: PathBuf,
    subscribed_project_ids: Vec<String>,
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
            project_id: project_id.clone(),
            public_base_url,
            state_path,
            subscribed_project_ids: vec![project_id],
        })
    }

    fn from_ui_file(
        default_workdir: &str,
        file: &TelegramBotFile,
    ) -> Result<Self, TelegramRelayConfigUnavailableReason> {
        if !file.config.enabled {
            return Err(TelegramRelayConfigUnavailableReason::Disabled);
        }
        let bot_token = match file
            .config
            .bot_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(token) => token.to_owned(),
            None => return Err(TelegramRelayConfigUnavailableReason::MissingBotToken),
        };
        let project_id = telegram_effective_default_project_id(&file.config)
            .ok_or(TelegramRelayConfigUnavailableReason::MissingProjectTarget)?;
        if project_id.is_empty() {
            return Err(TelegramRelayConfigUnavailableReason::MissingProjectTarget);
        }
        let subscribed_project_ids =
            telegram_effective_subscribed_project_ids(&file.config, &project_id);
        let state_path = resolve_termal_data_dir(default_workdir).join("telegram-bot.json");

        Ok(Self {
            api_base_url: default_termal_api_base_url(),
            bot_username: None,
            bot_token,
            chat_id: file.state.chat_id,
            poll_timeout_secs: TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS,
            project_id,
            public_base_url: None,
            state_path,
            subscribed_project_ids,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum TelegramRelayConfigUnavailableReason {
    Disabled,
    MissingBotToken,
    MissingProjectTarget,
}

fn telegram_effective_default_project_id(config: &TelegramUiConfig) -> Option<String> {
    config
        .default_project_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let mut subscribed = config
                .subscribed_project_ids
                .iter()
                .map(|project_id| project_id.trim())
                .filter(|project_id| !project_id.is_empty());
            let only = subscribed.next()?;
            if subscribed.next().is_none() {
                Some(only.to_owned())
            } else {
                None
            }
        })
}

fn telegram_effective_subscribed_project_ids(
    config: &TelegramUiConfig,
    default_project_id: &str,
) -> Vec<String> {
    let mut project_ids = Vec::new();
    for project_id in config
        .subscribed_project_ids
        .iter()
        .map(|project_id| project_id.trim())
        .filter(|project_id| !project_id.is_empty())
    {
        if !project_ids.iter().any(|candidate| candidate == project_id) {
            project_ids.push(project_id.to_owned());
        }
    }

    let default_project_id = default_project_id.trim();
    if !default_project_id.is_empty()
        && !project_ids
            .iter()
            .any(|project_id| project_id == default_project_id)
    {
        project_ids.push(default_project_id.to_owned());
    }

    project_ids
}

#[derive(Clone, Copy)]
struct TelegramRelayStatusSnapshot {
    running: bool,
    lifecycle: TelegramLifecycle,
}

#[cfg(not(test))]
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum TelegramRelayRuntimeState {
    #[default]
    Idle,
    Spawning,
    Running,
}

#[cfg(not(test))]
impl TelegramRelayRuntimeState {
    fn is_active(self) -> bool {
        matches!(self, Self::Spawning | Self::Running)
    }

    fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

#[cfg(not(test))]
#[derive(Default)]
struct TelegramRelayRuntime {
    config_fingerprint: Option<String>,
    generation: u64,
    handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Option<Arc<AtomicBool>>,
    state: TelegramRelayRuntimeState,
}

#[cfg(not(test))]
static TELEGRAM_RELAY_RUNTIME: LazyLock<Mutex<TelegramRelayRuntime>> =
    LazyLock::new(|| Mutex::new(TelegramRelayRuntime::default()));

#[cfg(not(test))]
fn start_telegram_relay_runtime(config: TelegramBotConfig) {
    let fingerprint = telegram_relay_config_fingerprint(&config);
    let mut runtime = TELEGRAM_RELAY_RUNTIME
        .lock()
        .expect("telegram relay runtime mutex poisoned");
    if runtime.config_fingerprint.as_deref() == Some(fingerprint.as_str())
        && runtime.state.is_active()
    {
        return;
    }

    let previous_shutdown = runtime.shutdown.take();
    let previous_handle = runtime.handle.take();

    let shutdown = Arc::new(AtomicBool::new(false));
    runtime.shutdown = Some(shutdown.clone());
    runtime.config_fingerprint = Some(fingerprint);
    runtime.generation = runtime.generation.saturating_add(1);
    let generation = runtime.generation;
    runtime.state = TelegramRelayRuntimeState::Spawning;
    drop(runtime);

    if let Some(previous_shutdown) = previous_shutdown {
        previous_shutdown.store(true, Ordering::Relaxed);
    }

    match std::thread::Builder::new()
        .name("termal-telegram-relay".to_owned())
        .spawn(move || {
            if let Some(previous_handle) = previous_handle {
                if let Err(err) = previous_handle.join() {
                    eprintln!(
                        "telegram> previous in-process relay thread panicked: {:?}",
                        err
                    );
                }
            }

            let should_run = {
                let mut runtime = TELEGRAM_RELAY_RUNTIME
                    .lock()
                    .expect("telegram relay runtime mutex poisoned");
                if runtime.generation == generation && !shutdown.load(Ordering::Relaxed) {
                    runtime.state = TelegramRelayRuntimeState::Running;
                    true
                } else {
                    false
                }
            };
            if !should_run {
                return;
            }

            let result = run_telegram_bot_with_config(config, Some(shutdown));
            if let Err(err) = result {
                eprintln!(
                    "telegram> in-process relay stopped: {}",
                    sanitize_telegram_log_detail(&err.to_string())
                );
            }
            let mut runtime = TELEGRAM_RELAY_RUNTIME
                .lock()
                .expect("telegram relay runtime mutex poisoned");
            if runtime.generation == generation {
                runtime.state = TelegramRelayRuntimeState::Idle;
                runtime.shutdown = None;
            }
        }) {
        Ok(handle) => {
            let mut runtime = TELEGRAM_RELAY_RUNTIME
                .lock()
                .expect("telegram relay runtime mutex poisoned");
            if runtime.generation == generation {
                runtime.handle = Some(handle);
            }
        }
        Err(err) => {
            let mut runtime = TELEGRAM_RELAY_RUNTIME
                .lock()
                .expect("telegram relay runtime mutex poisoned");
            if runtime.generation == generation {
                runtime.state = TelegramRelayRuntimeState::Idle;
                runtime.shutdown = None;
                runtime.config_fingerprint = None;
            }
            eprintln!(
                "telegram> failed to start in-process relay: {}",
                sanitize_telegram_log_detail(&err.to_string())
            );
        }
    }
}

#[cfg(not(test))]
fn stop_telegram_relay_runtime() {
    let mut runtime = TELEGRAM_RELAY_RUNTIME
        .lock()
        .expect("telegram relay runtime mutex poisoned");
    if let Some(shutdown) = runtime.shutdown.take() {
        shutdown.store(true, Ordering::Relaxed);
    }
    runtime.config_fingerprint = None;
    runtime.generation = runtime.generation.saturating_add(1);
    runtime.state = TelegramRelayRuntimeState::Idle;
}

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq)]
enum TelegramRelayRuntimeActionForTest {
    Start {
        project_id: String,
        subscribed_project_ids: Vec<String>,
    },
    Stop,
}

#[cfg(test)]
thread_local! {
    static TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS: std::cell::RefCell<Vec<TelegramRelayRuntimeActionForTest>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn start_telegram_relay_runtime(config: TelegramBotConfig) {
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS.with(|actions| {
        actions
            .borrow_mut()
            .push(TelegramRelayRuntimeActionForTest::Start {
                project_id: config.project_id,
                subscribed_project_ids: config.subscribed_project_ids,
            });
    });
}

#[cfg(test)]
fn stop_telegram_relay_runtime() {
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS.with(|actions| {
        actions
            .borrow_mut()
            .push(TelegramRelayRuntimeActionForTest::Stop);
    });
}

#[cfg(test)]
fn reset_telegram_relay_runtime_actions_for_tests() {
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS.with(|actions| actions.borrow_mut().clear());
}

#[cfg(test)]
fn take_telegram_relay_runtime_actions_for_tests() -> Vec<TelegramRelayRuntimeActionForTest> {
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS
        .with(|actions| std::mem::take(&mut *actions.borrow_mut()))
}

#[cfg(not(test))]
fn telegram_relay_status_snapshot() -> TelegramRelayStatusSnapshot {
    let runtime = TELEGRAM_RELAY_RUNTIME
        .lock()
        .expect("telegram relay runtime mutex poisoned");
    TelegramRelayStatusSnapshot {
        running: runtime.state.is_running(),
        lifecycle: TelegramLifecycle::InProcess,
    }
}

#[cfg(test)]
fn telegram_relay_status_snapshot() -> TelegramRelayStatusSnapshot {
    TelegramRelayStatusSnapshot {
        running: false,
        lifecycle: TelegramLifecycle::Manual,
    }
}

#[cfg(not(test))]
fn telegram_relay_config_fingerprint(config: &TelegramBotConfig) -> String {
    let mut hasher = Sha256::new();
    let payload = json!({
        "version": 1,
        "apiBaseUrl": &config.api_base_url,
        "botToken": &config.bot_token,
        "chatId": config.chat_id,
        "pollTimeoutSecs": config.poll_timeout_secs,
        "projectId": &config.project_id,
        "publicBaseUrl": &config.public_base_url,
        "subscribedProjectIds": &config.subscribed_project_ids,
    });
    let mut encoded =
        serde_json::to_vec(&payload).expect("telegram relay fingerprint should encode");
    hasher.update(&encoded);
    zeroize::Zeroize::zeroize(&mut encoded);
    format!("{:x}", hasher.finalize())
}

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
                backup_corrupt_telegram_bot_file(path, &err)?;
                TelegramBotFile::default()
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
            telegram.send_message(
                chat_id,
                &format!(
                    "This TermAl relay is not linked. Set TERMAL_TELEGRAM_CHAT_ID={chat_id} before starting the relay."
                ),
                None,
            )?;
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

    let assistant_forwarding_plan =
        prepare_assistant_forwarding_for_telegram_prompt(termal, session_id)?;
    termal.send_session_message(session_id, text)?;
    let assistant_forwarding_baseline_changed =
        apply_assistant_forwarding_plan(state, assistant_forwarding_plan);
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

    // While a session is active or approval-paused, assistant text can still
    // belong to the pre-existing local turn. If this arm was created behind
    // that turn, keep updating the baseline and do not forward old output as
    // the Telegram prompt's reply.
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

    if !response.session.status.can_forward_settled_assistant_text() {
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

    // Detect the "previously forwarded message has grown" case: a
    // forward that started mid-stream stored an id + char count;
    // by the time we re-poll after settle, the same id is present
    // with strictly-greater length. Re-forward that message in
    // full so the user sees the complete settled text instead of
    // a permanently-truncated mid-stream snapshot.
    let needs_resend_truncated = if cursor.resend_if_grown {
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
    } else if let Some(pos) = needs_resend_truncated {
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
    for (id, text) in &to_forward {
        let trimmed = text.trim();
        // Empty messages still bump the baseline so the next sync
        // doesn't keep re-checking them; they just don't produce a
        // Telegram send.
        if !trimmed.is_empty() {
            let chunks = chunk_telegram_message_text(trimmed);
            let text_chars = text.chars().count();
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
            text_chars: Some(text.chars().count()),
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
    if sent_visible_content {
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

/// Telegram's `sendMessage` rejects bodies over 4096 UTF-16 code
/// units. Stay below that limit using the same unit Telegram counts.
const TELEGRAM_MESSAGE_CHUNK_UTF16_UNITS: usize = 3500;
const TELEGRAM_DIGEST_FIELD_MAX_CHARS: usize = 120;
const TELEGRAM_ASSISTANT_CHUNK_SEND_FAILURE_LIMIT: usize = 3;

fn telegram_assistant_chunk_skipped_notice(
    chunk_index: usize,
    chunk_count: usize,
    failed_attempts: usize,
) -> String {
    format!(
        "[Telegram skipped assistant reply chunk {}/{} after {} failed delivery attempts.]",
        chunk_index + 1,
        chunk_count,
        failed_attempts
    )
}

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
        if !chunk.is_empty() {
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
    let mut parts = args.split_whitespace();
    let Some(raw_session_id) = parts.next() else {
        let text = match state.selected_session_id.as_deref() {
            Some(session_id) => format!(
                "Current Telegram session target: `{session_id}`.\nSend /session <session-id> to switch, /session clear to use the current project session, or /sessions to list ids."
            ),
            None => "No Telegram session target is selected. Send /session <session-id> to switch, or /sessions to list ids.".to_owned(),
        };
        telegram.send_message(chat_id, &text, None)?;
        return Ok(dirty);
    };
    if parts.next().is_some() {
        telegram.send_message(
            chat_id,
            "Use /session <session-id>, or /session clear to use the current project session.",
            None,
        )?;
        return Ok(dirty);
    }

    if matches!(raw_session_id, "clear" | "default" | "auto") {
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
    let Some(session) = find_telegram_project_session(&sessions, &project_id, raw_session_id)
    else {
        telegram.send_message(
            chat_id,
            &format!(
                "I couldn't find session `{raw_session_id}` in project `{}`. Send /sessions to list available ids.",
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
    let name = session.name.trim();
    let label = if name.is_empty() {
        session.id.as_str()
    } else {
        name
    };
    telegram.send_message(
        chat_id,
        &format!(
            "Telegram session target set to {label}.\nid: {}\nFree text will go to this session. Send /session clear to use the current project session.",
            session.id
        ),
        None,
    )?;
    Ok(dirty)
}

fn find_telegram_project_session<'a>(
    state: &'a TelegramStateSessionsResponse,
    project_id: &str,
    session_id: &str,
) -> Option<&'a TelegramStateSession> {
    state.sessions.iter().find(|session| {
        session.id == session_id && session.project_id.as_deref() == Some(project_id)
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
        .filter(|session| session.project_id.as_deref() == Some(project_id))
        .collect::<Vec<_>>();
    let has_more_sessions = sessions.len() > 12;
    sessions.reverse();
    sessions.sort_by_key(|session| !matches!(session.status, TelegramSessionStatus::Active));

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
            session.name.trim()
        ));
        lines.push(format!("  id: {}", session.id));
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
        "/status, /projects, /project <id>, /sessions, /session <id>, /approve, /reject, /continue, /fix, /commit, /iterate, /stop, /review"
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
            value
                .parse::<i64>()
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
