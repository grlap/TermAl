const TERMAL_DELEGATION_MCP_SERVER_NAME: &str = "termal-delegation";
const TERMAL_DELEGATION_MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const TERMAL_DELEGATION_MCP_DEFAULT_WAIT_INTERVAL_MS: u64 = 1000;
const TERMAL_DELEGATION_MCP_DEFAULT_WAIT_TIMEOUT_MS: u64 = 300_000;
const TERMAL_DELEGATION_MCP_MAX_WAIT_TIMEOUT_MS: u64 = 1_800_000;

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
}

impl TermalDelegationMcpBridge {
    fn new(parent_session_id: String, base_url: String) -> Result<Self> {
        let parent_session_id = required_path_identifier(
            Some(&Value::String(parent_session_id)),
            "delegation MCP parent session id",
        )?;
        Ok(Self {
            parent_session_id,
            base_url: normalize_termal_http_base_url(base_url),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("failed to build delegation MCP HTTP client")?,
        })
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
        // Peer tools (message/enumerate arbitrary sessions) are root-session only. A
        // delegation child (e.g. a read-only reviewer chewing on untrusted code) must not
        // reach them, so reject here as well as hiding them from tools/list (tm-r0y).
        if tool_is_peer_scoped(&name) && self.caller_is_delegation_child() {
            bail!(
                "`{name}` is not available to delegation-child sessions; peer messaging is \
                 restricted to root sessions"
            );
        }
        let result = match name.as_str() {
            "termal_spawn_session" => self.tool_spawn_session(arguments),
            "termal_get_session_status" => self.tool_get_session_status(arguments),
            "termal_get_session_result" => self.tool_get_session_result(arguments),
            "termal_cancel_session" => self.tool_cancel_session(arguments),
            "termal_followup_session" => self.tool_followup_session(arguments),
            "termal_send_to_session" => self.tool_send_to_session(arguments),
            "termal_list_sessions" => self.tool_list_sessions(arguments),
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
        // `sessionId` is interpolated into the request path once resolved, so it gets the
        // same path-identifier validation every `delegationId` in this file already uses.
        // A session NAME containing `/`, `\`, `?`, `#` or `%` is rejected here rather than
        // resolved; callers target such a session by its id (termal_list_sessions shows it).
        let session_ref = required_path_identifier(arguments.get("sessionId"), "sessionId")?;
        let message = required_string(arguments.get("message"), "message")?;
        // Agents routinely pass a session NAME here ("LegalCodex") rather than a TermAl id,
        // so resolve a name to its id before delivering. A value that already looks like a
        // TermAl id ("session-…") is used directly.
        let session_id = self.resolve_session_reference(&session_ref)?;
        // Reuse the standard message path: it delivers immediately when the target session
        // is idle and queues on the target's pending-prompt FIFO when it is mid-turn. Fire-
        // and-forget — we do not wait for or return the target's reply. Discard the large
        // state snapshot the route returns and confirm compactly.
        // Attribute the delivered message to THIS bridge's own session id. The
        // receiving backend resolves it to our current display name, so the
        // peer sees "<sender name>" instead of "You". Using our own id (not a
        // caller-supplied value) is what keeps the attribution unspoofable.
        let response = self.post_json(
            &format!("/api/sessions/{}/messages", session_id),
            &json!({ "text": message, "sourceSessionId": self.parent_session_id }),
        )?;
        let disposition = response
            .get("messageDisposition")
            .and_then(Value::as_str)
            .unwrap_or("accepted");
        Ok(json!({
            "sessionId": session_id,
            "resolvedFrom": session_ref,
            "delivered": true,
            "queued": disposition == "queuedBehindActiveTurn",
            "disposition": disposition
        }))
    }

    /// Resolves a peer session reference (an id or a name) to a VALIDATED target id.
    ///
    /// A value prefixed `session-` is a TermAl id; anything else is a session NAME matched
    /// case-insensitively via /api/state, across ALL projects (peer sessions frequently live
    /// in different projects). This is why a bare name — and the external Codex thread uuid
    /// shown in the UI — 404 without it. Ambiguous names and no match both return a guiding
    /// error.
    ///
    /// BOTH paths resolve against the live session list and land in the SAME filter: the
    /// target must exist, be a ROOT session (never a delegation child), and never be the
    /// caller itself. An earlier revision returned any `session-`-prefixed input verbatim,
    /// which (a) skipped the root-only filter this comment promises — letting an id target a
    /// delegation child or the sender — and (b) fed unvalidated text into the request path,
    /// where the `url` crate resolves dot segments, so `session-x/../../sessions/victim/stop#`
    /// normalised to a different POST route. Resolving every reference through the session
    /// list closes both; the `required_path_identifier` check in the caller is the
    /// belt-and-braces half, and it is load-bearing because tm-36l proposes skipping this
    /// fetch for id-shaped arguments.
    fn resolve_session_reference(&self, reference: &str) -> Result<String> {
        let state = self.get_json("/api/state")?;
        let sessions = state
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let reference_is_id = reference.starts_with("session-");
        let mut matches = sessions
            .iter()
            // Root sessions only — a peer message never targets a delegation child.
            .filter(|session| session.get("parentDelegationId").map_or(true, Value::is_null))
            .filter(|session| {
                if reference_is_id {
                    session.get("id").and_then(Value::as_str) == Some(reference)
                } else {
                    session
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| name.eq_ignore_ascii_case(reference))
                }
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
        let sessions = state
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let listed = sessions
            .iter()
            // Root sessions only — exclude delegation children (they carry a parent id).
            .filter(|session| {
                session
                    .get("parentDelegationId")
                    .map_or(true, Value::is_null)
            })
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
    /// must not enumerate or message arbitrary sessions (tm-r0y). Fail SAFE: if the backend
    /// can't be reached or the caller can't be found, treat it as a child (deny peer tools).
    fn caller_is_delegation_child(&self) -> bool {
        let Ok(state) = self.get_json("/api/state") else {
            return true;
        };
        let Some(sessions) = state.get("sessions").and_then(Value::as_array) else {
            return true;
        };
        match sessions.iter().find(|session| {
            session.get("id").and_then(Value::as_str) == Some(self.parent_session_id.as_str())
        }) {
            Some(session) => session
                .get("parentDelegationId")
                .is_some_and(|value| !value.is_null()),
            None => true,
        }
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
                        .is_some_and(tool_is_peer_scoped)
                });
            }
        }
        result
    }

    fn get_json(&self, path: &str) -> Result<Value> {
        self.decode_response(self.client.get(self.url(path)).send())
    }

    fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.decode_response(self.client.post(self.url(path)).json(body).send())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn decode_response(
        &self,
        response: std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<Value> {
        let response = response.context("failed to call TermAl delegation API")?;
        let status = response.status();
        let text = response
            .text()
            .context("failed to read TermAl delegation API response")?;
        if status.is_success() {
            return serde_json::from_str(&text)
                .with_context(|| format!("failed to parse TermAl API response JSON: {text}"));
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

/// Peer tools operate on ARBITRARY root sessions (message / enumerate), so they are
/// restricted to root callers and hidden from / rejected for delegation children (tm-r0y).
fn tool_is_peer_scoped(name: &str) -> bool {
    matches!(name, "termal_send_to_session" | "termal_list_sessions")
}

fn mcp_tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "termal_spawn_session",
                "description": "Create a TermAl child delegation under the current parent session. Single-line prompts matching a known slash command are resolved before spawning.",
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
                        "agent": { "type": "string", "enum": ["Codex", "Claude", "Cursor", "Gemini"] },
                        "model": { "type": "string" },
                        "mode": { "type": "string", "enum": ["reviewer", "explorer", "worker"] },
                        "writePolicy": {
                            "oneOf": [
                                { "type": "string", "enum": ["readOnly", "isolatedWorktree", "sharedWorktree"] },
                                { "type": "object" }
                            ]
                        }
                    }
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
                "description": "Send a message to another root-level TermAl session. `sessionId` accepts either a TermAl id (session-…) OR a session NAME (e.g. \"LegalCodex\"), resolved case-insensitively across all projects. Delivered immediately if the target is idle, or queued on its pending-prompt FIFO if it is mid-turn. The result's `disposition` is `deliveredToIdleSession` or `queuedBehindActiveTurn`. FIRE-AND-FORGET — this is NOT a delegation: there is no result to await and nothing to poll. The call returns as soon as the message is delivered/queued; do NOT wait for a reply. If the target replies, it arrives LATER as a separate incoming message in your own session (sent via its own termal_send_to_session), never as a return value here. If a name is ambiguous, use termal_list_sessions and pass the exact id.",
                "inputSchema": {
                    "type": "object",
                    "required": ["sessionId", "message"],
                    "properties": {
                        "sessionId": { "type": "string" },
                        "message": { "type": "string" }
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
mod delegation_mcp_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[derive(Clone, Debug)]
    struct TestMcpHttpRequest {
        method: String,
        path: String,
        body: String,
    }

    fn read_test_mcp_http_request(stream: &mut std::net::TcpStream) -> TestMcpHttpRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let bytes_read = stream.read(&mut chunk).expect("request should read");
            assert!(bytes_read > 0, "request closed before headers completed");
            buffer.extend_from_slice(&chunk[..bytes_read]);
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break end;
            }
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let request_line = headers
            .lines()
            .next()
            .expect("request line should be present");
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts
            .next()
            .expect("request method should be present")
            .to_owned();
        let path = request_parts
            .next()
            .expect("request path should be present")
            .to_owned();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buffer.len() < body_start + content_length {
            let bytes_read = stream.read(&mut chunk).expect("request body should read");
            if bytes_read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes_read]);
        }
        let body =
            String::from_utf8_lossy(&buffer[body_start..body_start + content_length]).to_string();
        TestMcpHttpRequest { method, path, body }
    }

    fn write_test_mcp_http_json_response(
        stream: &mut std::net::TcpStream,
        status: u16,
        body: Value,
    ) {
        let body = body.to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .expect("response should write");
    }

    fn spawn_test_mcp_http_server(
        expected_requests: usize,
        handler: impl Fn(TestMcpHttpRequest) -> (u16, Value) + Send + Sync + 'static,
    ) -> (
        String,
        Arc<Mutex<Vec<TestMcpHttpRequest>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("test server address should be readable")
        );
        let handler = Arc::new(handler);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = requests.clone();
        let server = thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("test request should connect");
                let request = read_test_mcp_http_request(&mut stream);
                thread_requests
                    .lock()
                    .expect("request log mutex poisoned")
                    .push(request.clone());
                let (status, body) = handler(request);
                write_test_mcp_http_json_response(&mut stream, status, body);
            }
        });
        (base_url, requests, server)
    }

    #[test]
    fn delegation_mcp_tools_list_exposes_parent_scoped_tools_only() {
        let tools = mcp_tools_list_result();
        let names = tools
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools list should be an array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "termal_spawn_session",
                "termal_get_session_status",
                "termal_get_session_result",
                "termal_cancel_session",
                "termal_wait_delegations",
                "termal_resume_after_delegations",
                "termal_followup_session",
                "termal_send_to_session",
                "termal_list_sessions",
            ]
        );
        // termal_list_sessions is the intentional peer-discovery tool; guard that no OTHER
        // broadly-scoped "list" tool creeps in.
        assert_eq!(
            names
                .iter()
                .copied()
                .filter(|name| name.contains("list"))
                .collect::<Vec<_>>(),
            vec!["termal_list_sessions"]
        );
    }

    #[test]
    fn delegation_mcp_initialize_reports_tool_capability() {
        let result = mcp_initialize_result();
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some(TERMAL_DELEGATION_MCP_PROTOCOL_VERSION)
        );
        assert!(result.pointer("/capabilities/tools").is_some());
        assert_eq!(
            result.pointer("/serverInfo/name").and_then(Value::as_str),
            Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
        );
    }

    #[test]
    fn delegation_mcp_configs_bind_parent_session_and_base_url() {
        let command = "C:\\termal\\termal.exe";
        let parent = "session-parent";
        let base_url = "http://127.0.0.1:9999/";

        let claude =
            termal_delegation_mcp_claude_config_json_with_command(command, parent, base_url);
        let claude: Value = serde_json::from_str(&claude).expect("Claude config should be JSON");
        assert_eq!(
            claude.pointer("/mcpServers/termal-delegation/command"),
            Some(&Value::String(command.to_owned()))
        );
        assert_eq!(
            claude.pointer("/mcpServers/termal-delegation/args/2"),
            Some(&Value::String(parent.to_owned()))
        );
        assert_eq!(
            claude.pointer("/mcpServers/termal-delegation/args/4"),
            Some(&Value::String("http://127.0.0.1:9999".to_owned()))
        );

        let acp = termal_delegation_mcp_acp_servers_with_command(command, parent, base_url);
        assert_eq!(
            acp.pointer("/0/name").and_then(Value::as_str),
            Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
        );
        assert_eq!(
            acp.pointer("/0/args/2"),
            Some(&Value::String(parent.to_owned()))
        );

        let codex = termal_delegation_mcp_codex_config_with_command(command, parent, base_url);
        assert_eq!(
            codex.pointer("/mcp_servers/termal-delegation/args/2"),
            Some(&Value::String(parent.to_owned()))
        );
    }

    #[test]
    fn delegation_mcp_rejects_path_unsafe_parent_and_delegation_ids() {
        let err = match TermalDelegationMcpBridge::new(
            "session-parent/other".to_owned(),
            "http://127.0.0.1:9999".to_owned(),
        ) {
            Ok(_) => panic!("path-unsafe parent id should be rejected"),
            Err(err) => err,
        };
        assert!(err
            .to_string()
            .contains("delegation MCP parent session id must not contain"));

        let bridge = TermalDelegationMcpBridge::new(
            "session-parent".to_owned(),
            "http://127.0.0.1:9999".to_owned(),
        )
        .expect("path-safe parent id should be accepted");

        let err = bridge
            .tool_get_session_status(json!({ "delegationId": "delegation-bad/result" }))
            .expect_err("path-unsafe status delegation id should be rejected");
        assert!(err
            .to_string()
            .contains("delegationId must not contain /, \\, ?, #, %, or control characters"));

        let err = bridge
            .tool_wait_delegations(json!({
                "delegationIds": ["delegation-good", "delegation-bad?x"],
                "timeoutMs": 1
            }))
            .expect_err("path-unsafe wait delegation id should be rejected before polling");
        assert!(err
            .to_string()
            .contains("delegationIds must not contain /, \\, ?, #, %, or control characters"));

        let err = bridge
            .tool_get_session_status(json!({ "delegationId": "delegation%2Fbad" }))
            .expect_err("encoded slash delegation id should be rejected");
        assert!(err
            .to_string()
            .contains("delegationId must not contain /, \\, ?, #, %, or control characters"));

        let err = bridge
            .tool_get_session_status(json!({ "delegationId": ".." }))
            .expect_err("navigation-only delegation id should be rejected");
        assert!(err.to_string().contains("delegationId must not be . or .."));
    }

    #[test]
    fn parse_mcp_slash_command_prompt_pins_ui_compatible_shape() {
        let parsed = parse_mcp_slash_command_prompt("/review-code staged -- include tests")
            .expect("valid slash command should parse");
        assert_eq!(parsed.command_name, "review-code");
        assert_eq!(parsed.arguments.as_deref(), Some("staged"));
        assert_eq!(parsed.note.as_deref(), Some("include tests"));

        let parsed = parse_mcp_slash_command_prompt("/review-code   ")
            .expect("trailing whitespace should not prevent parsing");
        assert_eq!(parsed.command_name, "review-code");
        assert_eq!(parsed.arguments, None);
        assert_eq!(parsed.note, None);

        let parsed = parse_mcp_slash_command_prompt("/review-code staged -- include tests\r")
            .expect("trailing carriage return should be trimmed like other trailing whitespace");
        assert_eq!(parsed.command_name, "review-code");
        assert_eq!(parsed.arguments.as_deref(), Some("staged"));
        assert_eq!(parsed.note.as_deref(), Some("include tests"));

        for prompt in [
            " /review-code",
            "/ review-code",
            "/",
            "/review/local",
            "/review-code\nextra",
            "/review-code\rextra",
            "review-code",
        ] {
            assert!(
                parse_mcp_slash_command_prompt(prompt).is_none(),
                "`{prompt}` should not be treated as an MCP slash command"
            );
        }
    }

    #[test]
    fn split_mcp_agent_command_tail_pins_note_separator_edges() {
        let cases = [
            ("", None, None),
            ("staged", Some("staged"), None),
            ("staged -- include tests", Some("staged"), Some("include tests")),
            ("--", None, None),
            ("  --  ", None, None),
            ("-- include tests", None, Some("include tests")),
            ("staged --", Some("staged"), None),
            ("staged -- -- second", Some("staged"), Some("-- second")),
            ("staged ---x", Some("staged ---x"), None),
            ("staged-- include tests", Some("staged-- include tests"), None),
            ("  staged   --   include tests  ", Some("staged"), Some("include tests")),
            ("\tstaged\t--\tinclude tests\t", Some("staged"), Some("include tests")),
            ("\u{2003}staged\u{2003}", Some("staged"), None),
            (
                "staged\u{2003}--\u{2003}include tests",
                Some("staged\u{2003}--\u{2003}include tests"),
                None,
            ),
        ];

        for (tail, expected_arguments, expected_note) in cases {
            let (arguments, note) = split_mcp_agent_command_tail(tail);
            assert_eq!(
                arguments.as_deref(),
                expected_arguments,
                "arguments mismatch for `{tail}`"
            );
            assert_eq!(
                note.as_deref(),
                expected_note,
                "note mismatch for `{tail}`"
            );
        }
    }

    #[test]
    fn delegation_mcp_list_sessions_returns_root_sessions_only() {
        let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/api/state");
            (
                200,
                json!({
                    "sessions": [
                        { "id": "session-root-a", "name": "HelloMe", "agent": "Codex", "status": "idle", "workdir": "C:/a", "preview": "hi" },
                        { "id": "session-root-b", "name": "HelloMe2", "agent": "Codex", "status": "active", "workdir": "C:/b", "preview": "yo" },
                        { "id": "session-child", "name": "Codex /review-code", "agent": "Codex", "status": "idle", "parentDelegationId": "delegation-x" }
                    ]
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_list_sessions(json!({}))
            .expect("list should succeed");
        let sessions = response
            .get("sessions")
            .and_then(Value::as_array)
            .expect("sessions should be an array");
        let ids = sessions
            .iter()
            .filter_map(|session| session.get("sessionId").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["session-root-a", "session-root-b"]);
        assert_eq!(sessions[1]["name"], "HelloMe2");
        server.join().expect("test server should join");
    }

    #[test]
    fn delegation_mcp_hides_and_rejects_peer_tools_for_delegation_child() {
        let (base_url, _requests, server) = spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.method, "GET");
            assert_eq!(request.path, "/api/state");
            (
                200,
                json!({
                    "sessions": [
                        { "id": "session-parent", "name": "Reviewer", "parentDelegationId": "delegation-x" }
                    ]
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        // tools/list omits the peer tools for a delegation-child caller...
        let names = bridge
            .tools_list_for_caller()
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name == "termal_send_to_session"));
        assert!(!names.iter().any(|name| name == "termal_list_sessions"));
        assert!(
            names.iter().any(|name| name == "termal_spawn_session"),
            "delegation tools stay available to children"
        );

        // ...and invoking one through the dispatch is rejected.
        let err = bridge
            .handle_tool_call(json!({
                "name": "termal_send_to_session",
                "arguments": { "sessionId": "session-x", "message": "hi" }
            }))
            .expect_err("a delegation child must not invoke a peer tool");
        assert!(
            err.to_string().contains("root sessions"),
            "error should explain the root-only restriction: {err}"
        );
        server.join().expect("test server should join");
    }

    #[test]
    fn delegation_mcp_send_to_session_resolves_name_across_projects() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (
                    200,
                    json!({
                        "sessions": [
                            { "id": "session-kadry", "name": "Kadry", "projectId": "project-kadry" },
                            { "id": "session-legal", "name": "LegalCodex", "projectId": "project-rincon" }
                        ]
                    }),
                ),
                ("POST", "/api/sessions/session-legal/messages") => {
                    let body: Value =
                        serde_json::from_str(&request.body).expect("send body should be JSON");
                    assert_eq!(body["text"], "hi legal");
                    (
                        202,
                        json!({
                            "revision": 1,
                            "sessions": [],
                            "messageDisposition": "deliveredToIdleSession"
                        }),
                    )
                }
                _ => (
                    404,
                    json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
                ),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_send_to_session(json!({ "sessionId": "LegalCodex", "message": "hi legal" }))
            .expect("send by name should resolve + deliver");
        assert_eq!(response["sessionId"], "session-legal");
        assert_eq!(response["resolvedFrom"], "LegalCodex");
        assert_eq!(response["delivered"], true);
        assert_eq!(response["queued"], false);
        assert_eq!(response["disposition"], "deliveredToIdleSession");
        server.join().expect("test server should join");
        let requests = requests.lock().expect("request log mutex poisoned");
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn delegation_mcp_send_to_session_unknown_name_errors() {
        let (base_url, _requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.path, "/api/state");
            (
                200,
                json!({ "sessions": [ { "id": "session-a", "name": "Alpha", "projectId": "p" } ] }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let err = bridge
            .tool_send_to_session(json!({ "sessionId": "Nonexistent", "message": "hi" }))
            .expect_err("unknown name should error");
        assert!(
            err.to_string().contains("termal_list_sessions"),
            "error should guide to termal_list_sessions: {err}"
        );
        server.join().expect("test server should join");
    }

    // An id-shaped reference now resolves through /api/state like a name does, so that the
    // root-only/non-self filter applies to BOTH paths (tm-88r). That is why this exercises
    // two requests: the resolve, then the delivery.
    #[test]
    fn delegation_mcp_send_to_session_posts_message_to_target() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (
                    200,
                    json!({
                        "sessions": [
                            { "id": "session-peer", "name": "Peer", "projectId": "p" }
                        ]
                    }),
                ),
                ("POST", "/api/sessions/session-peer/messages") => {
                    let body: Value =
                        serde_json::from_str(&request.body).expect("send body should be JSON");
                    assert_eq!(body["text"], "hello peer");
                    (
                        202,
                        json!({
                            "revision": 1,
                            "sessions": [],
                            "messageDisposition": "queuedBehindActiveTurn"
                        }),
                    )
                }
                _ => (
                    404,
                    json!({ "error": format!("unexpected {} {}", request.method, request.path) }),
                ),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_send_to_session(json!({
                "sessionId": "session-peer",
                "message": "hello peer"
            }))
            .expect("send-to-session should succeed");

        assert_eq!(response["sessionId"], "session-peer");
        assert_eq!(response["delivered"], true);
        assert_eq!(response["queued"], true);
        assert_eq!(response["disposition"], "queuedBehindActiveTurn");
        server.join().expect("test server should join");
        let requests = requests.lock().expect("request log mutex poisoned");
        assert_eq!(requests.len(), 2);
    }

    // tm-88r: `sessionId` is interpolated into the request path, and the `url` crate
    // resolves dot segments — so an unvalidated `session-`-prefixed reference turned
    // termal_send_to_session into a POST primitive against arbitrary routes. The path
    // validator must reject the traversal shape BEFORE any request is issued.
    // Neuter-verified: swapping `required_path_identifier` back to `required_string` makes
    // this fail (the reference reaches the resolver instead of being rejected outright).
    #[test]
    fn delegation_mcp_send_to_session_rejects_path_traversal_reference() {
        // Zero expected requests: rejection must happen before any HTTP call.
        let (base_url, requests, server) = spawn_test_mcp_http_server(0, move |request| {
            (
                500,
                json!({ "error": format!("no request expected: {} {}", request.method, request.path) }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        for reference in [
            "session-x/../../sessions/victim/stop#",
            "session-x/../../sessions/victim/stop",
            "session-a%2f..%2fvictim",
            "../session-victim",
            "session-x\\..\\victim",
        ] {
            let err = bridge
                .tool_send_to_session(json!({ "sessionId": reference, "message": "hi" }))
                .expect_err("a path-traversal reference must be rejected");
            assert!(
                err.to_string().contains("sessionId must not contain"),
                "reference `{reference}` should be rejected by the path validator: {err}"
            );
        }

        server.join().expect("test server should join");
        let requests = requests.lock().expect("request log mutex poisoned");
        assert!(
            requests.is_empty(),
            "a rejected reference must not reach the backend: {requests:?}"
        );
    }

    // tm-88r: the id path used to return any `session-`-prefixed value verbatim, skipping
    // the root-only filter its own comment promised. A delegation child is not a peer.
    #[test]
    fn delegation_mcp_send_to_session_rejects_delegation_child_and_self_targets() {
        let (base_url, _requests, server) = spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.path, "/api/state", "no delivery should be attempted");
            (
                200,
                json!({
                    "sessions": [
                        { "id": "session-parent", "name": "Me", "projectId": "p" },
                        {
                            "id": "session-child",
                            "name": "Child",
                            "projectId": "p",
                            "parentDelegationId": "delegation-1"
                        }
                    ]
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let child_err = bridge
            .tool_send_to_session(json!({ "sessionId": "session-child", "message": "hi" }))
            .expect_err("a delegation child must not be a peer target");
        assert!(
            child_err.to_string().contains("no root session"),
            "child target should be filtered out by the root-only filter: {child_err}"
        );

        let self_err = bridge
            .tool_send_to_session(json!({ "sessionId": "session-parent", "message": "hi" }))
            .expect_err("a session must not peer-message itself");
        assert!(
            self_err.to_string().contains("is this session"),
            "self target should be rejected explicitly: {self_err}"
        );

        server.join().expect("test server should join");
    }

    #[test]
    fn delegation_mcp_spawn_session_posts_parent_scoped_request() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/api/sessions/session-parent/delegations");
            let body: Value =
                serde_json::from_str(&request.body).expect("spawn body should be JSON");
            assert_eq!(body["prompt"], "Review this patch");
            assert_eq!(body["title"], "Codex review");
            assert_eq!(body["cwd"], "C:\\repo");
            assert_eq!(body["agent"], "Codex");
            assert_eq!(body["model"], "gpt-5.4");
            assert_eq!(body["mode"], "reviewer");
            assert_eq!(body.pointer("/writePolicy/kind"), Some(&json!("readOnly")));
            (
                200,
                json!({
                    "delegation": {
                        "id": "delegation-one",
                        "status": "running"
                    },
                    "childSessionId": "session-child"
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_spawn_session(json!({
                "prompt": "Review this patch",
                "title": "Codex review",
                "cwd": "C:\\repo",
                "agent": "Codex",
                "model": "gpt-5.4",
                "mode": "reviewer",
                "writePolicy": "readOnly"
            }))
            .expect("spawn should post delegation request");

        assert_eq!(response.pointer("/delegation/id"), Some(&json!("delegation-one")));
        assert_eq!(response["childSessionId"], "session-child");
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_spawn_session_resolves_known_slash_command_prompt() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/review-code/resolve") => {
                    let body: Value =
                        serde_json::from_str(&request.body).expect("resolve body should be JSON");
                    assert_eq!(body["arguments"], "staged");
                    assert_eq!(body["note"], "include tests");
                    assert_eq!(body["cwd"], "C:\\repo\\child");
                    assert_eq!(body["intent"], "delegate");
                    (
                        200,
                        json!({
                            "name": "review-code",
                            "visiblePrompt": "/review-code staged",
                            "expandedPrompt": "Expanded review-code command body",
                            "title": "Review local changes",
                            "delegation": {
                                "mode": "explorer",
                                "writePolicy": { "kind": "isolatedWorktree", "ownedPaths": [] }
                            }
                        }),
                    )
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    let body: Value = serde_json::from_str(&request.body)
                        .expect("delegation body should be JSON");
                    assert_eq!(body["prompt"], "Expanded review-code command body");
                    assert_eq!(body["title"], "Review local changes");
                    assert_eq!(body["cwd"], "C:\\repo\\child");
                    assert_eq!(body["mode"], "explorer");
                    assert_eq!(
                        body.pointer("/writePolicy/kind"),
                        Some(&json!("isolatedWorktree"))
                    );
                    (
                        200,
                        json!({
                            "delegation": {
                                "id": "delegation-one",
                                "status": "running"
                            },
                            "childSessionId": "session-child"
                        }),
                    )
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code staged -- include tests",
                "agent": "Codex",
                "cwd": "C:\\repo\\child"
            }))
            .expect("spawn should resolve the slash command then post delegation request");

        assert_eq!(response.pointer("/delegation/id"), Some(&json!("delegation-one")));
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_preserves_literal_prompt_for_unknown_slash_command() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/unknown/resolve") => {
                    (
                        404,
                        json!({
                            "error": "agent command not found"
                        }),
                    )
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    let body: Value = serde_json::from_str(&request.body)
                        .expect("delegation body should be JSON");
                    assert_eq!(body["prompt"], "/unknown keep literal");
                    (
                        200,
                        json!({
                            "delegation": {
                                "id": "delegation-one",
                                "status": "running"
                            },
                            "childSessionId": "session-child"
                        }),
                    )
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/unknown keep literal"
            }))
            .expect("unknown slash-like prompts should remain literal");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_surfaces_non_command_not_found_resolve_errors() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/agent-commands/review-code/resolve"
            );
            (
                404,
                json!({
                    "error": "session not found"
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let err = bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code"
            }))
            .expect_err("non-command 404 should surface");

        assert!(err
            .to_string()
            .contains("TermAl delegation API returned 404 Not Found: session not found"));
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_spawn_session_encodes_slash_command_path_segment() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/review%3Alocal/resolve") => {
                    (
                        200,
                        json!({
                            "name": "review:local",
                            "visiblePrompt": "/review:local",
                            "expandedPrompt": "Expanded colon command"
                        }),
                    )
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    let body: Value = serde_json::from_str(&request.body)
                        .expect("delegation body should be JSON");
                    assert_eq!(body["prompt"], "Expanded colon command");
                    (
                        200,
                        json!({
                            "delegation": {
                                "id": "delegation-one",
                                "status": "running"
                            },
                            "childSessionId": "session-child"
                        }),
                    )
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/review:local"
            }))
            .expect("command names should be encoded as a path segment");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_allows_percent_in_encoded_command_name() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/review%25local/resolve") => {
                    (
                        200,
                        json!({
                            "name": "review%local",
                            "visiblePrompt": "/review%local",
                            "expandedPrompt": "Expanded percent command"
                        }),
                    )
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    let body: Value = serde_json::from_str(&request.body)
                        .expect("delegation body should be JSON");
                    assert_eq!(body["prompt"], "Expanded percent command");
                    (
                        200,
                        json!({
                            "delegation": {
                                "id": "delegation-one",
                                "status": "running"
                            },
                            "childSessionId": "session-child"
                        }),
                    )
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/review%local"
            }))
            .expect("literal percent command names should be encoded as a path segment");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_rejects_parent_known_command_missing_from_requested_cwd() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/agent-commands/review-code/resolve"
            );
            let body: Value =
                serde_json::from_str(&request.body).expect("resolve body should be JSON");
            if body.get("cwd").is_some() {
                return (
                    404,
                    json!({
                        "error": "agent command not found"
                    }),
                );
            }
            (
                200,
                json!({
                    "name": "review-code",
                    "visiblePrompt": "/review-code",
                    "expandedPrompt": "Parent-scope review command"
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let err = bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code",
                "cwd": "C:\\repo\\child"
            }))
            .expect_err("parent-known command missing from requested cwd should fail");

        assert!(err
            .to_string()
            .contains("agent command `review-code` was not found in requested cwd"));
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_preserves_multiline_slash_like_prompt() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/api/sessions/session-parent/delegations");
            let body: Value =
                serde_json::from_str(&request.body).expect("delegation body should be JSON");
            assert_eq!(body["prompt"], "/review-code\nleave this literal");
            (
                200,
                json!({
                    "delegation": {
                        "id": "delegation-one",
                        "status": "running"
                    },
                    "childSessionId": "session-child"
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code\nleave this literal"
            }))
            .expect("multiline prompts should not be slash-expanded");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_spawn_session_preserves_spaced_slash_like_prompt() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/api/sessions/session-parent/delegations");
            let body: Value =
                serde_json::from_str(&request.body).expect("delegation body should be JSON");
            assert_eq!(body["prompt"], "/ review-code");
            (
                200,
                json!({
                    "delegation": {
                        "id": "delegation-one",
                        "status": "running"
                    },
                    "childSessionId": "session-child"
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/ review-code"
            }))
            .expect("slash followed by whitespace should stay literal like the UI parser");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_spawn_session_explicit_options_override_resolved_defaults() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/sessions/session-parent/agent-commands/review-code/resolve") => {
                    let body: Value =
                        serde_json::from_str(&request.body).expect("resolve body should be JSON");
                    assert!(body.get("arguments").is_none());
                    assert!(body.get("note").is_none());
                    assert_eq!(body["intent"], "delegate");
                    (
                        200,
                        json!({
                            "name": "review-code",
                            "visiblePrompt": "/review-code",
                            "expandedPrompt": "Expanded review-code command body",
                            "title": "Resolved title",
                            "delegation": {
                                "mode": "explorer",
                                "writePolicy": { "kind": "isolatedWorktree", "ownedPaths": [] }
                            }
                        }),
                    )
                }
                ("POST", "/api/sessions/session-parent/delegations") => {
                    let body: Value = serde_json::from_str(&request.body)
                        .expect("delegation body should be JSON");
                    assert_eq!(body["prompt"], "Expanded review-code command body");
                    assert_eq!(body["title"], "Explicit title");
                    assert_eq!(body["mode"], "reviewer");
                    assert_eq!(body.pointer("/writePolicy/kind"), Some(&json!("readOnly")));
                    (
                        200,
                        json!({
                            "delegation": {
                                "id": "delegation-one",
                                "status": "running"
                            },
                            "childSessionId": "session-child"
                        }),
                    )
                }
                other => panic!("unexpected request: {other:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code",
                "title": "Explicit title",
                "mode": "reviewer",
                "writePolicy": "readOnly"
            }))
            .expect("explicit spawn options should override resolved defaults");

        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 2);
    }

    #[test]
    fn delegation_mcp_spawn_session_rejects_empty_resolved_prompt() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/agent-commands/review-code/resolve"
            );
            (
                200,
                json!({
                    "name": "review-code",
                    "visiblePrompt": " ",
                    "expandedPrompt": ""
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let err = bridge
            .tool_spawn_session(json!({
                "prompt": "/review-code"
            }))
            .expect_err("empty resolved prompts should be rejected before spawning");

        assert!(err
            .to_string()
            .contains("agent command `review-code` resolved without prompt content"));
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_resume_after_delegations_posts_backend_wait() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/api/sessions/session-parent/delegation-waits");
            let body: Value =
                serde_json::from_str(&request.body).expect("resume wait body should be JSON");
            assert_eq!(
                body["delegationIds"],
                json!(["delegation-codex", "delegation-claude"])
            );
            assert_eq!(body["mode"], "all");
            assert_eq!(body["title"], "Delegated review fan-in");
            (
                200,
                json!({
                    "waitId": "delegation-wait-one",
                    "mode": "all",
                    "queued": true
                }),
            )
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_resume_after_delegations(json!({
                "delegationIds": ["delegation-codex", "delegation-claude"],
                "mode": "all",
                "title": "Delegated review fan-in"
            }))
            .expect("resume wait should post request");

        assert_eq!(response["waitId"], "delegation-wait-one");
        assert_eq!(response["queued"], true);
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_tools_call_wraps_api_result_as_text_content() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "GET");
            assert_eq!(
                request.path,
                "/api/sessions/session-parent/delegations/delegation-one"
            );
            (200, json!({ "delegation": { "status": "completed" } }))
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .handle_single_message(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "termal_get_session_status",
                    "arguments": {
                        "delegationId": "delegation-one"
                    }
                }
            }))
            .expect("tools/call should handle request")
            .expect("tools/call should return a response");

        assert_eq!(response["id"], 7);
        assert_eq!(response.pointer("/result/isError"), Some(&json!(false)));
        let text = response
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .expect("tool response should contain JSON text");
        let payload: Value = serde_json::from_str(text).expect("tool text should be JSON");
        assert_eq!(payload.pointer("/delegation/status"), Some(&json!("completed")));
        server.join().expect("test server should join");
        assert_eq!(requests.lock().expect("request log mutex poisoned").len(), 1);
    }

    #[test]
    fn delegation_mcp_wait_polls_until_terminal_then_fetches_result() {
        let status_calls = Arc::new(AtomicUsize::new(0));
        let handler_status_calls = status_calls.clone();
        let (base_url, requests, server) = spawn_test_mcp_http_server(3, move |request| {
            assert_eq!(request.method, "GET");
            assert!(request.body.is_empty());
            match request.path.as_str() {
                "/api/sessions/session-parent/delegations/delegation-done" => {
                    let call = handler_status_calls.fetch_add(1, Ordering::SeqCst);
                    let status = if call == 0 { "running" } else { "completed" };
                    (200, json!({ "delegation": { "status": status } }))
                }
                "/api/sessions/session-parent/delegations/delegation-done/result" => (
                    200,
                    json!({
                        "result": {
                            "status": "completed",
                            "summary": "MCP wait observed completion."
                        }
                    }),
                ),
                _ => (
                    404,
                    json!({ "error": format!("unexpected path {}", request.path) }),
                ),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_wait_delegations(json!({
                "delegationIds": ["delegation-done"],
                "mode": "all",
                "pollIntervalMs": 100,
                "timeoutMs": 2_000
            }))
            .expect("wait should complete");

        assert_eq!(response["timedOut"], false);
        assert_eq!(
            response.pointer("/statuses/0/delegation/status"),
            Some(&Value::String("completed".to_owned()))
        );
        assert_eq!(
            response.pointer("/results/0/result/result/summary"),
            Some(&Value::String("MCP wait observed completion.".to_owned()))
        );
        assert_eq!(status_calls.load(Ordering::SeqCst), 2);
        server.join().expect("test server should join");
        let requests = requests.lock().expect("request log mutex poisoned");
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .any(|request| request.path.ends_with("/delegation-done/result")),
            "terminal wait must fetch the result packet after status turns terminal"
        );
    }

    #[test]
    fn delegation_mcp_wait_treats_completed_failed_and_canceled_as_terminal() {
        let (base_url, requests, server) = spawn_test_mcp_http_server(6, move |request| {
            assert_eq!(request.method, "GET");
            assert!(request.body.is_empty());
            match request.path.as_str() {
                "/api/sessions/session-parent/delegations/delegation-completed" => {
                    (200, json!({ "delegation": { "status": "completed" } }))
                }
                "/api/sessions/session-parent/delegations/delegation-failed" => {
                    (200, json!({ "delegation": { "status": "failed" } }))
                }
                "/api/sessions/session-parent/delegations/delegation-canceled" => {
                    (200, json!({ "delegation": { "status": "canceled" } }))
                }
                "/api/sessions/session-parent/delegations/delegation-completed/result" => (
                    200,
                    json!({ "result": { "status": "completed", "summary": "completed" } }),
                ),
                "/api/sessions/session-parent/delegations/delegation-failed/result" => (
                    200,
                    json!({ "result": { "status": "failed", "summary": "failed" } }),
                ),
                "/api/sessions/session-parent/delegations/delegation-canceled/result" => (
                    200,
                    json!({ "result": { "status": "canceled", "summary": "canceled" } }),
                ),
                _ => (
                    404,
                    json!({ "error": format!("unexpected path {}", request.path) }),
                ),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".to_owned(), base_url)
            .expect("bridge should initialize");

        let response = bridge
            .tool_wait_delegations(json!({
                "delegationIds": [
                    "delegation-completed",
                    "delegation-failed",
                    "delegation-canceled"
                ],
                "mode": "all",
                "pollIntervalMs": 100,
                "timeoutMs": 2_000
            }))
            .expect("wait should complete");

        assert_eq!(response["timedOut"], false);
        assert_eq!(
            response.pointer("/statuses/0/delegation/status"),
            Some(&Value::String("completed".to_owned()))
        );
        assert_eq!(
            response.pointer("/statuses/1/delegation/status"),
            Some(&Value::String("failed".to_owned()))
        );
        assert_eq!(
            response.pointer("/statuses/2/delegation/status"),
            Some(&Value::String("canceled".to_owned()))
        );
        let results = response
            .get("results")
            .and_then(Value::as_array)
            .expect("results should be an array");
        assert_eq!(results.len(), 3);
        assert!(
            results.iter().all(|result| result.get("error").is_none()),
            "all terminal statuses should get result fetch attempts without synthetic errors"
        );
        server.join().expect("test server should join");
        let requests = requests.lock().expect("request log mutex poisoned");
        assert_eq!(requests.len(), 6);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path.ends_with("/result"))
                .count(),
            3
        );
    }
}
