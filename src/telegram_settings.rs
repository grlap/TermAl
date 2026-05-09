/*
Telegram settings HTTP surface.

This owns the UI-facing config/status/test endpoints for the Telegram relay.
The relay loop still reads the legacy flat runtime fields from
`telegram-bot.json`; the settings file format below keeps those fields flat and
adds a `config` object so the existing `cargo run -- telegram` path can ignore
UI-only fields during the transition.

Locking invariant: file I/O uses `telegram_settings_file_guard()`, and callers
must not hold the main app state mutex while acquiring that guard. This module
may briefly read app state while holding the file guard for validation, but it
must release app state before writing to disk.
*/

const TELEGRAM_BOT_TOKEN_MAX_CHARS: usize = 256;
const TELEGRAM_TEST_COOLDOWN: Duration = Duration::from_secs(2);
const TELEGRAM_TEST_RATE_LIMIT_RETAIN: Duration = Duration::from_secs(60);

static TELEGRAM_SETTINGS_FILE_LOCK: LazyLock<Mutex<()>> =
    LazyLock::new(|| Mutex::new(()));
static TELEGRAM_TEST_RATE_LIMITS: LazyLock<Mutex<HashMap<String, std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TelegramBotFile {
    #[serde(default)]
    config: TelegramUiConfig,
    #[serde(default, flatten)]
    state: TelegramBotState,
}

impl AppState {
    fn telegram_bot_file_path(&self) -> PathBuf {
        resolve_termal_data_dir(&self.default_workdir).join("telegram-bot.json")
    }

    fn telegram_status(&self) -> Result<TelegramStatusResponse, ApiError> {
        let _guard = telegram_settings_file_guard();
        let file = self.load_telegram_bot_file()?;
        Ok(self.telegram_status_from_file(file))
    }

    fn update_telegram_config(
        &self,
        request: UpdateTelegramConfigRequest,
    ) -> Result<TelegramStatusResponse, ApiError> {
        let _guard = telegram_settings_file_guard();
        let mut file = self.load_telegram_bot_file()?;
        // Stale on-disk project/session references are tolerated before
        // applying the user's patch; user-supplied unknown ids are still
        // rejected by validation below.
        file.config = self.sanitize_telegram_config_for_current_state(file.config);

        if let Some(enabled) = request.enabled {
            file.config.enabled = enabled;
        }
        if let Some(bot_token) = request.bot_token {
            file.config.bot_token = normalize_optional_secret(bot_token);
        }
        if let Some(project_ids) = request.subscribed_project_ids {
            file.config.subscribed_project_ids = normalize_project_id_list(project_ids);
        }
        if let Some(project_id) = request.default_project_id {
            file.config.default_project_id = normalize_optional_id(project_id);
        }
        if let Some(session_id) = request.default_session_id {
            file.config.default_session_id = normalize_optional_id(session_id);
        }

        self.validate_and_normalize_telegram_config(&mut file.config)?;
        // Re-sanitize after validation so a concurrent project/session delete
        // cannot leave references that the next status read would hide.
        file.config = self.sanitize_telegram_config_for_current_state(file.config);
        self.persist_telegram_bot_file(&file)?;
        #[cfg(not(test))]
        self.reconcile_telegram_relay_for_loaded_file(&file);

        Ok(self.telegram_status_from_file(file))
    }

    fn test_telegram_connection(
        &self,
        request: TelegramTestRequest,
    ) -> Result<TelegramTestResponse, ApiError> {
        let token = match request.bot_token {
            Some(value) => normalize_optional_secret(value)
                .ok_or_else(|| ApiError::bad_request("Telegram bot token is required"))?,
            None if request.use_saved_token => {
                let _guard = telegram_settings_file_guard();
                self.load_telegram_bot_file()?.config.bot_token
                    .ok_or_else(|| ApiError::bad_request("Telegram bot token is required"))?
            }
            None => return Err(ApiError::bad_request("Telegram bot token is required")),
        };
        validate_telegram_bot_token(&token)?;
        check_telegram_test_rate_limit(&token)?;

        let telegram = TelegramApiClient::new(&token, TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS)
            .map_err(|err| ApiError::internal(sanitize_telegram_log_detail(&err.to_string())))?;
        let bot = telegram.get_me().map_err(telegram_test_connection_error)?;

        Ok(TelegramTestResponse {
            bot_name: bot.first_name,
            bot_username: bot.username,
        })
    }

