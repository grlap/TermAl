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
const TELEGRAM_TARGET_ID_MAX_BYTES: usize = 256;
const TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT: usize = 100;
const TELEGRAM_TEST_COOLDOWN: Duration = Duration::from_secs(2);
const TELEGRAM_TEST_COOLDOWN_RETRY_AFTER: &str = "2";
const TELEGRAM_TEST_RATE_LIMIT_MESSAGE: &str =
    "Telegram connection tests are rate-limited. Try again in a moment.";

static TELEGRAM_SETTINGS_FILE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static TELEGRAM_TEST_RATE_LIMIT: LazyLock<Mutex<Option<std::time::Instant>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TelegramBotFile {
    #[serde(default)]
    config: TelegramUiConfig,
    #[serde(default, flatten)]
    state: TelegramBotState,
}

impl TelegramStatusResponse {
    fn from_telegram_settings(
        config: TelegramUiConfig,
        state: TelegramBotState,
        relay: TelegramRelayStatusSnapshot,
    ) -> Self {
        let configured = config
            .bot_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty());
        Self {
            configured,
            enabled: config.enabled,
            running: relay.running,
            lifecycle: relay.lifecycle,
            linked_chat_id: state.chat_id,
            bot_token_masked: config
                .bot_token
                .as_deref()
                .and_then(mask_telegram_bot_token),
            subscribed_project_ids: config.subscribed_project_ids,
            default_project_id: config.default_project_id,
            default_session_id: config.default_session_id,
        }
    }
}

impl AppState {
    fn telegram_bot_file_path(&self) -> PathBuf {
        resolve_termal_data_dir(&self.default_workdir).join("telegram-bot.json")
    }

    fn telegram_status(&self) -> Result<TelegramStatusResponse, ApiError> {
        let _guard = telegram_settings_file_guard();
        let mut file = self.load_telegram_bot_file()?;
        if self.sanitize_telegram_config_for_current_state_in_place(&mut file.config) {
            self.persist_telegram_bot_file(&file)?;
        }
        Ok(self.telegram_status_from_file(file))
    }

