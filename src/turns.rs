/*
Turn execution and event recording
agent stdout/stdin
  -> runtime-specific parser
  -> TurnRecorder callbacks
  -> SessionRecorder
  -> AppState mutations
  -> persistence + SSE deltas
The REPL path and the server path share the same recorder vocabulary so each
agent integration only needs one normalization layer.
*/

/// Runs a blocking turn for the selected agent.
fn run_turn_blocking(config: TurnConfig, recorder: &mut dyn TurnRecorder) -> Result<String> {
    match config.agent {
        Agent::Codex => run_codex_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            config
                .codex_sandbox_mode
                .unwrap_or_else(default_codex_sandbox_mode),
            config
                .codex_approval_policy
                .unwrap_or_else(default_codex_approval_policy),
            config
                .codex_reasoning_effort
                .unwrap_or_else(default_codex_reasoning_effort),
            &config.prompt,
            recorder,
        ),
        Agent::Claude => run_claude_turn(
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            config
                .claude_approval_mode
                .unwrap_or_else(default_claude_approval_mode),
            config.claude_effort.unwrap_or_else(default_claude_effort),
            &config.prompt,
            recorder,
        ),
        Agent::Cursor => run_acp_turn(
            AcpAgent::Cursor,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            &config.prompt,
            recorder,
        ),
        Agent::Gemini => run_acp_turn(
            AcpAgent::Gemini,
            &config.cwd,
            config.external_session_id.as_deref(),
            &config.model,
            &config.prompt,
            recorder,
        ),
    }
}


/// Runs ACP turn.
fn run_acp_turn(
    agent: AcpAgent,
    _cwd: &str,
    _external_session_id: Option<&str>,
    _model: &str,
    _prompt: &str,
    _recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    bail!("{} REPL mode is not supported yet", agent.label())
}