    fn telegram_status_from_file(&self, file: TelegramBotFile) -> TelegramStatusResponse {
        let config = self.sanitize_telegram_config_for_current_state(file.config);
        let configured = config
            .bot_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty());
        let relay = telegram_relay_status_snapshot();
        TelegramStatusResponse {
            configured,
            enabled: config.enabled,
            running: relay.running,
            lifecycle: relay.lifecycle,
            linked_chat_id: file.state.chat_id,
            bot_token_masked: config.bot_token.as_deref().and_then(mask_telegram_bot_token),
            subscribed_project_ids: config.subscribed_project_ids,
            default_project_id: config.default_project_id,
            default_session_id: config.default_session_id,
        }
    }

    fn load_telegram_bot_file(&self) -> Result<TelegramBotFile, ApiError> {
        let path = self.telegram_bot_file_path();
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(TelegramBotFile::default());
            }
            Err(err) => return Err(telegram_settings_file_error("read", &path, err)),
        };
        serde_json::from_slice(&raw)
            .map_err(|err| telegram_settings_file_error("parse", &path, err))
    }

    fn persist_telegram_bot_file(&self, file: &TelegramBotFile) -> Result<(), ApiError> {
        let path = self.telegram_bot_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                telegram_settings_file_error("create parent directory for", parent, err)
            })?;
        }

        let encoded = serde_json::to_vec_pretty(file).map_err(|err| {
            ApiError::internal(format!("failed to serialize Telegram settings: {err}"))
        })?;
        write_telegram_bot_file(&path, &encoded)
            .map_err(|err| telegram_settings_file_error("write", &path, err))
    }

    fn validate_and_normalize_telegram_config(
        &self,
        config: &mut TelegramUiConfig,
    ) -> Result<(), ApiError> {
        let mut normalized = config.clone();

        if let Some(token) = normalized.bot_token.as_deref() {
            validate_telegram_bot_token(token)?;
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();

        for project_id in &normalized.subscribed_project_ids {
            if !known_projects.contains(project_id.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown Telegram project `{project_id}`"
                )));
            }
        }

        if normalized.default_project_id.is_none()
            && normalized.subscribed_project_ids.len() == 1
        {
            normalized.default_project_id = normalized.subscribed_project_ids.first().cloned();
        }

        if let Some(project_id) = normalized.default_project_id.clone() {
            if !known_projects.contains(project_id.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown default Telegram project `{project_id}`"
                )));
            }
            if !normalized
                .subscribed_project_ids
                .iter()
                .any(|candidate| candidate == &project_id)
            {
                normalized.subscribed_project_ids.push(project_id);
            }
        }

        if let Some(session_id) = normalized.default_session_id.as_deref() {
            let session = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .ok_or_else(|| {
                    ApiError::bad_request(format!("unknown default Telegram session `{session_id}`"))
                })?;
            let session_project_id = session.session.project_id.as_deref().ok_or_else(|| {
                ApiError::bad_request("default Telegram session must belong to a project")
            })?;

            if !known_projects.contains(session_project_id) {
                return Err(ApiError::bad_request(format!(
                    "unknown default Telegram session project `{session_project_id}`"
                )));
            }

            match normalized.default_project_id.as_deref() {
                Some(project_id) if project_id != session_project_id => {
                    return Err(ApiError::bad_request(
                        "default Telegram session must belong to the default project",
                    ));
                }
                Some(_) => {}
                None => {
                    normalized.default_project_id = Some(session_project_id.to_owned());
                }
            }

            if !normalized
                .subscribed_project_ids
                .iter()
                .any(|candidate| candidate == session_project_id)
            {
                normalized
                    .subscribed_project_ids
                    .push(session_project_id.to_owned());
            }
        }

        *config = normalized;
        Ok(())
    }

    fn prune_telegram_config_for_deleted_project(&self, project_id: &str) -> Result<(), ApiError> {
        let _guard = telegram_settings_file_guard();
        let mut file = self.load_telegram_bot_file()?;
        let before = file.config.clone();

        file.config
            .subscribed_project_ids
            .retain(|candidate| candidate != project_id);
        if file.config.default_project_id.as_deref() == Some(project_id) {
            file.config.default_project_id = None;
            file.config.default_session_id = None;
        }

        if telegram_configs_equal(&before, &file.config) {
            return Ok(());
        }

        self.persist_telegram_bot_file(&file)?;
        #[cfg(not(test))]
        self.reconcile_telegram_relay_for_loaded_file(&file);
        Ok(())
    }

    fn sanitize_telegram_config_for_current_state(
        &self,
        mut config: TelegramUiConfig,
    ) -> TelegramUiConfig {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();
        config
            .subscribed_project_ids
            .retain(|project_id| known_projects.contains(project_id.as_str()));

        if config.default_project_id.is_none()
            && config.subscribed_project_ids.len() == 1
        {
            config.default_project_id = config.subscribed_project_ids.first().cloned();
        }

        if !config
            .default_project_id
            .as_deref()
            .is_some_and(|project_id| known_projects.contains(project_id))
        {
            config.default_project_id = None;
            config.default_session_id = None;
            return config;
        }

        if let Some(session_id) = config.default_session_id.as_deref() {
            let default_project_id = config.default_project_id.as_deref();
            let session_matches = inner.sessions.iter().any(|record| {
                record.session.id == session_id
                    && record.session.project_id.as_deref() == default_project_id
            });
            if !session_matches {
                config.default_session_id = None;
            }
        }

        config
    }

    #[cfg(not(test))]
    fn reconcile_telegram_relay_from_saved_settings(&self) {
        let _guard = telegram_settings_file_guard();
        match self.load_telegram_bot_file() {
            Ok(file) => self.reconcile_telegram_relay_for_loaded_file(&file),
            Err(err) => {
                eprintln!(
                    "telegram settings> failed to load relay config for startup: {}",
                    sanitize_telegram_log_detail(&err.message)
                );
                stop_telegram_relay_runtime();
            }
        }
    }

    #[cfg(not(test))]
    fn reconcile_telegram_relay_for_loaded_file(&self, file: &TelegramBotFile) {
        let mut file = file.clone();
        file.config = self.sanitize_telegram_config_for_current_state(file.config);
        if let Some(config) = TelegramBotConfig::from_ui_file(&self.default_workdir, &file) {
            start_telegram_relay_runtime(config);
        } else {
            stop_telegram_relay_runtime();
        }
    }
}

