/*
Telegram settings HTTP surface.

This owns the UI-facing config/status/test endpoints for the Telegram relay.
The relay loop still reads the legacy flat runtime fields from
`telegram-bot.json`; the settings file format below keeps those fields flat and
adds a `config` object so the existing `cargo run -- telegram` path can ignore
UI-only fields during the transition.
*/

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
        let file = self.load_telegram_bot_file()?;
        Ok(self.telegram_status_from_file(file))
    }

    fn update_telegram_config(
        &self,
        request: UpdateTelegramConfigRequest,
    ) -> Result<TelegramStatusResponse, ApiError> {
        let mut file = self.load_telegram_bot_file()?;

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

        self.validate_telegram_config(&mut file.config)?;
        self.persist_telegram_bot_file(&file)?;

        Ok(self.telegram_status_from_file(file))
    }

    fn test_telegram_connection(
        &self,
        request: TelegramTestRequest,
    ) -> Result<TelegramTestResponse, ApiError> {
        let token = request
            .bot_token
            .and_then(|value| normalize_optional_secret(Some(value)))
            .or_else(|| {
                self.load_telegram_bot_file()
                    .ok()
                    .and_then(|file| file.config.bot_token)
            })
            .ok_or_else(|| ApiError::bad_request("Telegram bot token is required"))?;

        let telegram = TelegramApiClient::new(&token, TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS)
            .map_err(|err| ApiError::internal(sanitize_telegram_log_detail(&err.to_string())))?;
        let bot = telegram.get_me().map_err(|err| {
            ApiError::bad_gateway(format!(
                "Telegram connection test failed: {}",
                sanitize_telegram_log_detail(&err.to_string())
            ))
        })?;

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
        TelegramStatusResponse {
            configured,
            enabled: config.enabled,
            // Phase 0 only persists configuration. The supervised in-process
            // relay will flip this once lifecycle ownership moves into the backend.
            running: false,
            lifecycle: "manual".to_owned(),
            linked_chat_id: file.state.chat_id,
            bot_token_masked: config.bot_token.as_deref().and_then(mask_telegram_bot_token),
            subscribed_project_ids: config.subscribed_project_ids,
            default_project_id: config.default_project_id,
            default_session_id: config.default_session_id,
        }
    }

    fn load_telegram_bot_file(&self) -> Result<TelegramBotFile, ApiError> {
        let path = self.telegram_bot_file_path();
        if !path.exists() {
            return Ok(TelegramBotFile::default());
        }

        let raw = fs::read(&path).map_err(|err| {
            ApiError::internal(format!("failed to read `{}`: {err}", path.display()))
        })?;
        serde_json::from_slice(&raw).map_err(|err| {
            ApiError::internal(format!("failed to parse `{}`: {err}", path.display()))
        })
    }

    fn persist_telegram_bot_file(&self, file: &TelegramBotFile) -> Result<(), ApiError> {
        let path = self.telegram_bot_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                ApiError::internal(format!("failed to create `{}`: {err}", parent.display()))
            })?;
        }

        let encoded = serde_json::to_vec_pretty(file).map_err(|err| {
            ApiError::internal(format!("failed to serialize Telegram settings: {err}"))
        })?;
        fs::write(&path, encoded).map_err(|err| {
            ApiError::internal(format!("failed to write `{}`: {err}", path.display()))
        })
    }

    fn validate_telegram_config(&self, config: &mut TelegramUiConfig) -> Result<(), ApiError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal("state lock poisoned"))?;
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();

        for project_id in &config.subscribed_project_ids {
            if !known_projects.contains(project_id.as_str()) {
                return Err(ApiError::bad_request(format!(
                    "unknown Telegram project `{project_id}`"
                )));
            }
        }

        if let Some(project_id) = config.default_project_id.as_deref() {
            if !known_projects.contains(project_id) {
                return Err(ApiError::bad_request(format!(
                    "unknown default Telegram project `{project_id}`"
                )));
            }
            if !config
                .subscribed_project_ids
                .iter()
                .any(|candidate| candidate == project_id)
            {
                config.subscribed_project_ids.push(project_id.to_owned());
            }
        }

        if let Some(session_id) = config.default_session_id.as_deref() {
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

            match config.default_project_id.as_deref() {
                Some(project_id) if project_id != session_project_id => {
                    return Err(ApiError::bad_request(
                        "default Telegram session must belong to the default project",
                    ));
                }
                Some(_) => {}
                None => {
                    config.default_project_id = Some(session_project_id.to_owned());
                }
            }

            if !config
                .subscribed_project_ids
                .iter()
                .any(|candidate| candidate == session_project_id)
            {
                config
                    .subscribed_project_ids
                    .push(session_project_id.to_owned());
            }
        }

        Ok(())
    }

    fn sanitize_telegram_config_for_current_state(
        &self,
        mut config: TelegramUiConfig,
    ) -> TelegramUiConfig {
        let Ok(inner) = self.inner.lock() else {
            return config;
        };
        let known_projects = inner
            .projects
            .iter()
            .map(|project| project.id.as_str())
            .collect::<HashSet<_>>();
        config
            .subscribed_project_ids
            .retain(|project_id| known_projects.contains(project_id.as_str()));

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
}

async fn get_telegram_status(
    State(state): State<AppState>,
) -> Result<Json<TelegramStatusResponse>, ApiError> {
    let response = run_blocking_api(move || state.telegram_status()).await?;
    Ok(Json(response))
}

async fn update_telegram_config(
    State(state): State<AppState>,
    Json(request): Json<UpdateTelegramConfigRequest>,
) -> Result<Json<TelegramStatusResponse>, ApiError> {
    let response = run_blocking_api(move || state.update_telegram_config(request)).await?;
    Ok(Json(response))
}

async fn test_telegram_connection(
    State(state): State<AppState>,
    Json(request): Json<TelegramTestRequest>,
) -> Result<Json<TelegramTestResponse>, ApiError> {
    let response = run_blocking_api(move || state.test_telegram_connection(request)).await?;
    Ok(Json(response))
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

fn mask_telegram_bot_token(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }

    let suffix_chars: Vec<char> = token.chars().rev().take(8).collect();
    let suffix: String = suffix_chars.into_iter().rev().collect();
    Some(format!("****{suffix}"))
}