/// Runs Codex turn.
fn run_codex_turn(
    cwd: &str,
    external_session_id: Option<&str>,
    model: &str,
    sandbox_mode: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    reasoning_effort: CodexReasoningEffort,
    prompt: &str,
    recorder: &mut dyn TurnRecorder,
) -> Result<String> {
    let cwd = normalize_local_user_facing_path(cwd);
    let codex_home = prepare_termal_codex_home(&cwd, "repl")?;
    let mut command = codex_command()?;
    command
        .arg("app-server")
        .args(["--listen", "stdio://"])
        .env("CODEX_HOME", &codex_home);

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start Codex app-server")?;

    let mut child_stdin = child
        .stdin
        .take()
        .context("failed to capture Codex app-server stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture Codex app-server stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture Codex app-server stderr")?;
    let process = Arc::new(SharedChild::new(child).context("failed to share Codex child")?);
    let stdout_rx = spawn_repl_codex_stdout_reader(stdout);

    let stderr_thread = std::thread::spawn(move || -> Vec<String> {
        let reader = BufReader::new(stderr);
        reader.lines().map_while(Result::ok).collect()
    });

    let mut repl_state = ReplCodexSessionState {
        resolved_session_id: external_session_id.map(str::to_owned),
        ..ReplCodexSessionState::default()
    };
    let run_result = (|| -> Result<String> {
        send_repl_codex_json_rpc_request(
            &mut child_stdin,
            &stdout_rx,
            &mut repl_state,
            recorder,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "termal",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
            Duration::from_secs(15),
        )?;
        write_codex_json_rpc_message(
            &mut child_stdin,
            &json_rpc_notification_message("initialized"),
        )?;

        let thread_result = match external_session_id {
            Some(thread_id) => send_repl_codex_json_rpc_request(
                &mut child_stdin,
                &stdout_rx,
                &mut repl_state,
                recorder,
                "thread/resume",
                json!({
                    "threadId": thread_id,
                    "cwd": cwd.as_str(),
                    "model": model,
                    "sandbox": sandbox_mode.as_cli_value(),
                    "approvalPolicy": approval_policy.as_cli_value(),
                }),
                Duration::from_secs(30),
            )?,
            None => send_repl_codex_json_rpc_request(
                &mut child_stdin,
                &stdout_rx,
                &mut repl_state,
                recorder,
                "thread/start",
                json!({
                    "cwd": cwd.as_str(),
                    "model": model,
                    "sandbox": sandbox_mode.as_cli_value(),
                    "approvalPolicy": approval_policy.as_cli_value(),
                    "personality": "pragmatic",
                }),
                Duration::from_secs(30),
            )?,
        };
        if let Some(thread_id) = thread_result.pointer("/thread/id").and_then(Value::as_str) {
            remember_repl_codex_thread_id(&mut repl_state, recorder, thread_id)?;
        }

        let resolved_thread_id = repl_state
            .resolved_session_id
            .clone()
            .ok_or_else(|| anyhow!("Codex did not return a thread id"))?;

        repl_state.turn_completed = false;
        repl_state.turn_failed = None;
        let turn_result = send_repl_codex_json_rpc_request(
            &mut child_stdin,
            &stdout_rx,
            &mut repl_state,
            recorder,
            "turn/start",
            json!({
                "threadId": resolved_thread_id,
                "cwd": cwd.as_str(),
                "approvalPolicy": approval_policy.as_cli_value(),
                "effort": reasoning_effort.as_api_value(),
                "model": model,
                "sandboxPolicy": codex_sandbox_policy_value(sandbox_mode),
                "input": codex_user_input_items(prompt, &[]),
            }),
            Duration::from_secs(30),
        )?;
        repl_state.current_turn_id = turn_result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(repl_state.current_turn_id.clone());

        pump_repl_codex_turn(&stdout_rx, &mut child_stdin, &mut repl_state, recorder)?;
        recorder.finish_streaming_text()?;
        if let Some(detail) = repl_state.turn_failed.as_deref() {
            bail!(detail.to_owned());
        }

        Ok(resolved_thread_id)
    })();

    let _ = recorder.finish_streaming_text();
    drop(child_stdin);
    let (status, forced_shutdown) = shutdown_repl_codex_process(&process)?;
    let stderr_lines = stderr_thread.join().unwrap_or_default();
    let stderr_output = stderr_lines.join("\n");

    match run_result {
        Ok(session_id) => {
            if !status.success() && !forced_shutdown {
                if stderr_output.trim().is_empty() {
                    bail!("Codex app-server exited with status {status}");
                } else {
                    bail!("Codex app-server exited with status {status}: {stderr_output}");
                }
            }
            Ok(session_id)
        }
        Err(err) => {
            if !status.success() && !forced_shutdown && !stderr_output.trim().is_empty() {
                Err(err.context(format!(
                    "Codex app-server exited with status {status}: {stderr_output}"
                )))
            } else {
                Err(err)
            }
        }
    }
}


/// Returns the default Codex sandbox mode.
fn default_codex_sandbox_mode() -> CodexSandboxMode {
    match std::env::var("TERMAL_CODEX_SANDBOX").ok().as_deref() {
        Some("read-only") => CodexSandboxMode::ReadOnly,
        Some("danger-full-access") => CodexSandboxMode::DangerFullAccess,
        _ => CodexSandboxMode::WorkspaceWrite,
    }
}

/// Returns the default Codex approval policy.
fn default_codex_approval_policy() -> CodexApprovalPolicy {
    match std::env::var("TERMAL_CODEX_APPROVAL").ok().as_deref() {
        Some("untrusted") => CodexApprovalPolicy::Untrusted,
        Some("on-request") => CodexApprovalPolicy::OnRequest,
        Some("on-failure") => CodexApprovalPolicy::OnFailure,
        _ => CodexApprovalPolicy::Never,
    }
}

/// Returns the default Codex reasoning effort.
fn default_codex_reasoning_effort() -> CodexReasoningEffort {
    match std::env::var("TERMAL_CODEX_REASONING_EFFORT")
        .ok()
        .as_deref()
    {
        Some("none") => CodexReasoningEffort::None,
        Some("minimal") => CodexReasoningEffort::Minimal,
        Some("low") => CodexReasoningEffort::Low,
        Some("high") => CodexReasoningEffort::High,
        Some("xhigh") => CodexReasoningEffort::XHigh,
        _ => CodexReasoningEffort::Medium,
    }
}

/// Returns the default Claude approval mode.
fn default_claude_approval_mode() -> ClaudeApprovalMode {
    ClaudeApprovalMode::Ask
}

