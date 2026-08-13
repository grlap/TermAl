// Codex MCP status discovery for the composer `/mcp` command.
//
// This is intentionally a status/read path, not an agent prompt. Codex's TUI
// handles `/mcp` locally, so forwarding that text as a turn would ask the model
// to interpret a client command it never owns. TermAl instead queries the same
// app-server authority (`mcpServerStatus/list`) and returns a narrow, sanitized
// view to the browser.

const CODEX_MCP_STATUS_PAGE_LIMIT: u64 = 100;
const CODEX_MCP_STATUS_MAX_PAGES: usize = 50;
const CODEX_MCP_STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const CODEX_MCP_STATUS_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

fn codex_mcp_request_timeout(
    deadline: std::time::Instant,
) -> Result<Duration, ApiError> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(ApiError::internal(
            "Codex MCP status exceeded the overall request deadline",
        ));
    }
    Ok(remaining.min(CODEX_MCP_STATUS_TIMEOUT))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexMcpServerStatusPage {
    #[serde(default)]
    data: Vec<CodexMcpServerStatusEntry>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexMcpServerStatusEntry {
    name: String,
    auth_status: String,
    #[serde(default)]
    tools: BTreeMap<String, CodexMcpToolStatusEntry>,
}

#[derive(Debug, Deserialize)]
struct CodexMcpToolStatusEntry {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

impl From<CodexMcpServerStatusEntry> for CodexMcpServerStatus {
    fn from(server: CodexMcpServerStatusEntry) -> Self {
        let mut tools = server
            .tools
            .into_values()
            .map(|tool| CodexMcpToolSummary {
                name: tool.name,
                title: tool.title,
                description: tool.description,
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            name: server.name,
            auth_status: server.auth_status,
            tools,
        }
    }
}

impl AppState {
    /// Lists MCP servers from the Codex app-server that owns `session_id`.
    ///
    /// Local sessions query the shared app-server directly. Remote proxy
    /// sessions forward the same read to their owning TermAl, so the inventory
    /// always describes the runtime that will execute the next Codex turn.
    fn list_codex_mcp_servers(
        &self,
        session_id: &str,
    ) -> Result<CodexMcpServersResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_list_codex_mcp_servers(session_id);
        }

        let agent = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner
                .session_by_index(index)
                .expect("session index should be valid")
                .session
                .agent
        };
        if agent != Agent::Codex {
            return Err(ApiError::bad_request(
                "MCP server status is only available for Codex sessions",
            ));
        }

        let mut servers = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let deadline = std::time::Instant::now() + CODEX_MCP_STATUS_TOTAL_TIMEOUT;

        for _ in 0..CODEX_MCP_STATUS_MAX_PAGES {
            let mut params = serde_json::Map::new();
            params.insert(
                "detail".to_owned(),
                Value::String("toolsAndAuthOnly".to_owned()),
            );
            params.insert(
                "limit".to_owned(),
                Value::Number(CODEX_MCP_STATUS_PAGE_LIMIT.into()),
            );
            if let Some(cursor) = &cursor {
                params.insert("cursor".to_owned(), Value::String(cursor.clone()));
            }

            let result = self.perform_codex_json_rpc_request(
                "mcpServerStatus/list",
                Value::Object(params),
                codex_mcp_request_timeout(deadline)?,
            )?;
            let page: CodexMcpServerStatusPage = serde_json::from_value(result).map_err(|err| {
                ApiError::internal(format!(
                    "Codex request `mcpServerStatus/list` returned an invalid response: {err}"
                ))
            })?;
            servers.extend(page.data.into_iter().map(CodexMcpServerStatus::from));

            let Some(next_cursor) = page.next_cursor.filter(|value| !value.is_empty()) else {
                servers.sort_by(|left, right| left.name.cmp(&right.name));
                return Ok(CodexMcpServersResponse { servers });
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(ApiError::internal(
                    "Codex MCP status pagination repeated a cursor",
                ));
            }
            cursor = Some(next_cursor);
        }

        Err(ApiError::internal(
            "Codex MCP status exceeded the pagination safety limit",
        ))
    }
}