async fn get_telegram_status(
    State(state): State<AppState>,
) -> Result<Json<TelegramStatusResponse>, ApiError> {
    let response = run_blocking_api(move || state.telegram_status()).await?;
    Ok(Json(response))
}

async fn update_telegram_config(
    State(state): State<AppState>,
    request: Result<Json<UpdateTelegramConfigRequest>, JsonRejection>,
) -> Result<Json<TelegramStatusResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Telegram settings request", rejection))?;
    let response = run_blocking_api(move || state.update_telegram_config(request)).await?;
    Ok(Json(response))
}

async fn test_telegram_connection(
    State(state): State<AppState>,
    request: Result<Json<TelegramTestRequest>, JsonRejection>,
) -> Result<Json<TelegramTestResponse>, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Telegram test request", rejection))?;
    let response = run_blocking_api(move || state.test_telegram_connection(request)).await?;
    Ok(Json(response))
}

fn telegram_settings_file_guard() -> std::sync::MutexGuard<'static, ()> {
    TELEGRAM_SETTINGS_FILE_LOCK
        .lock()
        .expect("telegram settings file mutex poisoned")
}

fn normalize_optional_secret(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_id(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn validate_telegram_bot_token(token: &str) -> Result<(), ApiError> {
    if token.chars().count() > TELEGRAM_BOT_TOKEN_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "Telegram bot token must be at most {TELEGRAM_BOT_TOKEN_MAX_CHARS} characters"
        )));
    }
    Ok(())
}

fn check_telegram_test_rate_limit(token: &str) -> Result<(), ApiError> {
    let key = telegram_test_rate_limit_key(token);
    let now = std::time::Instant::now();
    let mut limits = TELEGRAM_TEST_RATE_LIMITS
        .lock()
        .expect("telegram test rate limit mutex poisoned");
    limits.retain(|_, last_attempt| now.duration_since(*last_attempt) <= TELEGRAM_TEST_RATE_LIMIT_RETAIN);

    if let Some(last_attempt) = limits.get(&key) {
        if now.duration_since(*last_attempt) < TELEGRAM_TEST_COOLDOWN {
            return Err(ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                "Telegram connection tests are rate-limited. Try again in a moment.",
            ));
        }
    }

    limits.insert(key, now);
    Ok(())
}