/// Returns the default Claude effort.
fn default_claude_effort() -> ClaudeEffortLevel {
    ClaudeEffortLevel::Default
}

const LOCAL_REMOTE_ID: &str = "local";
const LOCAL_REMOTE_NAME: &str = "Local";
const DEFAULT_SSH_REMOTE_PORT: u16 = 22;

/// Returns the default local remote ID.
fn default_local_remote_id() -> String {
    LOCAL_REMOTE_ID.to_owned()
}

/// Returns the default remote enabled.
fn default_remote_enabled() -> bool {
    true
}

/// Returns the default remote configs.
fn default_remote_configs() -> Vec<RemoteConfig> {
    vec![RemoteConfig::local()]
}

/// Defines the remote transport variants.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum RemoteTransport {
    Local,
    Ssh,
}

/// Holds remote configuration.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConfig {
    id: String,
    name: String,
    transport: RemoteTransport,
    #[serde(default = "default_remote_enabled")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

impl RemoteConfig {
    /// Builds the local default value.
    fn local() -> Self {
        Self {
            id: default_local_remote_id(),
            name: LOCAL_REMOTE_NAME.to_owned(),
            transport: RemoteTransport::Local,
            enabled: true,
            host: None,
            port: None,
            user: None,
        }
    }
}

/// Represents app preferences.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppPreferences {
    #[serde(default = "default_codex_reasoning_effort")]
    default_codex_reasoning_effort: CodexReasoningEffort,
    #[serde(default = "default_claude_approval_mode")]
    default_claude_approval_mode: ClaudeApprovalMode,
    #[serde(default = "default_claude_effort")]
    default_claude_effort: ClaudeEffortLevel,
    #[serde(default = "default_remote_configs")]
    remotes: Vec<RemoteConfig>,
}

impl Default for AppPreferences {
    /// Builds the default value.
    fn default() -> Self {
        Self {
            default_codex_reasoning_effort: default_codex_reasoning_effort(),
            default_claude_approval_mode: default_claude_approval_mode(),
            default_claude_effort: default_claude_effort(),
            remotes: default_remote_configs(),
        }
    }
}

/// Returns the default cursor mode.
fn default_cursor_mode() -> CursorMode {
    CursorMode::Agent
}

/// Returns the default Gemini approval mode.
fn default_gemini_approval_mode() -> GeminiApprovalMode {
    GeminiApprovalMode::Default
}

/// Handles log unhandled Codex event.
fn log_unhandled_codex_event(context: &str, message: &Value) {
    eprintln!("codex diagnostic> {context}: {message}");
}


/// Handles flatten structured patch.
fn flatten_structured_patch(patches: &[Value]) -> String {
    patches
        .iter()
        .filter_map(|patch| patch.get("lines").and_then(Value::as_array))
        .flat_map(|lines| lines.iter())
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Handles fallback file diff.
fn fallback_file_diff(original: &str, updated: &str) -> String {
    let mut lines = Vec::new();
    for line in original.lines() {
        lines.push(format!("-{line}"));
    }
    for line in updated.lines() {
        lines.push(format!("+{line}"));
    }
    lines.join("\n")
}

/// Handles short file name.
fn short_file_name(file_path: &str) -> &str {
    file_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(file_path)
}

/// Summarizes error.
fn summarize_error(value: &Value) -> String {
    summarize_structured_error(value).unwrap_or_else(|| {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    })
}

/// Summarizes structured error.
fn summarize_structured_error(value: &Value) -> Option<String> {
    summarize_retryable_connectivity_error(value)
        .or_else(|| summarize_error_fields(value))
        .or_else(|| value.get("error").and_then(summarize_error_fields))
        .or_else(|| {
            value
                .get("result")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|result| !result.is_empty())
                .map(str::to_owned)
        })
}

/// Summarizes error fields.
fn summarize_error_fields(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message");
    let detail = trimmed_string_field(value, "additionalDetails")
        .or_else(|| trimmed_string_field(value, "detail"))
        .or_else(|| trimmed_string_field(value, "details"));

    match (message, detail) {
        (Some(message), Some(detail)) if contains_ignore_ascii_case(message, detail) => {
            Some(message.to_owned())
        }
        (Some(message), Some(detail)) if contains_ignore_ascii_case(detail, message) => {
            Some(detail.to_owned())
        }
        (Some(message), Some(detail)) => Some(format!("{message} {detail}")),
        (Some(message), None) => Some(message.to_owned()),
        (None, Some(detail)) => Some(detail.to_owned()),
        (None, None) => None,
    }
}

