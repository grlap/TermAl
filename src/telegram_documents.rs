/*
Telegram Markdown document API.

Owns: `POST /api/sessions/{id}/telegram/documents`, which sends one Markdown
file named by a session to the operator's linked Telegram chat. It owns the
request/response wire types, the typed error envelope
(`{ "error": ..., "code": ... }`), the session's allowed roots (its workdir
and, for a local project session, the project root), the Telegram link checks
(token saved, relay enabled, chat linked), and the orchestration of admission
and upload.

Deliberately does not own: file admission rules (`telegram_document_files.rs`),
the multipart upload (`telegram_document_upload.rs`), Telegram settings or the
relay loop (`telegram_settings.rs`, `telegram_runtime.rs`). Nothing here runs
on its own: a document is sent only by an explicit call to this route.

Secrets: the bot token is read from the OS credential store into
`TelegramDocumentLink`, which has no Debug or Serialize, and goes only to the
in-process HTTP client. Every error that may carry Telegram transport text is
passed through `sanitize_telegram_log_detail` before it is logged or returned.

Provenance: new module; nothing was moved here from another file.
*/

/// Telegram's caption limit, counted in UTF-16 units as Telegram counts it.
const TELEGRAM_DOCUMENT_CAPTION_MAX_UTF16_UNITS: usize = 1024;
/// Request timeout for one upload: `TelegramApiClient::new` adds 10 seconds.
const TELEGRAM_DOCUMENT_UPLOAD_TIMEOUT_SECS: u64 = 20;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendTelegramDocumentRequest {
    path: String,
    #[serde(default)]
    caption: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SendTelegramDocumentResponse {
    message_id: i64,
    file_name: String,
    path: String,
    size_bytes: u64,
}

#[derive(Debug)]
enum TelegramDocumentError {
    InvalidRequest(String),
    SessionNotFound,
    SessionRefused {
        status: StatusCode,
        message: String,
    },
    NotConfigured,
    NotConnected(String),
    PathOutsideAllowedRoots(String),
    FileNotFound(String),
    NotMarkdown(String),
    EmptyFile(String),
    TooLarge {
        size_bytes: u64,
    },
    TelegramApi {
        error_code: Option<i64>,
        message: String,
    },
    Internal(String),
}

impl TelegramDocumentError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::SessionNotFound => "session_not_found",
            Self::SessionRefused { .. } => "session_refused",
            Self::NotConfigured => "telegram_not_configured",
            Self::NotConnected(_) => "telegram_not_connected",
            Self::PathOutsideAllowedRoots(_) => "path_outside_allowed_roots",
            Self::FileNotFound(_) => "file_not_found",
            Self::NotMarkdown(_) => "not_markdown",
            Self::EmptyFile(_) => "file_empty",
            Self::TooLarge { .. } => "file_too_large",
            Self::TelegramApi { .. } => "telegram_api_error",
            Self::Internal(_) => "internal_error",
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) | Self::EmptyFile(_) => StatusCode::BAD_REQUEST,
            Self::SessionNotFound | Self::FileNotFound(_) => StatusCode::NOT_FOUND,
            Self::SessionRefused { status, .. } => *status,
            Self::NotConfigured | Self::NotConnected(_) => StatusCode::CONFLICT,
            Self::PathOutsideAllowedRoots(_) => StatusCode::FORBIDDEN,
            Self::NotMarkdown(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TelegramApi { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::InvalidRequest(message)
            | Self::NotConnected(message)
            | Self::PathOutsideAllowedRoots(message)
            | Self::FileNotFound(message)
            | Self::NotMarkdown(message)
            | Self::EmptyFile(message)
            | Self::Internal(message) => message.clone(),
            Self::SessionRefused { message, .. } | Self::TelegramApi { message, .. } => {
                message.clone()
            }
            Self::SessionNotFound => "session not found".to_owned(),
            Self::NotConfigured => {
                "Telegram is not configured: save a bot token in Settings -> Telegram".to_owned()
            }
            Self::TooLarge { size_bytes } => format!(
                "file is {size_bytes} bytes; the limit is {TELEGRAM_DOCUMENT_MAX_BYTES} bytes"
            ),
        }
    }
}

impl IntoResponse for TelegramDocumentError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": self.message(),
            "code": self.code(),
        });
        match &self {
            Self::TooLarge { size_bytes } => {
                body["sizeBytes"] = json!(size_bytes);
                body["maxBytes"] = json!(TELEGRAM_DOCUMENT_MAX_BYTES);
            }
            Self::TelegramApi {
                error_code: Some(error_code),
                ..
            } => {
                body["telegramErrorCode"] = json!(error_code);
            }
            _ => {}
        }
        (self.status(), Json(body)).into_response()
    }
}

