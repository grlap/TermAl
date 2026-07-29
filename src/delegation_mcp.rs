const TERMAL_DELEGATION_MCP_SERVER_NAME: &str = "termal-delegation";
const TERMAL_DELEGATION_MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const TERMAL_DELEGATION_MCP_DEFAULT_WAIT_INTERVAL_MS: u64 = 1000;
const TERMAL_DELEGATION_MCP_DEFAULT_WAIT_TIMEOUT_MS: u64 = 300_000;
const TERMAL_DELEGATION_MCP_MAX_WAIT_TIMEOUT_MS: u64 = 1_800_000;
const TERMAL_DELEGATION_MCP_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Clause shape of the typed storage-busy rejection the backend emits before
/// a write transaction begins or after a failed SQLite commit that remains
/// rollback-safe. A message carries the structural no-commit guarantee only
/// when one of its `;`-separated clauses, trimmed, BOTH starts with `no ` and
/// ends with the suffix below — i.e. "no mailbox write was committed by this
/// operation, so retry the same request" or the board-store analog. Matching
/// the negation structurally (not just the instruction tail) keeps a
/// hypothetical positive phrasing like "a mailbox write was committed by
/// this operation, so retry the same request" from ever being classified
/// retryable; a machine-readable error code is the eventual replacement
/// (tm-uwx.7.4 cross-review finding 1).
const TERMAL_DELEGATION_NO_COMMIT_CLAUSE_PREFIX: &str = "no ";
const TERMAL_DELEGATION_NO_COMMIT_CLAUSE_SUFFIX: &str =
    " write was committed by this operation, so retry the same request";
/// Pure reads use a distinct exact clause: their pre-transaction rejection
/// attempted no mutation at all. Keep this machine-matched and fail-closed
/// alongside the structural no-commit write clause until typed error codes
/// replace both prose contracts.
const TERMAL_DELEGATION_SAFE_READ_RETRY_CLAUSE: &str =
    "no mutation was attempted by this read operation, so retry the same request";
/// Duplicate mailbox append requests can arrive while the original durable
/// row is waiting for its notification outcome to finalize. Replaying the
/// same idempotency-keyed request is safe even though the append itself
/// already committed, so this needs its own exact clause rather than the
/// structurally false no-commit wording.
const TERMAL_DELEGATION_DURABLE_APPEND_RETRY_CLAUSE: &str =
    "the original mailbox append is durable and replaying the same idempotency key is safe";
/// Total attempts for an explicitly opted-in safe-replay rejection: one
/// initial try plus up to four replays. Base delays double from 200ms
/// (200/400/800/1600) and are
/// then dephased per session by deterministic jitter (see
/// `safe_replay_retry_jitter_percent`) so peers released by the same
/// saturation event do not re-collide in lockstep. The WHOLE helper honors a
/// single `request_timeout` budget: each attempt's HTTP timeout shrinks to
/// the remaining budget, and no replay is attempted unless the remaining
/// budget can fund its delay plus
/// `TERMAL_DELEGATION_SAFE_REPLAY_MIN_REPLAY_BUDGET` of useful request time —
/// the caller then sees the last typed rejection rather than a
/// budget-starved transport error. A cumulative end-to-end deadline across a
/// whole multi-request operation remains tm-p0u territory.
const TERMAL_DELEGATION_SAFE_REPLAY_RETRY_ATTEMPTS: u32 = 5;
const TERMAL_DELEGATION_SAFE_REPLAY_RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const TERMAL_DELEGATION_SAFE_REPLAY_MIN_REPLAY_BUDGET: Duration = Duration::from_secs(2);

fn parse_delegation_mcp_mode_args(
    args: impl Iterator<Item = String>,
) -> Result<(String, Option<String>)> {
    let mut parent_session_id: Option<String> = None;
    let mut base_url: Option<String> = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--parent-session-id" => {
                let value = args
                    .next()
                    .context("delegation-mcp requires a value after --parent-session-id")?;
                parent_session_id = Some(value);
            }
            "--base-url" => {
                let value = args
                    .next()
                    .context("delegation-mcp requires a value after --base-url")?;
                base_url = Some(value);
            }
            "--help" | "-h" => {
                bail!("usage: termal delegation-mcp --parent-session-id <id> [--base-url <url>]");
            }
            other => bail!("unknown delegation-mcp argument `{other}`"),
        }
    }
    let parent_session_id = parent_session_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .context("delegation-mcp requires --parent-session-id <id>")?;
    Ok((parent_session_id, base_url))
}