/// Summarizes retryable connectivity error.
fn summarize_retryable_connectivity_error(value: &Value) -> Option<String> {
    if !is_retryable_connectivity_error(value) {
        return None;
    }

    let mut summary = "Connection dropped before the response finished.".to_owned();
    if let Some(retry_status) = summarize_retry_status(value) {
        summary.push(' ');
        summary.push_str(&retry_status);
    } else {
        summary.push_str(" Retrying automatically.");
    }

    Some(summary)
}

/// Returns whether retryable connectivity error.
fn is_retryable_connectivity_error(value: &Value) -> bool {
    codex_error_will_retry(value) && has_connectivity_marker(value)
}

/// Handles Codex error will retry.
fn codex_error_will_retry(value: &Value) -> bool {
    value
        .get("willRetry")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("error")
            .and_then(|error| error.get("willRetry"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Returns whether connectivity marker.
fn has_connectivity_marker(value: &Value) -> bool {
    value
        .pointer("/error/codexErrorInfo/responseStreamDisconnected")
        .is_some_and(|marker| !marker.is_null())
        || [
            trimmed_string_field(value, "message"),
            trimmed_string_field(value, "additionalDetails"),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "message")),
            value
                .get("error")
                .and_then(|error| trimmed_string_field(error, "additionalDetails")),
        ]
        .into_iter()
        .flatten()
        .any(is_connectivity_text)
}

/// Summarizes retry status.
fn summarize_retry_status(value: &Value) -> Option<String> {
    let message = trimmed_string_field(value, "message").or_else(|| {
        value
            .get("error")
            .and_then(|error| trimmed_string_field(error, "message"))
    })?;

    let counts = message
        .strip_prefix("Reconnecting...")
        .or_else(|| message.strip_prefix("Reconnecting…"))
        .map(str::trim);

    let Some(counts) = counts else {
        return Some("Retrying automatically.".to_owned());
    };

    let Some((current, total)) = counts.split_once('/') else {
        return Some("Retrying automatically.".to_owned());
    };

    let current = current.trim().parse::<usize>().ok()?;
    let total = total.trim().parse::<usize>().ok()?;
    Some(format!(
        "Retrying automatically (attempt {current} of {total})."
    ))
}

/// Handles trimmed string field.
fn trimmed_string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
}

/// Returns whether ignore ascii case.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Returns whether connectivity text.
fn is_connectivity_text(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    normalized.contains("stream disconnected before completion")
        || normalized.contains("websocket closed by server before response.completed")
        || normalized.contains("response stream disconnected")
        || normalized.contains("connection dropped")
        || normalized.contains("reconnecting")
}

/// Builds preview.
fn make_preview(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let compact = first_line.replace('\t', " ");
    let compact = compact.trim();
    if compact.is_empty() {
        return "Waiting for activity.".to_owned();
    }

    const LIMIT: usize = 88;
    let mut preview = compact.chars().take(LIMIT).collect::<String>();
    if compact.chars().count() > LIMIT {
        preview.push_str("...");
    }
    preview
}

/// Handles image attachment summary.
fn image_attachment_summary(count: usize) -> String {
    match count {
        0 => "Waiting for activity.".to_owned(),
        1 => "1 image attached".to_owned(),
        count => format!("{count} images attached"),
    }
}

/// Handles prompt preview text.
fn prompt_preview_text(text: &str, attachments: &[MessageImageAttachment]) -> String {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        return make_preview(trimmed);
    }

    make_preview(&image_attachment_summary(attachments.len()))
}

/// Handles shell language.
fn shell_language() -> &'static str {
    "bash"
}