fn telegram_test_rate_limit_key(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn telegram_test_connection_error(err: anyhow::Error) -> ApiError {
    let detail = sanitize_telegram_log_detail(&err.to_string());
    let message = format!("Telegram connection test failed: {detail}");
    if let Some(api_error) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<TelegramApiError>())
    {
        if telegram_getme_error_is_rate_limited(api_error) {
            ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, message)
        } else if telegram_getme_error_is_token_validation_failure(api_error) {
            ApiError::from_status(StatusCode::UNPROCESSABLE_ENTITY, message)
        } else {
            ApiError::bad_gateway(message)
        }
    } else {
        ApiError::bad_gateway(message)
    }
}

/// Classifies Telegram `getMe` rate limits for `/api/telegram/test`.
///
/// Telegram can carry rate-limit information in either the JSON API
/// `error_code` or the HTTP status, so either signal is enough once the method
/// is known to be `getMe`.
fn telegram_getme_error_is_rate_limited(err: &TelegramApiError) -> bool {
    err.method == "getMe"
        && (err.error_code == Some(429) || err.status == StatusCode::TOO_MANY_REQUESTS)
}

/// Classifies Telegram `getMe` token/auth validation failures.
///
/// Unlike rate limits, validation failures require aligned API and HTTP signals
/// when Telegram supplies an API code. Contradictory envelopes fall back to a
/// generic upstream failure so token-remediation UI is not shown for ambiguous
/// Telegram responses.
fn telegram_getme_error_is_token_validation_failure(err: &TelegramApiError) -> bool {
    if err.method != "getMe" {
        return false;
    }

    let status_is_token_error = matches!(
        err.status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
    );
    match err.error_code {
        Some(code) => matches!(code, 400 | 401 | 403 | 404) && status_is_token_error,
        None => status_is_token_error,
    }
}

fn normalize_project_id_list(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        normalized.push(value);
    }
    normalized
}

fn telegram_configs_equal(left: &TelegramUiConfig, right: &TelegramUiConfig) -> bool {
    left.enabled == right.enabled
        && left.bot_token == right.bot_token
        && left.subscribed_project_ids == right.subscribed_project_ids
        && left.default_project_id == right.default_project_id
        && left.default_session_id == right.default_session_id
}

fn mask_telegram_bot_token(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }

    let suffix_chars: Vec<char> = token.chars().rev().take(4).collect();
    let suffix: String = suffix_chars.into_iter().rev().collect();
    Some(format!("****{suffix}"))
}

fn telegram_settings_file_error(
    operation: &str,
    path: &FsPath,
    err: impl std::fmt::Display,
) -> ApiError {
    let err = sanitize_telegram_log_detail(&err.to_string());
    eprintln!(
        "telegram settings> failed to {operation} `{}`: {err}",
        path.display()
    );
    ApiError::internal(format!("failed to {operation} Telegram settings"))
}

fn write_telegram_bot_file(path: &FsPath, encoded: &[u8]) -> io::Result<()> {
    let temp_path = telegram_bot_temp_file_path(path);
    let result = (|| {
        let mut file = open_telegram_bot_temp_file(&temp_path)?;
        file.write_all(encoded)?;
        file.sync_all()?;
        drop(file);
        harden_telegram_bot_file_permissions(&temp_path)?;
        replace_telegram_bot_file(&temp_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    result
}

fn telegram_bot_temp_file_path(path: &FsPath) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| FsPath::new("."));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("telegram-bot.json");
    parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()))
}

#[cfg(windows)]
fn replace_telegram_bot_file(temp_path: &FsPath, path: &FsPath) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    fn wide_path(path: &FsPath) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let from = wide_path(temp_path);
    let to = wide_path(path);
    let result = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_telegram_bot_file(temp_path: &FsPath, path: &FsPath) -> io::Result<()> {
    fs::rename(temp_path, path)
}

#[cfg(unix)]
fn open_telegram_bot_temp_file(path: &FsPath) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .create(true)
        .create_new(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_telegram_bot_temp_file(path: &FsPath) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .create_new(true)
        .truncate(true)
        .write(true)
        .open(path)
}

#[cfg(unix)]
fn harden_telegram_bot_file_permissions(path: &FsPath) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn harden_telegram_bot_file_permissions(_path: &FsPath) -> io::Result<()> {
    Ok(())
}
