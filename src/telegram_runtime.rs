/*
Telegram relay runtime and configuration.

This include! fragment keeps process lifecycle, environment-derived config,
and in-process relay supervision out of the update handling module.
*/

const TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";
const TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS: u64 = 5;
const TELEGRAM_ERROR_RETRY_DELAY: Duration = Duration::from_secs(2);
const TELEGRAM_GET_UPDATES_LIMIT: i64 = 25;
const TELEGRAM_MAX_UPDATES_PER_ITERATION: usize = TELEGRAM_GET_UPDATES_LIMIT as usize;
const TELEGRAM_RELAY_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TELEGRAM_USER_ERROR_MAX_CHARS: usize = 240;
const TELEGRAM_CALLBACK_ERROR_MAX_CHARS: usize = 180;
const TELEGRAM_SAFE_USER_ERROR_DETAIL: &str = "Check TermAl for details, then try again.";
const TELEGRAM_CALLBACK_DATA_MAX_BYTES: usize = 64;

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

/// Runs Telegram bot.
fn run_telegram_bot() -> Result<()> {
    let cwd_path = std::env::current_dir().context("failed to resolve current directory")?;
    let cwd = cwd_path
        .to_str()
        .context("current directory is not valid UTF-8")?
        .to_owned();
    let config = TelegramBotConfig::from_env(&cwd)?;
    run_telegram_bot_with_config(config, None, None)
}

fn run_telegram_bot_with_config(
    mut config: TelegramBotConfig,
    shutdown: Option<Arc<AtomicBool>>,
    on_ready: Option<Box<dyn FnOnce() + Send>>,
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

    if let Some(on_ready) = on_ready {
        on_ready();
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
    let update_count = updates.len();
    if update_count > TELEGRAM_MAX_UPDATES_PER_ITERATION {
        eprintln!(
            "telegram> capped oversized update batch at {TELEGRAM_MAX_UPDATES_PER_ITERATION} of {update_count} updates"
        );
    }
    for update in updates
        .into_iter()
        .take(TELEGRAM_MAX_UPDATES_PER_ITERATION)
    {
        if telegram_relay_shutdown_requested(shutdown) {
            break;
        }
        let next_update_id = update.update_id.saturating_add(1);
        if state.next_update_id != Some(next_update_id) {
            state.next_update_id = Some(next_update_id);
            dirty = true;
            persist_telegram_update_cursor_after_update(&config.state_path, state);
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
        bot_token: Option<String>,
    ) -> Result<Self, TelegramRelayConfigUnavailableReason> {
        if !file.config.enabled {
            return Err(TelegramRelayConfigUnavailableReason::Disabled);
        }
        let bot_token = match bot_token
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

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum TelegramRelayRuntimeState {
    #[default]
    Idle,
    Spawning,
    Running,
    Stopping,
}

impl TelegramRelayRuntimeState {
    fn is_active(self) -> bool {
        matches!(self, Self::Spawning | Self::Running | Self::Stopping)
    }

    fn is_running(self) -> bool {
        self.is_active()
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

            let should_start = {
                let runtime = TELEGRAM_RELAY_RUNTIME
                    .lock()
                    .expect("telegram relay runtime mutex poisoned");
                if runtime.generation == generation && !shutdown.load(Ordering::Relaxed) {
                    true
                } else {
                    false
                }
            };
            if !should_start {
                return;
            }

            let ready_shutdown = shutdown.clone();
            let result = run_telegram_bot_with_config(
                config,
                Some(shutdown),
                Some(Box::new(move || {
                    let mut runtime = TELEGRAM_RELAY_RUNTIME
                        .lock()
                        .expect("telegram relay runtime mutex poisoned");
                    if runtime.generation == generation && !ready_shutdown.load(Ordering::Relaxed)
                    {
                        runtime.state = TelegramRelayRuntimeState::Running;
                    }
                })),
            );
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
    let previous_handle = runtime.handle.take();
    runtime.config_fingerprint = None;
    runtime.generation = runtime.generation.saturating_add(1);
    let generation = runtime.generation;
    runtime.state = if previous_handle.is_some() {
        TelegramRelayRuntimeState::Stopping
    } else {
        TelegramRelayRuntimeState::Idle
    };
    drop(runtime);

    if let Some(previous_handle) = previous_handle {
        if let Err(err) = previous_handle.join() {
            eprintln!("telegram> in-process relay thread panicked while stopping: {err:?}");
        }
        let mut runtime = TELEGRAM_RELAY_RUNTIME
            .lock()
            .expect("telegram relay runtime mutex poisoned");
        if runtime.generation == generation {
            runtime.state = TelegramRelayRuntimeState::Idle;
        }
    }
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
    static TELEGRAM_RELAY_RUNTIME_RUNNING_FOR_TESTS: std::cell::RefCell<bool> =
        const { std::cell::RefCell::new(false) };
}

#[cfg(test)]
fn start_telegram_relay_runtime(config: TelegramBotConfig) {
    TELEGRAM_RELAY_RUNTIME_RUNNING_FOR_TESTS.with(|running| {
        *running.borrow_mut() = true;
    });
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
    TELEGRAM_RELAY_RUNTIME_RUNNING_FOR_TESTS.with(|running| {
        *running.borrow_mut() = false;
    });
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS.with(|actions| {
        actions
            .borrow_mut()
            .push(TelegramRelayRuntimeActionForTest::Stop);
    });
}

#[cfg(test)]
fn reset_telegram_relay_runtime_actions_for_tests() {
    TELEGRAM_RELAY_RUNTIME_ACTIONS_FOR_TESTS.with(|actions| actions.borrow_mut().clear());
    TELEGRAM_RELAY_RUNTIME_RUNNING_FOR_TESTS.with(|running| {
        *running.borrow_mut() = false;
    });
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
    let running = TELEGRAM_RELAY_RUNTIME_RUNNING_FOR_TESTS.with(|running| *running.borrow());
    TelegramRelayStatusSnapshot {
        running,
        lifecycle: TelegramLifecycle::InProcess,
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