fn default_termal_http_base_url() -> String {
    if let Ok(value) = std::env::var("TERMAL_BASE_URL") {
        let value = value.trim();
        if !value.is_empty() {
            return value.trim_end_matches('/').to_owned();
        }
    }
    let port = std::env::var("TERMAL_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8787);
    format!("http://127.0.0.1:{port}")
}

fn normalize_termal_http_base_url(base_url: impl AsRef<str>) -> String {
    let base_url = base_url.as_ref().trim().trim_end_matches('/');
    if base_url.is_empty() {
        default_termal_http_base_url()
    } else {
        base_url.to_owned()
    }
}

fn termal_delegation_mcp_args(parent_session_id: &str, base_url: &str) -> Vec<String> {
    vec![
        "delegation-mcp".to_owned(),
        "--parent-session-id".to_owned(),
        parent_session_id.to_owned(),
        "--base-url".to_owned(),
        normalize_termal_http_base_url(base_url),
    ]
}

fn termal_delegation_mcp_stdio_config_with_command(
    command: &str,
    parent_session_id: &str,
    base_url: &str,
) -> Value {
    json!({
        "command": command,
        "args": termal_delegation_mcp_args(parent_session_id, base_url),
        "env": {},
    })
}

fn termal_delegation_mcp_current_exe() -> Result<String> {
    Ok(std::env::current_exe()
        .context("failed to resolve TermAl executable for delegation MCP")?
        .to_string_lossy()
        .into_owned())
}

fn termal_delegation_mcp_claude_config_json_with_command(
    command: &str,
    parent_session_id: &str,
    base_url: &str,
) -> String {
    json!({
        "mcpServers": {
            TERMAL_DELEGATION_MCP_SERVER_NAME: termal_delegation_mcp_stdio_config_with_command(
                command,
                parent_session_id,
                base_url,
            ),
        },
    })
    .to_string()
}

fn termal_delegation_mcp_acp_servers_with_command(
    command: &str,
    parent_session_id: &str,
    base_url: &str,
) -> Value {
    let server =
        termal_delegation_mcp_stdio_config_with_command(command, parent_session_id, base_url);
    json!([{
        "name": TERMAL_DELEGATION_MCP_SERVER_NAME,
        "command": server.get("command").cloned().unwrap_or(Value::Null),
        "args": server.get("args").cloned().unwrap_or_else(|| json!([])),
        "env": server.get("env").cloned().unwrap_or_else(|| json!({})),
    }])
}

fn termal_delegation_mcp_codex_config_with_command(
    command: &str,
    parent_session_id: &str,
    base_url: &str,
) -> Value {
    let server =
        termal_delegation_mcp_stdio_config_with_command(command, parent_session_id, base_url);
    json!({
        "mcp_servers": {
            TERMAL_DELEGATION_MCP_SERVER_NAME: {
                "command": server.get("command").cloned().unwrap_or(Value::Null),
                "args": server.get("args").cloned().unwrap_or_else(|| json!([])),
                "env": server.get("env").cloned().unwrap_or_else(|| json!({})),
            },
        },
    })
}

impl AppState {
    fn set_local_http_base_url(&self, base_url: String) {
        *self
            .local_http_base_url
            .lock()
            .expect("local HTTP base URL mutex poisoned") =
            Some(normalize_termal_http_base_url(base_url));
    }

    fn local_http_base_url(&self) -> String {
        self.local_http_base_url
            .lock()
            .expect("local HTTP base URL mutex poisoned")
            .clone()
            .unwrap_or_else(default_termal_http_base_url)
    }

    fn termal_delegation_mcp_claude_config_json(&self, parent_session_id: &str) -> Result<String> {
        let command = termal_delegation_mcp_current_exe()?;
        Ok(termal_delegation_mcp_claude_config_json_with_command(
            &command,
            parent_session_id,
            &self.local_http_base_url(),
        ))
    }

    fn termal_delegation_mcp_acp_servers(&self, parent_session_id: &str) -> Result<Value> {
        let command = termal_delegation_mcp_current_exe()?;
        Ok(termal_delegation_mcp_acp_servers_with_command(
            &command,
            parent_session_id,
            &self.local_http_base_url(),
        ))
    }

    fn termal_delegation_mcp_codex_config(&self, parent_session_id: &str) -> Result<Value> {
        let command = termal_delegation_mcp_current_exe()?;
        Ok(termal_delegation_mcp_codex_config_with_command(
            &command,
            parent_session_id,
            &self.local_http_base_url(),
        ))
    }
}

struct TermalDelegationMcpBridge {
    parent_session_id: String,
    base_url: String,
    client: reqwest::blocking::Client,
    request_timeout: Duration,
    // A caller classified as a root by both `is_root_peer_session` link
    // sources cannot become a delegation child under the same session id.
    // Repair may restore a missing parent marker only for a session already
    // present in the durable delegation-child index. The conjunctive rule is
    // pinned by
    // `delegation_mcp_indexed_child_with_null_marker_is_not_a_root_peer`.
    // Revisit this lifetime cache before introducing root-to-child adoption,
    // conversion, or id reuse (tm-487).
    caller_is_delegation_child: OnceLock<bool>,
    // Sleep hook for explicit safe-replay retries. Production uses
    // `std::thread::sleep`; tests inject a recording no-op so retry cadence is
    // asserted without real waiting (no-flaky-tests law: no timing
    // assumptions, no wall-clock dependence).
    safe_replay_retry_sleeper: fn(Duration),
}

fn delegation_child_session_ids(state: &Value) -> HashSet<&str> {
    state
        .get("delegations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|delegation| delegation.get("childSessionId").and_then(Value::as_str))
        .filter(|session_id| !session_id.trim().is_empty())
        .collect()
}

fn is_root_peer_session(session: &Value, delegation_child_ids: &HashSet<&str>) -> bool {
    session
        .get("parentDelegationId")
        .map_or(true, Value::is_null)
        && session
            .get("id")
            .and_then(Value::as_str)
            .map_or(true, |session_id| !delegation_child_ids.contains(session_id))
}

impl TermalDelegationMcpBridge {
    fn new(parent_session_id: String, base_url: String) -> Result<Self> {
        Self::new_with_timeout(
            parent_session_id,
            base_url,
            TERMAL_DELEGATION_MCP_HTTP_TIMEOUT,
        )
    }

    fn new_with_timeout(
        parent_session_id: String,
        base_url: String,
        request_timeout: Duration,
    ) -> Result<Self> {
        let parent_session_id = required_path_identifier(
            Some(&Value::String(parent_session_id)),
            "delegation MCP parent session id",
        )?;
        Ok(Self {
            parent_session_id,
            base_url: normalize_termal_http_base_url(base_url),
            client: reqwest::blocking::Client::builder()
                .timeout(request_timeout)
                .build()
                .context("failed to build delegation MCP HTTP client")?,
            request_timeout,
            caller_is_delegation_child: OnceLock::new(),
            safe_replay_retry_sleeper: std::thread::sleep,
        })
    }

    /// Test-only injection point for the safe-replay sleep hook; keeps the
    /// production constructors byte-identical while letting tests assert exact
    /// backoff cadence without real waiting.
    #[cfg(test)]
    fn with_safe_replay_retry_sleeper(mut self, sleeper: fn(Duration)) -> Self {
        self.safe_replay_retry_sleeper = sleeper;
        self
    }

    fn handle_message(&self, message: Value) -> Result<Option<Value>> {
        if let Some(batch) = message.as_array() {
            let mut responses = Vec::new();
            for item in batch {
                if let Some(response) = self.handle_single_message(item.clone())? {
                    responses.push(response);
                }
            }
            return Ok((!responses.is_empty()).then_some(Value::Array(responses)));
        }
        self.handle_single_message(message)
    }

    fn handle_single_message(&self, message: Value) -> Result<Option<Value>> {
        let id = message.get("id").cloned();
        let Some(id_for_response) = id.clone() else {
            if message.get("method").and_then(Value::as_str) == Some("notifications/initialized") {
                return Ok(None);
            }
            return Ok(None);
        };
        let method = match message.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => {
                return Ok(Some(mcp_json_rpc_error(
                    id_for_response,
                    -32600,
                    "Invalid request",
                )));
            }
        };
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => Ok(mcp_initialize_result()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list_for_caller()),
            "tools/call" => self.handle_tool_call(params),
            "notifications/initialized" => return Ok(None),
            _ => {
                return Ok(Some(mcp_json_rpc_error(
                    id_for_response,
                    -32601,
                    &format!("method `{method}` is not supported"),
                )));
            }
        };
        Ok(Some(match result {
            Ok(result) => mcp_json_rpc_result(id_for_response, result),
            Err(err) => mcp_json_rpc_tool_error(id_for_response, err.to_string()),
        }))
    }

    fn handle_tool_call(&self, params: Value) -> Result<Value> {
        let name = required_string(params.get("name"), "tool name")?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // Peer/coordination tools (messaging, enumerating arbitrary sessions,
        // the project coordination board) are root-session only. A delegation
        // child (e.g. a read-only reviewer chewing on untrusted code) must not
        // reach them, so reject here as well as hiding them from tools/list
        // (tm-r0y; board scoping additionally enforced by the backend).
        if tool_requires_root_session(&name) && self.caller_is_delegation_child() {
            bail!(
                "`{name}` is not available to delegation-child sessions; peer and \
                 coordination tools are restricted to root sessions"
            );
        }
        let result = match name.as_str() {
            "termal_spawn_session" => self.tool_spawn_session(arguments),
            "termal_list_delegations" => self.tool_list_delegations(arguments),
            "termal_get_session_status" => self.tool_get_session_status(arguments),
            "termal_get_session_result" => self.tool_get_session_result(arguments),
            "termal_cancel_session" => self.tool_cancel_session(arguments),
            "termal_followup_session" => self.tool_followup_session(arguments),
            "termal_send_to_session" => self.tool_send_to_session(arguments),
            "termal_list_sessions" => self.tool_list_sessions(arguments),
            "termal_list_mailboxes" => self.tool_list_mailboxes(arguments),
            "termal_read_mailbox" => self.tool_read_mailbox(arguments),
            "termal_read_mailbox_message" => self.tool_read_mailbox_message(arguments),
            "termal_acknowledge_mailbox" => self.tool_acknowledge_mailbox(arguments),
            "termal_board_list" => self.tool_board_list(arguments),
            "termal_board_get" => self.tool_board_get(arguments),
            "termal_board_set" => self.tool_board_set(arguments),
            "termal_wait_delegations" => self.tool_wait_delegations(arguments),
            "termal_resume_after_delegations" => self.tool_resume_after_delegations(arguments),
            other => Err(anyhow!("unknown TermAl delegation MCP tool `{other}`")),
        }?;
        Ok(mcp_tool_text_result(&result, false))
    }

    fn tool_spawn_session(&self, arguments: Value) -> Result<Value> {
        let prompt = required_string(arguments.get("prompt"), "prompt")?;
        let cwd = optional_string(arguments.get("cwd"));
        let resolved_prompt = self.resolve_spawn_prompt_if_agent_command(&prompt, cwd.as_deref())?;
        let mut body = serde_json::Map::new();
        body.insert("prompt".to_owned(), Value::String(resolved_prompt.prompt));
        if !insert_optional_string(&mut body, "title", arguments.get("title")) {
            if let Some(title) = resolved_prompt.title {
                body.insert("title".to_owned(), Value::String(title));
            }
        }
        if let Some(cwd) = cwd {
            body.insert("cwd".to_owned(), Value::String(cwd));
        }
        insert_optional_string(&mut body, "agent", arguments.get("agent"));
        insert_optional_string(&mut body, "model", arguments.get("model"));
        body.insert(
            "mode".to_owned(),
            optional_string(arguments.get("mode"))
                .or(resolved_prompt.mode)
                .map(Value::String)
                .unwrap_or_else(|| Value::String("reviewer".to_owned())),
        );
        body.insert(
            "writePolicy".to_owned(),
            arguments
                .get("writePolicy")
                .map(|value| normalize_mcp_write_policy(Some(value)))
                .or(resolved_prompt.write_policy)
                .unwrap_or_else(|| normalize_mcp_write_policy(None)),
        );
        self.post_json(
            &format!("/api/sessions/{}/delegations", self.parent_session_id),
            &Value::Object(body),
        )
    }

    fn tool_list_delegations(&self, _arguments: Value) -> Result<Value> {
        self.get_json(&format!(
            "/api/sessions/{}/delegations",
            self.parent_session_id
        ))
    }

    fn resolve_spawn_prompt_if_agent_command(
        &self,
        prompt: &str,
        cwd: Option<&str>,
    ) -> Result<McpSpawnPrompt> {
        let Some(parsed) = parse_mcp_slash_command_prompt(prompt) else {
            return Ok(McpSpawnPrompt::literal(prompt));
        };
        let command_name =
            required_agent_command_name(Some(&Value::String(parsed.command_name.clone())))?;
        let resolved = match self.try_resolve_agent_command_for_spawn(&command_name, &parsed, cwd)? {
            Some(resolved) => resolved,
            None if cwd.is_none() => return Ok(McpSpawnPrompt::literal(prompt)),
            None => {
                if self
                    .try_resolve_agent_command_for_spawn(&command_name, &parsed, None)?
                    .is_some()
                {
                    bail!(
                        "agent command `{command_name}` was not found in requested cwd `{}`",
                        cwd.unwrap_or_default()
                    );
                }
                return Ok(McpSpawnPrompt::literal(prompt));
            }
        };
        let prompt = resolved
            .get("expandedPrompt")
            .or_else(|| resolved.get("visiblePrompt"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .with_context(|| {
                format!("agent command `{command_name}` resolved without prompt content")
            })?
            .to_owned();
        let title = resolved
            .pointer("/delegation/title")
            .or_else(|| resolved.get("title"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let mode = resolved
            .pointer("/delegation/mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let write_policy = resolved.pointer("/delegation/writePolicy").cloned();
        Ok(McpSpawnPrompt {
            prompt,
            title,
            mode,
            write_policy,
        })
    }

    fn try_resolve_agent_command_for_spawn(
        &self,
        command_name: &str,
        parsed: &McpSlashCommandPrompt,
        cwd: Option<&str>,
    ) -> Result<Option<Value>> {
        match self.resolve_agent_command_for_spawn(command_name, parsed, cwd) {
            Ok(resolved) => Ok(Some(resolved)),
            Err(err)
                if err
                    .downcast_ref::<TermalDelegationApiError>()
                    .is_some_and(TermalDelegationApiError::is_agent_command_not_found) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    fn resolve_agent_command_for_spawn(
        &self,
        command_name: &str,
        parsed: &McpSlashCommandPrompt,
        cwd: Option<&str>,
    ) -> Result<Value> {
        let mut body = serde_json::Map::new();
        if let Some(arguments) = parsed.arguments.as_deref() {
            body.insert("arguments".to_owned(), Value::String(arguments.to_owned()));
        }
        if let Some(note) = parsed.note.as_deref() {
            body.insert("note".to_owned(), Value::String(note.to_owned()));
        }
        if let Some(cwd) = cwd {
            body.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
        }
        body.insert("intent".to_owned(), Value::String("delegate".to_owned()));
        self.post_json(
            &format!(
                "/api/sessions/{}/agent-commands/{}/resolve",
                self.parent_session_id,
                encode_uri_component(command_name)
            ),
            &Value::Object(body),
        )
    }

    fn tool_get_session_status(&self, arguments: Value) -> Result<Value> {
        let delegation_id =
            required_path_identifier(arguments.get("delegationId"), "delegationId")?;
        self.get_json(&format!(
            "/api/sessions/{}/delegations/{}",
            self.parent_session_id, delegation_id
        ))
    }

    fn tool_get_session_result(&self, arguments: Value) -> Result<Value> {
        let delegation_id =
            required_path_identifier(arguments.get("delegationId"), "delegationId")?;
        self.get_json(&format!(
            "/api/sessions/{}/delegations/{}/result",
            self.parent_session_id, delegation_id
        ))
    }

    fn tool_cancel_session(&self, arguments: Value) -> Result<Value> {
        let delegation_id =
            required_path_identifier(arguments.get("delegationId"), "delegationId")?;
        self.post_json(
            &format!(
                "/api/sessions/{}/delegations/{}/cancel",
                self.parent_session_id, delegation_id
            ),
            &json!({}),
        )
    }

    fn tool_followup_session(&self, arguments: Value) -> Result<Value> {
        let delegation_id =
            required_path_identifier(arguments.get("delegationId"), "delegationId")?;
        let message = required_string(arguments.get("message"), "message")?;
        self.post_json(
            &format!(
                "/api/sessions/{}/delegations/{}/followup",
                self.parent_session_id, delegation_id
            ),
            &json!({ "message": message }),
        )
    }

    fn tool_send_to_session(&self, arguments: Value) -> Result<Value> {
        // Keep both id and name forms within the same conservative identifier grammar.
        // A session NAME containing `/`, `\`, `?`, `#` or `%` is rejected here rather than
        // resolved; callers target such a session by its id (termal_list_sessions shows it).
        let session_ref = required_path_identifier(arguments.get("sessionId"), "sessionId")?;
        let message = required_string(arguments.get("message"), "message")?;
        let idempotency_key = required_string(arguments.get("idempotencyKey"), "idempotencyKey")?;
        if idempotency_key.len() > 256 {
            bail!("idempotencyKey exceeds 256 bytes");
        }
        // Agents routinely pass a session NAME here ("LegalCodex") rather than a TermAl id,
        // so resolve a name to its id before delivering. A value that already looks like a
        // TermAl id ("session-…") is used directly.
        let session_id = self.resolve_session_reference(&session_ref)?;
        let mut body = serde_json::Map::new();
        body.insert(
            "targetSessionId".to_owned(),
            Value::String(session_id.clone()),
        );
        body.insert("message".to_owned(), Value::String(message));
        body.insert(
            "idempotencyKey".to_owned(),
            Value::String(idempotency_key),
        );
        insert_optional_string(&mut body, "topic", arguments.get("topic"));
        insert_optional_string(&mut body, "stateStamp", arguments.get("stateStamp"));
        if let Some(class) = optional_string(arguments.get("class")) {
            body.insert("class".to_owned(), Value::String(class));
        }
        let path = format!(
            "/api/sessions/{}/mailboxes/send",
            self.parent_session_id
        );
        let response = self
            .post_json_with_safe_replay(&path, &Value::Object(body))
            .map_err(mailbox_send_bridge_error)?;
        let receipt = serde_json::from_value::<MailboxAppendReceipt>(response).map_err(|source| {
            mailbox_send_bridge_error(
                TermalDelegationResponseError {
                    method: "POST",
                    path: path.clone(),
                    message: format!("mailbox send response shape was invalid: {source}"),
                }
                .into(),
            )
        })?;
        let mut response = serde_json::to_value(receipt)
            .context("failed to encode validated mailbox send receipt")?;
        let object = response
            .as_object_mut()
            .expect("serialized mailbox receipt should be an object");
        object.insert("sessionId".to_owned(), Value::String(session_id));
        object.insert("resolvedFrom".to_owned(), Value::String(session_ref));
        Ok(response)
    }

    fn tool_list_mailboxes(&self, _arguments: Value) -> Result<Value> {
        let mailboxes = self.get_json_with_safe_replay(&format!(
            "/api/sessions/{}/mailboxes",
            self.parent_session_id
        ))?;
        Ok(json!({ "mailboxes": mailboxes }))
    }

    fn tool_board_list(&self, arguments: Value) -> Result<Value> {
        let mut query_pairs = Vec::new();
        if let Some(after_key) = arguments.get("afterKey") {
            let after_key = required_board_key_shaped(Some(after_key), "afterKey")?;
            query_pairs.push(format!("afterKey={after_key}"));
        }
        for field in ["limit", "snapshotGeneration", "knownGeneration"] {
            if let Some(value) = arguments.get(field) {
                let value = required_u64(Some(value), field)?;
                query_pairs.push(format!("{field}={value}"));
            }
        }
        let query = if query_pairs.is_empty() {
            String::new()
        } else {
            format!("?{}", query_pairs.join("&"))
        };
        self.get_json_with_safe_replay(&format!(
            "/api/sessions/{}/board{query}",
            self.parent_session_id
        ))
    }

    fn tool_board_get(&self, arguments: Value) -> Result<Value> {
        let key = required_board_key_shaped(arguments.get("key"), "key")?;
        self.get_json_with_safe_replay(&format!(
            "/api/sessions/{}/board/keys/{key}",
            self.parent_session_id
        ))
    }

    fn tool_board_set(&self, arguments: Value) -> Result<Value> {
        let key = required_board_key_shaped(arguments.get("key"), "key")?;
        let expected_revision = required_u64(arguments.get("expectedRevision"), "expectedRevision")?;
        let idempotency_key = required_string(arguments.get("idempotencyKey"), "idempotencyKey")?;
        let state_stamp =
            optional_nonempty_string(arguments.get("stateStamp"), "stateStamp")?;
        let correlation = BoardSetCorrelation {
            key: key.clone(),
            expected_revision,
            author_session_id: self.parent_session_id.clone(),
            value: arguments.get("value").cloned(),
            delete: arguments
                .get("delete")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            state_stamp: state_stamp.clone(),
        };
        let mut body = serde_json::Map::new();
        body.insert("key".to_owned(), Value::String(key));
        body.insert("expectedRevision".to_owned(), json!(expected_revision));
        body.insert("idempotencyKey".to_owned(), Value::String(idempotency_key));
        // The value/delete discriminator is forwarded verbatim; the backend
        // owns the mutual-exclusion rule and the store owns all content
        // validation, so their error messages surface unaltered.
        if let Some(value) = arguments.get("value") {
            body.insert("value".to_owned(), value.clone());
        }
        if let Some(delete) = arguments.get("delete") {
            body.insert("delete".to_owned(), delete.clone());
        }
        if let Some(state_stamp) = state_stamp {
            body.insert("stateStamp".to_owned(), Value::String(state_stamp));
        }
        let response = self
            .post_json_with_safe_replay(
                &format!("/api/sessions/{}/board/set", self.parent_session_id),
                &Value::Object(body),
            )
            .map_err(board_set_bridge_error)?;
        // Validate the receipt shape AND correlate it with the request: a
        // structurally valid 2xx for the wrong key/intent proves nothing
        // about the requested mutation, so a mismatch is treated exactly
        // like a malformed body — outcome unknown, retry same key (review,
        // mailbox #236-3).
        let path = format!("/api/sessions/{}/board/set", self.parent_session_id);
        let receipt = serde_json::from_value::<CoordinationBoardSetReceipt>(response).map_err(
            |source| {
                board_set_bridge_error(
                    TermalDelegationResponseError {
                        method: "POST",
                        path: path.clone(),
                        message: format!(
                            "failed to decode coordination board receipt: {source}"
                        ),
                    }
                    .into(),
                )
            },
        )?;
        if let Err(mismatch) = correlate_board_receipt(&receipt, &correlation) {
            return Err(board_set_bridge_error(
                TermalDelegationResponseError {
                    method: "POST",
                    path,
                    message: format!(
                        "coordination board receipt does not correlate with the request ({mismatch})"
                    ),
                }
                .into(),
            ));
        }
        serde_json::to_value(&receipt).context("failed to re-encode coordination board receipt")
    }

    fn tool_read_mailbox(&self, arguments: Value) -> Result<Value> {
        let mailbox_id =
            required_path_identifier(arguments.get("mailboxId"), "mailboxId")?;
        let after_sequence = arguments
            .get("afterSequence")
            .map(|value| required_u64(Some(value), "afterSequence"))
            .transpose()?
            .unwrap_or(0);
        let limit = arguments
            .get("limit")
            .map(|value| required_u64(Some(value), "limit"))
            .transpose()?
            .unwrap_or(50);
        let messages = self.post_json_with_safe_replay(
            &format!(
                "/api/sessions/{}/mailboxes/{}/read",
                self.parent_session_id, mailbox_id
            ),
            &json!({ "afterSequence": after_sequence, "limit": limit }),
        )?;
        Ok(json!({ "mailboxId": mailbox_id, "messages": messages }))
    }

    fn tool_read_mailbox_message(&self, arguments: Value) -> Result<Value> {
        let message_id =
            required_path_identifier(arguments.get("messageId"), "messageId")?;
        self.get_json_with_safe_replay(&format!(
            "/api/sessions/{}/mailbox-messages/{}",
            self.parent_session_id, message_id
        ))
    }

    fn tool_acknowledge_mailbox(&self, arguments: Value) -> Result<Value> {
        let mailbox_id =
            required_path_identifier(arguments.get("mailboxId"), "mailboxId")?;
        let expected_processed_through = required_u64(
            arguments.get("expectedProcessedThrough"),
            "expectedProcessedThrough",
        )?;
        let processed_through =
            required_u64(arguments.get("processedThrough"), "processedThrough")?;
        let path = format!(
            "/api/sessions/{}/mailboxes/{}/acknowledge",
            self.parent_session_id, mailbox_id
        );
        let response = self
            .post_json_with_safe_replay(
                &path,
                &json!({
                    "expectedProcessedThrough": expected_processed_through,
                    "processedThrough": processed_through
                }),
            )
            .map_err(mailbox_acknowledgement_bridge_error)?;
        let summary = serde_json::from_value::<MailboxSummary>(response).map_err(|source| {
            mailbox_acknowledgement_bridge_error(
                TermalDelegationResponseError {
                    method: "POST",
                    path,
                    message: format!(
                        "mailbox acknowledgement response shape was invalid: {source}"
                    ),
                }
                .into(),
            )
        })?;
        serde_json::to_value(summary).context("failed to encode validated mailbox summary")
    }

    /// Resolves a peer session reference (an id or a name) to a VALIDATED target id.
    ///
    /// A value prefixed `session-` is a TermAl id; anything else is a session NAME matched
    /// case-insensitively via /api/state, across ALL projects (peer sessions frequently live
    /// in different projects). This is why a bare name — and the external Codex thread uuid
    /// shown in the UI — 404 without it. Ambiguous names and no match both return a guiding
    /// error.
    ///
    /// Exact ids skip target discovery through `/api/state`. The peer-tool gate performs one
    /// fail-safe caller-eligibility lookup and caches that immutable classification for the
    /// bridge lifetime, so sustained exact-id traffic does not serialize full state per
    /// message. The sender id remains path-validated by bridge construction, the target id is
    /// sent only as JSON, and the mailbox backend authoritatively rejects self,
    /// delegation-child, hidden, remote-proxy, and nonexistent targets. Names still require
    /// the live root-session inventory for case-insensitive and ambiguity-aware resolution.
    fn resolve_session_reference(&self, reference: &str) -> Result<String> {
        if reference.starts_with("session-") {
            if reference == self.parent_session_id {
                bail!(
                    "`{reference}` is this session — termal_send_to_session delivers to a PEER \
                     session, not to yourself"
                );
            }
            return Ok(reference.to_owned());
        }

        let state = self.get_json("/api/state")?;
        let delegation_child_ids = delegation_child_session_ids(&state);
        let sessions = state
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut matches = sessions
            .iter()
            // Root sessions only — use both the direct parent link and the
            // durable delegation inventory so a stale/missing denormalized
            // link cannot turn a child into a peer target.
            .filter(|session| is_root_peer_session(session, &delegation_child_ids))
            .filter(|session| {
                session
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(reference))
            })
            .filter_map(|session| {
                let id = session.get("id").and_then(Value::as_str)?.to_owned();
                let project = session
                    .get("projectId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                Some((id, project))
            })
            .collect::<Vec<_>>();
        match matches.len() {
            0 => bail!(
                "no root session has id or name `{reference}` — call termal_list_sessions to see \
                 the available sessions and their ids"
            ),
            1 => {
                let id = matches.remove(0).0;
                if id == self.parent_session_id {
                    bail!(
                        "`{reference}` is this session — termal_send_to_session delivers to a PEER \
                         session, not to yourself"
                    );
                }
                Ok(id)
            }
            _ => {
                let listed = matches
                    .iter()
                    .map(|(id, project)| format!("{id} (project {project})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "session name `{reference}` is ambiguous — matches {listed}; pass the exact sessionId"
                )
            }
        }
    }

    fn tool_list_sessions(&self, _arguments: Value) -> Result<Value> {
        // Peer discovery: resolve a session by name to its id for termal_send_to_session.
        // /api/state is metadata-only (no transcripts), so this is a cheap summary read.
        let state = self.get_json("/api/state")?;
        let delegation_child_ids = delegation_child_session_ids(&state);
        let sessions = state
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let listed = sessions
            .iter()
            // Root sessions only. The delegation row is authoritative even
            // if an older/re-attached child temporarily lacks its parent id.
            .filter(|session| is_root_peer_session(session, &delegation_child_ids))
            .map(|session| {
                json!({
                    "sessionId": session.get("id").cloned().unwrap_or(Value::Null),
                    "name": session.get("name").cloned().unwrap_or(Value::Null),
                    "agent": session.get("agent").cloned().unwrap_or(Value::Null),
                    "status": session.get("status").cloned().unwrap_or(Value::Null),
                    "workdir": session.get("workdir").cloned().unwrap_or(Value::Null),
                    "preview": session.get("preview").cloned().unwrap_or(Value::Null),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({ "sessions": listed }))
    }

    fn tool_resume_after_delegations(&self, arguments: Value) -> Result<Value> {
        let delegation_ids =
            required_path_identifier_array(arguments.get("delegationIds"), "delegationIds")?;
        let mut body = serde_json::Map::new();
        body.insert(
            "delegationIds".to_owned(),
            Value::Array(delegation_ids.into_iter().map(Value::String).collect()),
        );
        if let Some(mode) = optional_string(arguments.get("mode")) {
            body.insert("mode".to_owned(), Value::String(mode));
        }
        insert_optional_string(&mut body, "title", arguments.get("title"));
        self.post_json(
            &format!("/api/sessions/{}/delegation-waits", self.parent_session_id),
            &Value::Object(body),
        )
    }

    fn tool_wait_delegations(&self, arguments: Value) -> Result<Value> {
        let delegation_ids =
            required_path_identifier_array(arguments.get("delegationIds"), "delegationIds")?;
        let mode = optional_string(arguments.get("mode")).unwrap_or_else(|| "all".to_owned());
        let mode = match mode.as_str() {
            "all" | "any" => mode,
            other => bail!("mode must be `all` or `any`, got `{other}`"),
        };
        let poll_interval_ms = optional_u64(arguments.get("pollIntervalMs"))
            .unwrap_or(TERMAL_DELEGATION_MCP_DEFAULT_WAIT_INTERVAL_MS)
            .clamp(100, 30_000);
        let timeout_ms = optional_u64(arguments.get("timeoutMs"))
            .unwrap_or(TERMAL_DELEGATION_MCP_DEFAULT_WAIT_TIMEOUT_MS)
            .min(TERMAL_DELEGATION_MCP_MAX_WAIT_TIMEOUT_MS);
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let mut statuses = Vec::new();
            for id in &delegation_ids {
                statuses.push(self.get_json(&format!(
                    "/api/sessions/{}/delegations/{}",
                    self.parent_session_id, id
                ))?);
            }
            let terminal_count = statuses
                .iter()
                .filter(|status| {
                    delegation_status_from_response(status)
                        .is_some_and(is_terminal_delegation_status)
                })
                .count();
            let satisfied = if mode == "any" {
                terminal_count > 0
            } else {
                terminal_count == delegation_ids.len()
            };
            if satisfied {
                let mut results = Vec::new();
                for id in &delegation_ids {
                    match self.get_json(&format!(
                        "/api/sessions/{}/delegations/{}/result",
                        self.parent_session_id, id
                    )) {
                        Ok(result) => results.push(json!({
                            "delegationId": id,
                            "result": result,
                        })),
                        Err(err) => results.push(json!({
                            "delegationId": id,
                            "error": err.to_string(),
                        })),
                    }
                }
                return Ok(json!({
                    "mode": mode,
                    "timedOut": false,
                    "statuses": statuses,
                    "results": results,
                }));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(json!({
                    "mode": mode,
                    "timedOut": true,
                    "statuses": statuses,
                    "results": [],
                }));
            }
            std::thread::sleep(Duration::from_millis(poll_interval_ms));
        }
    }

    /// Whether the session this bridge serves is a delegation CHILD (a spawned reviewer /
    /// explorer / worker) rather than a root session. Peer tools are root-only, so a child
    /// must not enumerate or message arbitrary sessions (tm-r0y). Root classification is
    /// conjunctive: both the session's parent marker and the durable delegation-child index
    /// must be clear. A repair can therefore restore a missing marker only after the index
    /// already classified that session as a child; a root grant cannot precede that repair.
    /// Fail SAFE: if the backend can't be reached or the caller can't be found, treat it as
    /// a child (deny peer tools).
    fn caller_is_delegation_child(&self) -> bool {
        if let Some(is_child) = self.caller_is_delegation_child.get() {
            return *is_child;
        }
        let Ok(state) = self.get_json("/api/state") else {
            // Fail safe without caching a transient backend failure. A later
            // call may retry the eligibility lookup after the backend recovers.
            return true;
        };
        let delegation_child_ids = delegation_child_session_ids(&state);
        let Some(sessions) = state.get("sessions").and_then(Value::as_array) else {
            return true;
        };
        let Some(session) = sessions.iter().find(|session| {
            session.get("id").and_then(Value::as_str) == Some(self.parent_session_id.as_str())
        }) else {
            // Hidden Claude spares are intentionally omitted from `/api/state`.
            // Their MCP bridge can be queried before the same session record is
            // promoted visible, so fail closed for this call without turning a
            // routine pre-promotion snapshot into a lifetime denial.
            return true;
        };
        let is_child = !is_root_peer_session(session, &delegation_child_ids);
        let _ = self.caller_is_delegation_child.set(is_child);
        is_child
    }

    /// The advertised tool list for this bridge's caller: the full set for a root session, or
    /// the set with the peer tools removed for a delegation child (tm-r0y).
    fn tools_list_for_caller(&self) -> Value {
        let mut result = mcp_tools_list_result();
        if self.caller_is_delegation_child() {
            if let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) {
                tools.retain(|tool| {
                    !tool
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(tool_requires_root_session)
                });
            }
        }
        result
    }

    fn get_json(&self, path: &str) -> Result<Value> {
        self.decode_response(
            "GET",
            path,
            self.client
                .get(self.url(path))
                .timeout(self.request_timeout)
                .send(),
        )
    }

    fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.decode_response(
            "POST",
            path,
            self.client
                .post(self.url(path))
                .timeout(self.request_timeout)
                .json(body)
                .send(),
        )
    }

    /// Explicit retry opt-in for operations whose callers have proved replay
    /// safe: pure reads, CAS acknowledgements/board writes, and
    /// idempotency-keyed mailbox appends. Generic bridge GET/POST helpers are
    /// deliberately single-attempt so future endpoints cannot inherit replay
    /// merely by returning matching prose.
    fn get_json_with_safe_replay(&self, path: &str) -> Result<Value> {
        self.request_json_with_safe_replay_retry("GET", path, |remaining| {
            self.client.get(self.url(path)).timeout(remaining).send()
        })
    }

    fn post_json_with_safe_replay(&self, path: &str, body: &Value) -> Result<Value> {
        self.request_json_with_safe_replay_retry("POST", path, |remaining| {
            self.client
                .post(self.url(path))
                .timeout(remaining)
                .json(body)
                .send()
        })
    }

    /// Replays a request while the backend answers with a typed safe-replay
    /// storage-busy rejection (see the clause constants above for why replay
    /// is provably safe). The closure rebuilds the request from scratch on every
    /// attempt — with a per-attempt HTTP timeout shrunk to the remaining
    /// overall budget — so GET and POST share one bounded policy and the
    /// whole helper can never exceed one `request_timeout` of wall time
    /// (apart from scheduler overhead; the jittered sleeps burn down the same
    /// deadline). Anything else — transport failures, other
    /// statuses, even other 503s — returns on the first attempt untouched:
    /// without a recognized write or read safety clause there is no
    /// structural guarantee that replay is safe, and blind replay would be
    /// wrong.
    fn request_json_with_safe_replay_retry(
        &self,
        method: &'static str,
        path: &str,
        send: impl Fn(Duration) -> std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<Value> {
        let deadline = std::time::Instant::now() + self.request_timeout;
        let mut completed_attempts = 0u32;
        loop {
            completed_attempts += 1;
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .max(Duration::from_millis(1));
            let result = self.decode_response(method, path, send(remaining));
            let retry = completed_attempts < TERMAL_DELEGATION_SAFE_REPLAY_RETRY_ATTEMPTS
                && result
                    .as_ref()
                    .err()
                    .is_some_and(error_is_safe_replay_service_unavailable);
            if !retry {
                return result;
            }
            let delay =
                safe_replay_retry_delay(&self.parent_session_id, completed_attempts);
            // Only sleep-and-replay when the remaining budget can fund the
            // delay AND leave a minimally useful request window; otherwise
            // surface the last typed rejection now. The comparison is
            // one-sided on purpose: elapsed time can only turn a would-be
            // replay into an immediate return, never the reverse, which keeps
            // short-budget behavior deterministic under arbitrary scheduling
            // stalls.
            let budget_left = deadline.saturating_duration_since(std::time::Instant::now());
            if budget_left <= delay + TERMAL_DELEGATION_SAFE_REPLAY_MIN_REPLAY_BUDGET {
                return result;
            }
            (self.safe_replay_retry_sleeper)(delay);
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn decode_response(
        &self,
        method: &'static str,
        path: &str,
        response: std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<Value> {
        let response = response.map_err(|source| TermalDelegationTransportError {
            method,
            path: path.to_owned(),
            phase: "sending request",
            timeout: self.request_timeout,
            source,
        })?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|source| TermalDelegationTransportError {
                method,
                path: path.to_owned(),
                phase: "reading response body",
                timeout: self.request_timeout,
                source,
            })?;
        if status.is_success() {
            return serde_json::from_str(&text).map_err(|source| {
                TermalDelegationResponseError {
                    method,
                    path: path.to_owned(),
                    message: format!("failed to parse successful response JSON: {source}"),
                }
                .into()
            });
        }
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or(text);
        Err(TermalDelegationApiError { status, message }.into())
    }
}

fn mailbox_send_bridge_error(err: anyhow::Error) -> anyhow::Error {
    if err.downcast_ref::<TermalDelegationTransportError>().is_some()
        || err.downcast_ref::<TermalDelegationResponseError>().is_some()
    {
        return anyhow!(
            "mailbox send receipt was not received; the append outcome is unknown. Retry with the \
             same idempotencyKey to recover the original receipt safely: {err}"
        );
    }
    err
}

/// Request-side snapshot used to correlate a board set receipt with the
/// intent that produced it (review, mailbox #236-3).
struct BoardSetCorrelation {
    key: String,
    expected_revision: u64,
    author_session_id: String,
    value: Option<Value>,
    delete: bool,
    state_stamp: Option<String>,
}

/// A receipt confirms the requested mutation only when every request-visible
/// field lines up. Duplicate replays still correlate: the durable original
/// was the identical request. Returns the first mismatch as text.
fn correlate_board_receipt(
    receipt: &CoordinationBoardSetReceipt,
    correlation: &BoardSetCorrelation,
) -> std::result::Result<(), String> {
    if receipt.key != correlation.key {
        return Err(format!(
            "receipt key `{}` != requested `{}`",
            receipt.key, correlation.key
        ));
    }
    if receipt.author_session_id != correlation.author_session_id {
        return Err(format!(
            "receipt author `{}` != caller `{}`",
            receipt.author_session_id, correlation.author_session_id
        ));
    }
    if receipt.prior_revision != correlation.expected_revision {
        return Err(format!(
            "receipt priorRevision {} != expectedRevision {}",
            receipt.prior_revision, correlation.expected_revision
        ));
    }
    // The 2xx body is untrusted input: checked_add keeps a hostile
    // priorRevision of u64::MAX a correlation mismatch instead of a
    // debug-build overflow panic (review, mailbox #239).
    if receipt
        .prior_revision
        .checked_add(1)
        .is_none_or(|successor| receipt.revision != successor)
    {
        return Err(format!(
            "receipt revision {} is not priorRevision {} + 1",
            receipt.revision, receipt.prior_revision
        ));
    }
    if receipt.deleted != correlation.delete {
        return Err(format!(
            "receipt deleted={} != requested delete={}",
            receipt.deleted, correlation.delete
        ));
    }
    if correlation.delete {
        // A tombstone receipt must carry the structurally null value; a
        // deleted:true receipt with a non-null value is not OUR delete
        // (review, mailbox #239).
        if receipt.value != Value::Null {
            return Err("receipt claims deletion but carries a non-null value".to_owned());
        }
    } else {
        let requested_value = correlation.value.as_ref().cloned().unwrap_or(Value::Null);
        if receipt.value != requested_value {
            return Err("receipt value differs from the requested value".to_owned());
        }
    }
    if receipt.state_stamp != correlation.state_stamp {
        return Err(format!(
            "receipt stateStamp {:?} != requested {:?}",
            receipt.state_stamp, correlation.state_stamp
        ));
    }
    Ok(())
}

/// Board mutations mirror mailbox sends' unknown-outcome discipline: when
/// transport fails or a 2xx body cannot be decoded, the write may or may not
/// have committed, so the caller must retry the EXACT same intent with the
/// SAME idempotencyKey — a committed original then replays as
/// `duplicate: true`, an uncommitted one applies fresh. Typed non-2xx API
/// errors (400/404/409/503) pass through unchanged: their outcome is known.
fn board_set_bridge_error(err: anyhow::Error) -> anyhow::Error {
    if err.downcast_ref::<TermalDelegationTransportError>().is_some()
        || err.downcast_ref::<TermalDelegationResponseError>().is_some()
    {
        return anyhow!(
            "board update receipt was not received; the write outcome is unknown. Retry the \
             exact same request with the SAME idempotencyKey: a committed write replays as \
             duplicate=true, an uncommitted one applies cleanly: {err}"
        );
    }
    err
}

fn mailbox_acknowledgement_bridge_error(err: anyhow::Error) -> anyhow::Error {
    if err.downcast_ref::<TermalDelegationTransportError>().is_some()
        || err.downcast_ref::<TermalDelegationResponseError>().is_some()
    {
        return anyhow!(
            "mailbox acknowledgement response was not received; the cursor outcome is unknown. \
             Call termal_list_mailboxes before retrying, then use its processedThrough as \
             expectedProcessedThrough: {err}"
        );
    }
    err
}

#[derive(Debug)]
struct TermalDelegationResponseError {
    method: &'static str,
    path: String,
    message: String,
}

impl std::fmt::Display for TermalDelegationResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "TermAl delegation API {} {} returned an unusable successful response: {}",
            self.method, self.path, self.message
        )
    }
}

impl std::error::Error for TermalDelegationResponseError {}

#[derive(Debug)]
struct TermalDelegationTransportError {
    method: &'static str,
    path: String,
    phase: &'static str,
    timeout: Duration,
    source: reqwest::Error,
}

impl std::fmt::Display for TermalDelegationTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let classification = if self.source.is_timeout() {
            format!("timed out after {:?}", self.timeout)
        } else if self.source.is_connect() {
            "could not connect".to_owned()
        } else {
            "transport failed".to_owned()
        };
        write!(
            formatter,
            "TermAl delegation API {} {} {} while {}: {}",
            self.method, self.path, classification, self.phase, self.source
        )
    }
}

impl std::error::Error for TermalDelegationTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
struct TermalDelegationApiError {
    status: StatusCode,
    message: String,
}

impl TermalDelegationApiError {
    fn is_agent_command_not_found(&self) -> bool {
        self.status == StatusCode::NOT_FOUND && self.message == "agent command not found"
    }
}

/// True only for a typed safe-replay rejection whose body contains an exact
/// read/durable-append clause or a structural no-commit write clause. This
/// classifier is consulted only by explicit safe-replay call sites; generic
/// bridge requests remain single-attempt regardless of response prose.
fn error_is_safe_replay_service_unavailable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<TermalDelegationApiError>()
        .is_some_and(|api_error| {
            api_error.status == StatusCode::SERVICE_UNAVAILABLE
                && message_carries_safe_replay_clause(&api_error.message)
        })
}

fn message_carries_safe_replay_clause(message: &str) -> bool {
    message.split(';').any(|clause| {
        let clause = clause.trim();
        clause == TERMAL_DELEGATION_SAFE_READ_RETRY_CLAUSE
            || clause == TERMAL_DELEGATION_DURABLE_APPEND_RETRY_CLAUSE
            || (clause.starts_with(TERMAL_DELEGATION_NO_COMMIT_CLAUSE_PREFIX)
                && clause.ends_with(TERMAL_DELEGATION_NO_COMMIT_CLAUSE_SUFFIX))
    })
}

/// Deterministic, session-stable jitter percentage in `75..=125`. Peers
/// released by the same saturation event dephase because their session ids
/// hash to different schedules, while any single bridge's schedule is exact
/// and test-assertable — no randomness, no clock reads (FNV-1a over the
/// session id and attempt number).
#[cfg(test)]
fn safe_replay_retry_jitter_percent(session_id: &str, completed_attempts: u32) -> u32 {
    session_stable_retry_jitter_percent(session_id, completed_attempts)
}

/// Backoff after the Nth failed attempt: base 200ms doubling per failure
/// (200/400/800/1600 across the four replays permitted by
/// `TERMAL_DELEGATION_SAFE_REPLAY_RETRY_ATTEMPTS`), scaled by the session's
/// deterministic jitter percentage.
fn safe_replay_retry_delay(session_id: &str, completed_attempts: u32) -> Duration {
    session_stable_retry_delay(
        TERMAL_DELEGATION_SAFE_REPLAY_RETRY_BASE_DELAY,
        session_id,
        completed_attempts,
    )
}

impl std::fmt::Display for TermalDelegationApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "TermAl delegation API returned {}: {}",
            self.status, self.message
        )
    }
}

impl std::error::Error for TermalDelegationApiError {}

fn run_delegation_mcp_bridge(parent_session_id: String, base_url: String) -> Result<()> {
    let bridge = TermalDelegationMcpBridge::new(parent_session_id, base_url)?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read MCP stdin")?;
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)
            .with_context(|| format!("failed to parse MCP JSON-RPC message: {line}"))?;
        if let Some(response) = bridge.handle_message(message)? {
            serde_json::to_writer(&mut stdout, &response)
                .context("failed to write MCP response JSON")?;
            stdout
                .write_all(b"\n")
                .context("failed to write MCP response newline")?;
            stdout.flush().context("failed to flush MCP response")?;
        }
    }
    Ok(())
}