/// The saved bot token and linked chat. Deliberately not Debug or Serialize
/// so the token cannot be printed or returned by accident.
struct TelegramDocumentLink {
    bot_token: String,
    chat_id: i64,
}

/// Canonical, normalized roots a session may send files from.
struct TelegramDocumentSessionRoots {
    workdir: PathBuf,
    allowed: Vec<PathBuf>,
}

impl AppState {
    /// Sends one Markdown file from the session's allowed roots to the linked
    /// Telegram chat. `connect` builds the sender from the saved bot token;
    /// production passes `connect_telegram_document_sender`, tests a fake.
    fn send_telegram_markdown_document<S: TelegramDocumentSender>(
        &self,
        session_id: &str,
        request: SendTelegramDocumentRequest,
        connect: impl FnOnce(&str) -> Result<S>,
    ) -> Result<SendTelegramDocumentResponse, TelegramDocumentError> {
        self.send_telegram_markdown_document_with_admission(
            session_id,
            request,
            connect,
            admit_telegram_markdown_document,
        )
    }

    // Admission is a per-call dependency so deterministic filesystem-race tests
    // exercise this same orchestration without a global hook or a real sender.
    fn send_telegram_markdown_document_with_admission<S: TelegramDocumentSender>(
        &self,
        session_id: &str,
        request: SendTelegramDocumentRequest,
        connect: impl FnOnce(&str) -> Result<S>,
        admit: impl FnOnce(
            &str,
            &FsPath,
            &[PathBuf],
        ) -> Result<AdmittedTelegramDocument, TelegramDocumentError>,
    ) -> Result<SendTelegramDocumentResponse, TelegramDocumentError> {
        let caption = normalize_telegram_document_caption(request.caption)?;
        let roots = self.telegram_document_session_roots(session_id)?;
        let link = self.telegram_document_link()?;
        let document = admit(&request.path, &roots.workdir, &roots.allowed)?;

        let sender = connect(&link.bot_token).map_err(|err| {
            TelegramDocumentError::Internal(sanitize_telegram_log_detail(&format!(
                "failed to build the Telegram client: {err:#}"
            )))
        })?;
        let upload = TelegramDocumentUpload {
            file_name: &document.file_name,
            bytes: &document.bytes,
            caption: caption.as_deref(),
        };
        let message_id = sender
            .send_document(link.chat_id, &upload)
            .map_err(telegram_document_api_error)?;

        Ok(SendTelegramDocumentResponse {
            message_id,
            file_name: document.file_name,
            path: document.canonical_path.to_string_lossy().into_owned(),
            size_bytes: document.bytes.len() as u64,
        })
    }

    fn telegram_document_session_roots(
        &self,
        session_id: &str,
    ) -> Result<TelegramDocumentSessionRoots, TelegramDocumentError> {
        let (workdir, project_root) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or(TelegramDocumentError::SessionNotFound)?;
            let record = &inner.sessions[index];
            if record.hidden {
                return Err(TelegramDocumentError::SessionNotFound);
            }
            if !record.is_local_session() {
                return Err(TelegramDocumentError::SessionRefused {
                    status: StatusCode::BAD_REQUEST,
                    message: "only local sessions can send Telegram documents".to_owned(),
                });
            }
            let project_root = match record
                .session
                .project_id
                .as_deref()
                .and_then(|project_id| inner.find_project(project_id))
            {
                Some(project) if project.remote_id != LOCAL_REMOTE_ID => {
                    return Err(TelegramDocumentError::SessionRefused {
                        status: StatusCode::BAD_REQUEST,
                        message: format!(
                            "project `{}` runs on remote `{}`; only local sessions can send Telegram documents",
                            project.name, project.remote_id
                        ),
                    });
                }
                Some(project) => Some(project.root_path.clone()),
                None => None,
            };
            (record.session.workdir.clone(), project_root)
        };

        self.ensure_read_only_delegation_allows_session_write_action(
            Some(session_id),
            "Sending a Telegram document",
        )
        .map_err(|err| TelegramDocumentError::SessionRefused {
            status: err.status,
            message: err.message,
        })?;