    fn update_telegram_config(
        &self,
        request: UpdateTelegramConfigRequest,
    ) -> Result<TelegramStatusResponse, ApiError> {
        let _guard = telegram_settings_file_guard();
        let mut file = self.load_telegram_bot_file()?;
        // Layering is intentional: first tolerate/scrub stale persisted
        // project/session references, then validate the user's patch strictly.
        file.config = self.sanitize_telegram_config_for_current_state(file.config);

        if let Some(enabled) = request.enabled {
            file.config.enabled = enabled;
        }
        if let Some(bot_token) = request.bot_token {
            file.config.bot_token = normalize_optional_secret(bot_token);
        }
        if let Some(project_ids) = request.subscribed_project_ids {
            file.config.subscribed_project_ids = normalize_project_id_list(project_ids)?;
        }
        if let Some(project_id) = request.default_project_id {
            file.config.default_project_id =
                normalize_optional_id(project_id, "default Telegram project id")?;
        }
        if let Some(session_id) = request.default_session_id {
            file.config.default_session_id =
                normalize_optional_id(session_id, "default Telegram session id")?;
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
                self.load_telegram_bot_file()?
                    .config
                    .bot_token
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
        let relay = telegram_relay_status_snapshot();
        TelegramStatusResponse::from_telegram_settings(config, file.state, relay)
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
        if let Some(token) = config.bot_token.as_deref() {
            validate_telegram_bot_token(token)?;
        }
        let mut subscribed_project_ids = config.subscribed_project_ids.clone();
        let mut default_project_id = config.default_project_id.clone();
        let default_session_id = config.default_session_id.clone();
        validate_telegram_target_ids(
            &subscribed_project_ids,
            default_project_id.as_deref(),
            default_session_id.as_deref(),
        )?;

        let inner = self.inner.lock().expect("state mutex poisoned");
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();

        for project_id in &subscribed_project_ids {
            if !known_projects.contains(project_id.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown Telegram project `{project_id}`"
                )));
            }
        }

        if default_project_id.is_none() && subscribed_project_ids.len() == 1 {
            default_project_id = subscribed_project_ids.first().cloned();
        }

        if let Some(project_id) = default_project_id.clone() {
            if !known_projects.contains(project_id.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown default Telegram project `{project_id}`"
                )));
            }
            if !subscribed_project_ids
                .iter()
                .any(|candidate| candidate == &project_id)
            {
                subscribed_project_ids.push(project_id);
            }
        }

        if let Some(session_id) = default_session_id.as_deref() {
            let session = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "unknown default Telegram session `{session_id}`"
                    ))
                })?;
            let session_project_id = session.session.project_id.as_deref().ok_or_else(|| {
                ApiError::bad_request("default Telegram session must belong to a project")
            })?;

            if !known_projects.contains(session_project_id) {
                return Err(ApiError::bad_request(format!(
                    "unknown default Telegram session project `{session_project_id}`"
                )));
            }

            match default_project_id.as_deref() {
                Some(project_id) if project_id != session_project_id => {
                    return Err(ApiError::bad_request(
                        "default Telegram session must belong to the default project",
                    ));
                }
                Some(_) => {}
                None => {
                    default_project_id = Some(session_project_id.to_owned());
                }
            }

            if !subscribed_project_ids
                .iter()
                .any(|candidate| candidate == session_project_id)
            {
                subscribed_project_ids.push(session_project_id.to_owned());
            }
        }

        if config.enabled
            && config
                .bot_token
                .as_deref()
                .is_some_and(|token| !token.trim().is_empty())
            && subscribed_project_ids.is_empty()
        {
            return Err(ApiError::bad_request(
                "choose at least one Telegram project before enabling the relay",
            ));
        }

        config.subscribed_project_ids = subscribed_project_ids;
        config.default_project_id = default_project_id;
        config.default_session_id = default_session_id;
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
        if telegram_config_is_enabled_without_project_target(&file.config) {
            file.config.enabled = false;
        }

        if telegram_configs_equal(&before, &file.config) {
            return Ok(());
        }

        self.persist_telegram_bot_file(&file)?;
        #[cfg(not(test))]
        self.reconcile_telegram_relay_for_loaded_file(&file);
        Ok(())
    }

    fn prune_telegram_state_for_deleted_session(&self, session_id: &str) -> Result<(), ApiError> {
        let _guard = telegram_settings_file_guard();
        let mut file = self.load_telegram_bot_file()?;
        let mut changed = false;

        changed |= file
            .state
            .assistant_forwarding_cursors
            .remove(session_id)
            .is_some();
        changed |= clear_forward_next_assistant_message_session_id(&mut file.state, session_id);
        if file.state.selected_session_id.as_deref() == Some(session_id) {
            changed |= clear_telegram_project_scoped_state(&mut file.state);
        }
        if file.config.default_session_id.as_deref() == Some(session_id) {
            file.config.default_session_id = None;
            changed = true;
        }

        if !changed {
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
        self.sanitize_telegram_config_for_current_state_in_place(&mut config);
        config
    }

    fn sanitize_telegram_config_for_current_state_in_place(
        &self,
        config: &mut TelegramUiConfig,
    ) -> bool {
        let mut changed = false;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();
        let old_subscription_count = config.subscribed_project_ids.len();
        config
            .subscribed_project_ids
            .retain(|project_id| known_projects.contains(project_id.as_str()));
        changed |= config.subscribed_project_ids.len() != old_subscription_count;

        if config.default_project_id.is_none() && config.subscribed_project_ids.len() == 1 {
            config.default_project_id = config.subscribed_project_ids.first().cloned();
            changed = true;
        }

        if !config
            .default_project_id
            .as_deref()
            .is_some_and(|project_id| known_projects.contains(project_id))
        {
            changed |= config.default_project_id.is_some() || config.default_session_id.is_some();
            config.default_project_id = None;
            config.default_session_id = None;
            return changed;
        }

        if let Some(session_id) = config.default_session_id.as_deref() {
            let default_project_id = config.default_project_id.as_deref();
            let session_matches = inner.sessions.iter().any(|record| {
                record.session.id == session_id
                    && record.session.project_id.as_deref() == default_project_id
            });
            if !session_matches {
                config.default_session_id = None;
                changed = true;
            }
        }

        changed
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
) -> Result<Response, ApiError> {
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("Telegram test request", rejection))?;
    match run_blocking_api(move || state.test_telegram_connection(request)).await {
        Ok(response) => Ok(Json(response).into_response()),
        Err(err)
            if err.status == StatusCode::TOO_MANY_REQUESTS
                && err.message == TELEGRAM_TEST_RATE_LIMIT_MESSAGE =>
        {
            let mut response = err.into_response();
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from_static(TELEGRAM_TEST_COOLDOWN_RETRY_AFTER),
            );
            Ok(response)
        }
        Err(err) => Err(err),
    }
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