fn mcp_initialize_result() -> Value {
    json!({
        "protocolVersion": TERMAL_DELEGATION_MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {},
        },
        "serverInfo": {
            "name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Transport-safety shape check for board keys crossing the bridge into URL
/// paths and query strings: ASCII lowercase alphanumerics plus `.`/`_`/`-`,
/// bounded by the store's shared byte limit. Deliberately LOOSER than the
/// store's segment grammar — the store remains the single validation
/// authority for structure; this only guarantees the characters cannot alter
/// the request's path or query framing (no `/`, `%`, `&`, `?`, `#`,
/// whitespace are possible). Exact `.` and `..` are rejected because URL
/// clients normalize those path segments before sending the request.
fn required_board_key_shaped(value: Option<&Value>, field: &str) -> Result<String> {
    let key = required_string(value, field)?;
    if matches!(key.as_str(), "." | "..")
        || key.len() > COORDINATION_BOARD_MAX_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    {
        return Err(anyhow!(
            "{field} must be 1-{} bytes of lowercase alphanumerics, `.`, `_`, or `-`, and must \
             not be `.` or `..`",
            COORDINATION_BOARD_MAX_KEY_BYTES
        ));
    }
    Ok(key)
}

/// Peer and board tools operate on arbitrary root-session coordination state,
/// so they are restricted to root callers and hidden from / rejected for
/// delegation children (tm-r0y).
fn tool_requires_root_session(name: &str) -> bool {
    matches!(
        name,
        "termal_send_to_session"
            | "termal_list_sessions"
            | "termal_list_mailboxes"
            | "termal_read_mailbox"
            | "termal_read_mailbox_message"
            | "termal_acknowledge_mailbox"
            | "termal_board_list"
            | "termal_board_get"
            | "termal_board_set"
    )
}

fn mcp_tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "termal_spawn_session",
                "description": "Create a TermAl child delegation under the current parent session. Single-line prompts matching a known slash command are resolved before spawning. OpenCode does not support writePolicy readOnly; use isolatedWorktree for bounded writable OpenCode work.",
                "inputSchema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "Task prompt. Single-line known slash commands are resolved with delegation intent before spawning."
                        },
                        "title": { "type": "string" },
                        "cwd": {
                            "type": "string",
                            "description": "Working directory for the spawned session. For single-line known slash-command prompts, cwd also scopes command resolution."
                        },
                        "agent": { "type": "string", "enum": ["Codex", "Claude", "Cursor", "Gemini", "OpenCode"] },
                        "model": { "type": "string" },
                        "mode": { "type": "string", "enum": ["reviewer", "explorer", "worker"] },
                        "writePolicy": {
                            "description": "OpenCode rejects readOnly. Use isolatedWorktree for OpenCode delegation sessions.",
                            "oneOf": [
                                { "type": "string", "enum": ["readOnly", "isolatedWorktree", "sharedWorktree"] },
                                { "type": "object" }
                            ]
                        }
                    }
                }
            },
            {
                "name": "termal_list_delegations",
                "description": "List compact metadata for every delegation owned by the current parent session. Use this to recover exact delegationId and childSessionId values after a spawn result or conversation context was truncated; the recovered ids can be passed directly to status, result, cancel, follow-up, wait, or resume tools. Same-title delegations remain separate. Takes no arguments.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "termal_get_session_status",
                "description": "Get a parent-scoped TermAl delegation status.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationId"],
                    "properties": { "delegationId": { "type": "string" } }
                }
            },
            {
                "name": "termal_get_session_result",
                "description": "Get the compact result packet for a completed parent-scoped TermAl delegation.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationId"],
                    "properties": { "delegationId": { "type": "string" } }
                }
            },
            {
                "name": "termal_cancel_session",
                "description": "Cancel a parent-scoped TermAl delegation.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationId"],
                    "properties": { "delegationId": { "type": "string" } }
                }
            },
            {
                "name": "termal_wait_delegations",
                "description": "Synchronously poll parent-scoped delegations until any/all are terminal or timeout.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationIds"],
                    "properties": {
                        "delegationIds": { "type": "array", "items": { "type": "string" } },
                        "mode": { "type": "string", "enum": ["all", "any"] },
                        "pollIntervalMs": { "type": "integer" },
                        "timeoutMs": { "type": "integer" }
                    }
                }
            },
            {
                "name": "termal_resume_after_delegations",
                "description": "Schedule a durable TermAl backend resume wait for parent-scoped delegations.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationIds"],
                    "properties": {
                        "delegationIds": { "type": "array", "items": { "type": "string" } },
                        "mode": { "type": "string", "enum": ["all", "any"] },
                        "title": { "type": "string" }
                    }
                }
            },
            {
                "name": "termal_followup_session",
                "description": "Resume a COMPLETED parent-scoped TermAl delegation with a follow-up message. Fails if the delegation is still running (wait via termal_resume_after_delegations first) or if its child session was removed.",
                "inputSchema": {
                    "type": "object",
                    "required": ["delegationId", "message"],
                    "properties": {
                        "delegationId": { "type": "string" },
                        "message": { "type": "string" }
                    }
                }
            },
            {
                "name": "termal_send_to_session",
                "description": "Durably append a routine message to the neutral mailbox shared with another root-level TermAl session, then best-effort wake that peer with metadata only. `sessionId` accepts a TermAl id or case-insensitive session name; prefer an exact id for sustained traffic because names require peer discovery. `idempotencyKey` is required and sender-scoped: retrying the same intent returns the original receipt with duplicate=true; reusing it for different content conflicts. If transport fails before a receipt arrives, the append outcome is unknown: retry the exact same intent and key. Receipt `notificationDisposition` is the immutable point-in-time dispatch outcome; mailbox reads expose the evolving row lifecycle as `notificationState`. The durable body is fetched through termal_read_mailbox. FIRE-AND-FORGET — there is no reply to await.",
                "inputSchema": {
                    "type": "object",
                    "required": ["sessionId", "message", "idempotencyKey"],
                    "properties": {
                        "sessionId": { "type": "string" },
                        "message": { "type": "string" },
                        "idempotencyKey": { "type": "string", "maxLength": 256 },
                        "topic": { "type": "string" },
                        "stateStamp": { "type": "string" },
                        "class": {
                            "type": "string",
                            "enum": ["routine"],
                            "description": "Foundation mailboxes support routine delivery only. STOP/urgent delivery is intentionally inactive."
                        }
                    }
                }
            },
            {
                "name": "termal_list_sessions",
                "description": "List the root-level TermAl sessions (sessionId, name, agent, status, workdir, preview) so you can resolve a session by name to its id for termal_send_to_session. Excludes delegation-child sessions. Takes no arguments.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "termal_list_mailboxes",
                "description": "List durable neutral mailboxes for this session, including participants, latest sequence, and this session's unread count. Takes no arguments.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "termal_read_mailbox",
                "description": "Fetch a FIFO range of durable mailbox messages. Each message's `notificationState` is the current mutable wake lifecycle state, not the sender receipt's immutable `notificationDisposition`. Reading never advances the processed cursor; acknowledge separately after processing.",
                "inputSchema": {
                    "type": "object",
                    "required": ["mailboxId"],
                    "properties": {
                        "mailboxId": { "type": "string" },
                        "afterSequence": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                    }
                }
            },
            {
                "name": "termal_read_mailbox_message",
                "description": "Fetch one exact durable mailbox message by receipt messageId. Its `notificationState` is the current mutable wake lifecycle state, not the sender receipt's immutable `notificationDisposition`. The caller must be a current participant.",
                "inputSchema": {
                    "type": "object",
                    "required": ["messageId"],
                    "properties": {
                        "messageId": { "type": "string" }
                    }
                }
            },
            {
                "name": "termal_acknowledge_mailbox",
                "description": "Advance this session's mailbox processed cursor with a forward-only compare-and-swap. Supply the cursor value you observed and the sequence processed through. New progress requires the observed cursor to match; replay at or below the durable cursor succeeds idempotently after a lost response; only a stale attempt to advance past the durable cursor conflicts.",
                "inputSchema": {
                    "type": "object",
                    "required": ["mailboxId", "expectedProcessedThrough", "processedThrough"],
                    "properties": {
                        "mailboxId": { "type": "string" },
                        "expectedProcessedThrough": { "type": "integer", "minimum": 0 },
                        "processedThrough": { "type": "integer", "minimum": 0 }
                    }
                }
            },
            {
                "name": "termal_board_list",
                "description": "List this project's coordination board: small versioned JSON facts (LEVEL state — durable truth peers read at their own pace), the complement of mailbox messages (EDGE events that wake sessions). Board writes never wake anyone. Response carries the scope `generation` (bumps on every successful write); pass it back as `knownGeneration` on a later first-page call for a cheap unchanged short-circuit, or as `snapshotGeneration` with `afterKey` to continue pagination (a mutation between pages returns 409 — restart). Entries are sorted by key; deleted keys never appear.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "afterKey": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200 },
                        "snapshotGeneration": { "type": "integer", "minimum": 0 },
                        "knownGeneration": { "type": "integer", "minimum": 0 }
                    }
                }
            },
            {
                "name": "termal_board_get",
                "description": "Fetch one coordination board key's current head: value, per-key `revision` (CAS token for termal_board_set), `updatedAtGeneration` (scope generation when this key was last written), current whole-scope `scopeGeneration`, author, and `stateStamp`. Pass only `scopeGeneration` as knownGeneration for a later first-page list probe. Deleted or never-created keys return 404; a 404/409 `detail` may include the tombstone's revision, which is the deliberate-restore CAS token.",
                "inputSchema": {
                    "type": "object",
                    "required": ["key"],
                    "properties": {
                        "key": { "type": "string" }
                    }
                }
            },
            {
                "name": "termal_board_set",
                "description": "Create, update, or delete one coordination board key with compare-and-swap. `expectedRevision` 0 = create-only (key must never have existed; a tombstoned key 409s with its revision in `detail` — re-set AT that revision to consciously restore). Otherwise pass the exact current revision from get/list. Provide `value` (any JSON <=4KiB canonical, `null` is a real value) to set, or `delete: true` with NO value to delete (deletes need expectedRevision >= 1). `idempotencyKey` is required and author-scoped: retrying the same intent returns the original receipt with `duplicate: true`; reusing it for different content 409s. Writes NEVER wake peers — send a mailbox message if the change needs someone's attention now. Keys: 1-8 dotted segments, lowercase alphanumeric start, `_`/`-` allowed after (e.g. `activity.rust-suite`).",
                "inputSchema": {
                    "type": "object",
                    "required": ["key", "expectedRevision", "idempotencyKey"],
                    "properties": {
                        "key": { "type": "string" },
                        "value": {},
                        "delete": { "type": "boolean" },
                        "expectedRevision": { "type": "integer", "minimum": 0 },
                        "idempotencyKey": { "type": "string" },
                        "stateStamp": { "type": "string" }
                    }
                }
            }
        ]
    })
}