        let workdir = fs::canonicalize(&workdir)
            .map(|path| normalize_user_facing_path(&path))
            .map_err(|err| TelegramDocumentError::SessionRefused {
                status: StatusCode::CONFLICT,
                message: format!("session workdir `{workdir}` cannot be resolved: {err}"),
            })?;
        let mut allowed = vec![workdir.clone()];
        // A project root that no longer resolves adds nothing; the workdir
        // still bounds the session.
        if let Some(project_root) = project_root
            .and_then(|root| fs::canonicalize(root).ok())
            .map(|path| normalize_user_facing_path(&path))
            && !allowed.contains(&project_root)
        {
            allowed.push(project_root);
        }
        Ok(TelegramDocumentSessionRoots { workdir, allowed })
    }

    fn telegram_document_link(&self) -> Result<TelegramDocumentLink, TelegramDocumentError> {
        let _guard = telegram_settings_file_guard();
        let runtime = self
            .load_telegram_bot_file()
            .map_err(|err| TelegramDocumentError::Internal(err.message))?;
        let config = self.telegram_config_from_state();
        let bot_token = self
            .saved_telegram_bot_token()
            .map_err(|err| TelegramDocumentError::Internal(err.message))?
            .filter(|token| !token.trim().is_empty())
            .ok_or(TelegramDocumentError::NotConfigured)?;
        if !config.enabled {
            return Err(TelegramDocumentError::NotConnected(
                "the Telegram relay is disabled; enable it in Settings -> Telegram".to_owned(),
            ));
        }
        let chat_id = runtime.chat_id.ok_or_else(|| {
            TelegramDocumentError::NotConnected(
                "no Telegram chat is linked yet; open the bot in Telegram and send /start"
                    .to_owned(),
            )
        })?;
        Ok(TelegramDocumentLink { bot_token, chat_id })
    }
}

fn normalize_telegram_document_caption(
    caption: Option<String>,
) -> Result<Option<String>, TelegramDocumentError> {
    let Some(caption) = caption
        .map(|caption| caption.trim().to_owned())
        .filter(|caption| !caption.is_empty())
    else {
        return Ok(None);
    };
    if caption.encode_utf16().count() > TELEGRAM_DOCUMENT_CAPTION_MAX_UTF16_UNITS {
        return Err(TelegramDocumentError::InvalidRequest(format!(
            "caption must be at most {TELEGRAM_DOCUMENT_CAPTION_MAX_UTF16_UNITS} characters"
        )));
    }
    Ok(Some(caption))
}

fn telegram_document_api_error(err: anyhow::Error) -> TelegramDocumentError {
    let error_code = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<TelegramApiError>())
        .and_then(|api_error| api_error.error_code);
    TelegramDocumentError::TelegramApi {
        error_code,
        message: sanitize_telegram_log_detail(&format!(
            "Telegram did not accept the document: {err:#}"
        )),
    }
}

#[cfg(not(test))]
fn connect_telegram_document_sender(bot_token: &str) -> Result<TelegramApiClient> {
    TelegramApiClient::new(bot_token, TELEGRAM_DOCUMENT_UPLOAD_TIMEOUT_SECS)
}

/// Tests never reach Telegram: the route's real client is replaced by a
/// refusal, and success paths use a fake `TelegramDocumentSender`.
#[cfg(test)]
fn connect_telegram_document_sender(_bot_token: &str) -> Result<TelegramApiClient> {
    let _ = TELEGRAM_DOCUMENT_UPLOAD_TIMEOUT_SECS;
    bail!("the real Telegram client is disabled in tests")
}

async fn send_session_telegram_document(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<SendTelegramDocumentRequest>, JsonRejection>,
) -> Response {
    send_session_telegram_document_with_sender(
        AxumPath(session_id),
        State(state),
        request,
        connect_telegram_document_sender,
    )
    .await
}

async fn send_session_telegram_document_with_sender<S: TelegramDocumentSender + Send + 'static>(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<SendTelegramDocumentRequest>, JsonRejection>,
    connect: impl FnOnce(&str) -> Result<S> + Send + 'static,
) -> Response {
    let request = match request {
        Ok(Json(request)) => request,
        Err(rejection) => {
            return TelegramDocumentError::InvalidRequest(format!(
                "invalid Telegram document request JSON: {}",
                rejection.body_text()
            ))
            .into_response();
        }
    };
    let outcome = tokio::task::spawn_blocking(move || {
        state.send_telegram_markdown_document(&session_id, request, connect)
    })
    .await;
    match outcome {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => {
            if matches!(
                err,
                TelegramDocumentError::TelegramApi { .. } | TelegramDocumentError::Internal(_)
            ) {
                eprintln!(
                    "telegram documents> {}: {}",
                    err.code(),
                    sanitize_telegram_log_detail(&err.message())
                );
            }
            err.into_response()
        }
        Err(err) => {
            TelegramDocumentError::Internal(format!("blocking task failed: {err}")).into_response()
        }
    }
}