/// Infers language from path.
fn infer_language_from_path(path: &FsPath) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match file_name.as_str() {
        "dockerfile" => return Some("dockerfile"),
        "makefile" => return Some("makefile"),
        _ => {}
    }

    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "bash" | "sh" | "zsh" => Some("bash"),
        "cjs" | "js" | "jsx" | "mjs" => Some("javascript"),
        "css" => Some("css"),
        "dart" => Some("dart"),
        "go" => Some("go"),
        "htm" | "html" | "svg" | "xml" => Some("xml"),
        "ini" | "toml" => Some("ini"),
        "json" => Some("json"),
        "md" | "mdx" => Some("markdown"),
        "mts" | "ts" | "tsx" => Some("typescript"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "sql" => Some("sql"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

/// Infers command output language.
fn infer_command_output_language(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("git diff")
        || normalized
            .split(command_token_separator)
            .any(|token| token == "diff" || token == "patch")
    {
        return Some("diff");
    }

    if !normalized
        .split(command_token_separator)
        .any(is_file_viewer_command)
    {
        return None;
    }

    command
        .split(command_token_separator)
        .map(clean_command_path_hint)
        .rev()
        .find_map(|candidate| infer_language_from_path(FsPath::new(candidate)))
}

/// Handles command token separator.
fn command_token_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '`' | '|' | '&' | ';')
}

/// Returns whether file viewer command.
fn is_file_viewer_command(token: &str) -> bool {
    matches!(
        token,
        "bat" | "cat" | "head" | "less" | "more" | "sed" | "tail"
    )
}

/// Cleans command path hint.
fn clean_command_path_hint(token: &str) -> &str {
    let trimmed = token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
        )
    });

    trimmed
        .rsplit_once('=')
        .map(|(_, value)| value)
        .unwrap_or(trimmed)
        .trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
            )
        })
}

/// Handles Codex user input items.
fn codex_user_input_items(prompt: &str, attachments: &[PromptImageAttachment]) -> Vec<Value> {
    let mut input = Vec::with_capacity(attachments.len() + usize::from(!prompt.is_empty()));

    if !prompt.is_empty() {
        input.push(json!({
            "type": "text",
            "text": prompt,
        }));
    }

    input.extend(attachments.iter().map(|attachment| {
        json!({
            "type": "image",
            "url": codex_image_data_url(attachment),
        })
    }));

    input
}

/// Handles Codex image data URL.
fn codex_image_data_url(attachment: &PromptImageAttachment) -> String {
    format!(
        "data:{};base64,{}",
        attachment.metadata.media_type, attachment.data
    )
}

/// Parses prompt image attachments.
fn parse_prompt_image_attachments(
    requests: &[SendMessageAttachmentRequest],
) -> std::result::Result<Vec<PromptImageAttachment>, ApiError> {
    requests
        .iter()
        .enumerate()
        .map(|(index, request)| parse_prompt_image_attachment(index, request))
        .collect()
}

/// Parses prompt image attachment.
fn parse_prompt_image_attachment(
    index: usize,
    request: &SendMessageAttachmentRequest,
) -> std::result::Result<PromptImageAttachment, ApiError> {
    let media_type = request.media_type.trim().to_ascii_lowercase();
    if !matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err(ApiError::bad_request(format!(
            "unsupported image attachment type `{media_type}`"
        )));
    }

    let data = request.data.trim();
    if data.is_empty() {
        return Err(ApiError::bad_request(
            "image attachment data cannot be empty",
        ));
    }

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| ApiError::bad_request("image attachment data is not valid base64"))?;
    if decoded.len() > MAX_IMAGE_ATTACHMENT_BYTES {
        return Err(ApiError::bad_request(format!(
            "image attachment exceeds the {} MB limit",
            MAX_IMAGE_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }

    let file_name = request
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_attachment_file_name)
        .unwrap_or_else(|| default_attachment_file_name(index, &media_type));

    Ok(PromptImageAttachment {
        data: data.to_owned(),
        metadata: MessageImageAttachment {
            byte_size: decoded.len(),
            file_name,
            media_type,
        },
    })
}

/// Returns the default attachment file name.
fn default_attachment_file_name(index: usize, media_type: &str) -> String {
    let extension = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "img",
    };

    format!("pasted-image-{}.{}", index + 1, extension)
}

/// Sanitizes attachment file name.
fn sanitize_attachment_file_name(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|character| match character {
            '/' | '\\' | '\0' => '-',
            other => other,
        })
        .collect::<String>();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "pasted-image".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// Stamps now.
fn stamp_now() -> String {
    Local::now().format("%H:%M:%S").to_string()
}