fn mcp_json_rpc_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn mcp_json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

fn mcp_json_rpc_tool_error(id: Value, message: String) -> Value {
    mcp_json_rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
    )
}

fn mcp_tool_text_result(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn required_string(value: Option<&Value>, label: &str) -> Result<String> {
    optional_string(value)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{label} is required"))
}

fn required_u64(value: Option<&Value>, label: &str) -> Result<u64> {
    value
        .and_then(Value::as_u64)
        .with_context(|| format!("{label} must be a non-negative integer"))
}

fn optional_nonempty_string(value: Option<&Value>, label: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .with_context(|| format!("{label} must be a non-empty string"))?;
    if value.trim().is_empty() {
        bail!("{label} must be a non-empty string");
    }
    // Preserve the exact caller value. The board store includes stateStamp in
    // its idempotency request hash, so trimming here would make an MCP retry
    // disagree with the equivalent raw-HTTP request.
    Ok(Some(value.to_owned()))
}

fn required_path_identifier(value: Option<&Value>, label: &str) -> Result<String> {
    let value = required_string(value, label)?;
    if value
        .chars()
        .any(|ch| {
            ch == '/' || ch == '?' || ch == '#' || ch == '%' || ch == '\\' || ch.is_control()
        })
    {
        bail!("{label} must not contain /, \\, ?, #, %, or control characters");
    }
    if value == "." || value == ".." {
        bail!("{label} must not be . or ..");
    }
    Ok(value)
}

fn required_agent_command_name(value: Option<&Value>) -> Result<String> {
    let value = required_string(value, "command")?;
    if value.chars().any(|ch| {
        ch == '/' || ch == '?' || ch == '#' || ch == '\\' || ch.is_control()
    }) {
        bail!("command must not contain /, \\, ?, #, or control characters");
    }
    if value == "." || value == ".." {
        bail!("command must not be . or ..");
    }
    Ok(value)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn insert_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&Value>,
) -> bool {
    if let Some(value) = optional_string(value) {
        object.insert(key.to_owned(), Value::String(value));
        true
    } else {
        false
    }
}

struct McpSpawnPrompt {
    prompt: String,
    title: Option<String>,
    mode: Option<String>,
    write_policy: Option<Value>,
}

impl McpSpawnPrompt {
    fn literal(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_owned(),
            title: None,
            mode: None,
            write_policy: None,
        }
    }
}

