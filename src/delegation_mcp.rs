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
        let parent_session_id = parent_session_id.trim().to_owned();
        if parent_session_id.is_empty() {
            bail!("delegation MCP parent session id cannot be empty");
        }
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
            "tools/list" => Ok(mcp_tools_list_result()),
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
        let result = match name.as_str() {
            "termal_spawn_session" => self.tool_spawn_session(arguments),
            "termal_get_session_status" => self.tool_get_session_status(arguments),
            "termal_get_session_result" => self.tool_get_session_result(arguments),
            "termal_cancel_session" => self.tool_cancel_session(arguments),
            "termal_wait_delegations" => self.tool_wait_delegations(arguments),
            "termal_resume_after_delegations" => self.tool_resume_after_delegations(arguments),
            other => Err(anyhow!("unknown TermAl delegation MCP tool `{other}`")),
        }?;
        Ok(mcp_tool_text_result(&result, false))
    }

    fn tool_spawn_session(&self, arguments: Value) -> Result<Value> {
        let prompt = required_string(arguments.get("prompt"), "prompt")?;
        let mut body = serde_json::Map::new();
        body.insert("prompt".to_owned(), Value::String(prompt));
        insert_optional_string(&mut body, "title", arguments.get("title"));
        insert_optional_string(&mut body, "cwd", arguments.get("cwd"));
        insert_optional_string(&mut body, "agent", arguments.get("agent"));
        insert_optional_string(&mut body, "model", arguments.get("model"));
        body.insert(
            "mode".to_owned(),
            optional_string(arguments.get("mode"))
                .map(Value::String)
                .unwrap_or_else(|| Value::String("reviewer".to_owned())),
        );
        body.insert(
            "writePolicy".to_owned(),
            normalize_mcp_write_policy(arguments.get("writePolicy")),
        );
        self.post_json(
            &format!("/api/sessions/{}/delegations", self.parent_session_id),
            &Value::Object(body),
        )
    }

    fn tool_get_session_status(&self, arguments: Value) -> Result<Value> {
        let delegation_id = required_string(arguments.get("delegationId"), "delegationId")?;
        self.get_json(&format!(
            "/api/sessions/{}/delegations/{}",
            self.parent_session_id, delegation_id
        ))
    }

    fn tool_get_session_result(&self, arguments: Value) -> Result<Value> {
        let delegation_id = required_string(arguments.get("delegationId"), "delegationId")?;
        self.get_json(&format!(
            "/api/sessions/{}/delegations/{}/result",
            self.parent_session_id, delegation_id
        ))
    }

    fn tool_cancel_session(&self, arguments: Value) -> Result<Value> {
        let delegation_id = required_string(arguments.get("delegationId"), "delegationId")?;
        self.post_json(
            &format!(
                "/api/sessions/{}/delegations/{}/cancel",
                self.parent_session_id, delegation_id
            ),
            &json!({}),
        )
    }

    fn tool_resume_after_delegations(&self, arguments: Value) -> Result<Value> {
        let delegation_ids =
            required_string_array(arguments.get("delegationIds"), "delegationIds")?;
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
            required_string_array(arguments.get("delegationIds"), "delegationIds")?;
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
        bail!("TermAl delegation API returned {status}: {message}")
    }
}

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

fn mcp_tools_list_result() -> Value {
    json!({
        "tools": [
            {
                "name": "termal_spawn_session",
                "description": "Create a TermAl child delegation under the current parent session.",
                "inputSchema": {
                    "type": "object",
                    "required": ["prompt"],
                    "properties": {
                        "prompt": { "type": "string" },
                        "title": { "type": "string" },
                        "cwd": { "type": "string" },
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
) {
    if let Some(value) = optional_string(value) {
        object.insert(key.to_owned(), Value::String(value));
    }
}

fn required_string_array(value: Option<&Value>, label: &str) -> Result<Vec<String>> {
    let array = value
        .and_then(Value::as_array)
        .with_context(|| format!("{label} must be an array"))?;
    let mut values = Vec::new();
    for item in array {
        let value = required_string(Some(item), label)?;
        values.push(value);
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
            ]
        );
        assert!(!names.iter().any(|name| name.contains("list")));
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