fn normalize_optional_id(value: Option<String>, label: &str) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    validate_telegram_target_id(label, &value)?;
    Ok(Some(value))
}

fn validate_telegram_bot_token(token: &str) -> Result<(), ApiError> {
    if token.chars().count() > TELEGRAM_BOT_TOKEN_MAX_CHARS {
        return Err(ApiError::bad_request(format!(
            "Telegram bot token must be at most {TELEGRAM_BOT_TOKEN_MAX_CHARS} characters"
        )));
    }
    Ok(())
}

fn check_telegram_test_rate_limit(_token: &str) -> Result<(), ApiError> {
    let now = std::time::Instant::now();
    let mut last_attempt = TELEGRAM_TEST_RATE_LIMIT
        .lock()
        .expect("telegram test rate limit mutex poisoned");

    if let Some(last_attempt) = *last_attempt {
        if now.duration_since(last_attempt) < TELEGRAM_TEST_COOLDOWN {
            return Err(ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS,
                TELEGRAM_TEST_RATE_LIMIT_MESSAGE,
            ));
        }
    }

    *last_attempt = Some(now);
    Ok(())
}

#[cfg(test)]
fn reset_telegram_test_rate_limit_for_tests() {
    *TELEGRAM_TEST_RATE_LIMIT
        .lock()
        .expect("telegram test rate limit mutex poisoned") = None;
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

fn normalize_project_id_list(values: Vec<String>) -> Result<Vec<String>, ApiError> {
    if values.len() > TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT {
        return Err(ApiError::bad_request(format!(
            "Telegram subscribed projects must include at most {TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT} projects"
        )));
    }
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if value.is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        validate_telegram_target_id("Telegram subscribed project id", &value)?;
        normalized.push(value);
    }
    Ok(normalized)
}

fn validate_telegram_target_ids(
    subscribed_project_ids: &[String],
    default_project_id: Option<&str>,
    default_session_id: Option<&str>,
) -> Result<(), ApiError> {
    if subscribed_project_ids.len() > TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT {
        return Err(ApiError::bad_request(format!(
            "Telegram subscribed projects must include at most {TELEGRAM_SUBSCRIBED_PROJECT_IDS_MAX_COUNT} projects"
        )));
    }
    for project_id in subscribed_project_ids {
        validate_telegram_target_id("Telegram subscribed project id", project_id)?;
    }
    if let Some(project_id) = default_project_id {
        validate_telegram_target_id("default Telegram project id", project_id)?;
    }
    if let Some(session_id) = default_session_id {
        validate_telegram_target_id("default Telegram session id", session_id)?;
    }
    Ok(())
}

fn validate_telegram_target_id(label: &str, value: &str) -> Result<(), ApiError> {
    if value.len() > TELEGRAM_TARGET_ID_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "{label} must be at most {TELEGRAM_TARGET_ID_MAX_BYTES} bytes"
        )));
    }
    Ok(())
}

fn telegram_configs_equal(left: &TelegramUiConfig, right: &TelegramUiConfig) -> bool {
    left.enabled == right.enabled
        && left.bot_token == right.bot_token
        && left.subscribed_project_ids == right.subscribed_project_ids
        && left.default_project_id == right.default_project_id
        && left.default_session_id == right.default_session_id
}

fn telegram_config_is_enabled_without_project_target(config: &TelegramUiConfig) -> bool {
    // Keep this predicate shared with prune paths so project deletion cannot
    // persist a relay shape the normal settings save path would reject.
    config.enabled
        && config
            .bot_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
        && config.subscribed_project_ids.is_empty()
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