struct McpSlashCommandPrompt {
    command_name: String,
    arguments: Option<String>,
    note: Option<String>,
}

/// Parses the single-line slash-command shape supported by `termal_spawn_session`.
///
/// Example: `/review-code staged -- include tests` resolves to command
/// `review-code`, arguments `staged`, and note `include tests`.
fn parse_mcp_slash_command_prompt(prompt: &str) -> Option<McpSlashCommandPrompt> {
    let prompt = prompt.trim_end();
    if prompt.contains('\n') || prompt.contains('\r') {
        return None;
    }
    let rest = prompt.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let mut parts = rest.splitn(2, char::is_whitespace);
    let command_name = parts.next()?;
    if command_name.is_empty() || command_name.contains('/') {
        return None;
    }
    let (arguments, note) = split_mcp_agent_command_tail(parts.next().unwrap_or_default());
    Some(McpSlashCommandPrompt {
        command_name: command_name.to_owned(),
        arguments,
        note,
    })
}

/// Splits slash-command tail text using the same `--` note separator as the UI.
fn split_mcp_agent_command_tail(tail: &str) -> (Option<String>, Option<String>) {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let bytes = trimmed.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'-'
            && bytes[index + 1] == b'-'
            && (index == 0 || bytes[index - 1].is_ascii_whitespace())
            && (index + 2 == bytes.len() || bytes[index + 2].is_ascii_whitespace())
        {
            let arguments = trimmed[..index].trim();
            let note = trimmed[index + 2..].trim();
            return (
                (!arguments.is_empty()).then(|| arguments.to_owned()),
                (!note.is_empty()).then(|| note.to_owned()),
            );
        }
        index += 1;
    }
    (Some(trimmed.to_owned()), None)
}

fn required_path_identifier_array(value: Option<&Value>, label: &str) -> Result<Vec<String>> {
    let array = value
        .and_then(Value::as_array)
        .with_context(|| format!("{label} must be an array"))?;
    let mut values = Vec::new();
    for item in array {
        values.push(required_path_identifier(Some(item), label)?);
    }
    if values.is_empty() {
        bail!("{label} must not be empty");
    }
    Ok(values)
}

fn optional_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

fn normalize_mcp_write_policy(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(kind)) => json!({ "kind": kind }),
        Some(value) => value.clone(),
        None => json!({ "kind": "readOnly" }),
    }
}

fn delegation_status_from_response(value: &Value) -> Option<&str> {
    value.pointer("/delegation/status").and_then(Value::as_str)
}

fn is_terminal_delegation_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "canceled")
}

#[cfg(test)]
#[path = "delegation_mcp_tests.rs"]
mod delegation_mcp_tests;
