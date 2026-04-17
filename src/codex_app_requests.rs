// Codex app-server request + item event handlers.
//
// The shared Codex runtime pushes two categories of payloads that
// need UI-side rendering work on TermAl's end:
//
// 1. **Requests** (JSON-RPC calls where Codex expects TermAl to
//    respond): command-execution approvals, file-change approvals,
//    permissions approvals, user-input questions, MCP elicitations.
//    Each is captured in a typed `CodexPending*` register on the
//    session record so the user-facing "approve / reject / respond"
//    UI flow can later look up the request and route the answer
//    back. `handle_codex_app_server_request` is the dispatcher;
//    `describe_codex_permission_request`,
//    `describe_codex_user_input_request`, and
//    `describe_codex_mcp_elicitation_request` format the
//    human-readable summaries that land in the session sidebar.
//    `describe_codex_app_server_request` is the fallback formatter
//    for methods that don't have purpose-built handling.
//
// 2. **Item lifecycle events** (fire-and-forget notifications about
//    work units): `item/started` and `item/completed`. These cover
//    agent messages, command executions, file edits, web searches,
//    MCP tool invocations, etc. The `started` handler registers
//    spinners / running entries on the recorder; the `completed`
//    handler finalizes them with output, diff, status, or error.
//
// The approval-and-response plumbing that closes the loop (reads
// the user's approve/reject decision and actually sends it back to
// Codex) lives in `codex_submissions.rs`. The text-streaming half
// of `item/completed` for agent messages lives in
// `codex_text_stream.rs`. This file is just the dispatcher + the
// per-kind describers.


/// Dispatches an inbound Codex server request that expects a response
/// back. Recognizes four purpose-built kinds — command-execution
/// approval, file-change approval, permissions approval, user-input
/// request, and MCP elicitation — and falls back to a generic
/// `CodexPendingAppRequest` for any other method. Each branch writes
/// a `Message` through the recorder describing the request for the
/// UI sidebar; the request id is retained so the later approval/response
/// plumbing can match the reply back to Codex.
fn handle_codex_app_server_request(
    method: &str,
    message: &Value,
    recorder: &mut impl CodexTurnRecorder,
) -> Result<()> {
    let request_id = message
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("Codex app-server request missing id"))?;
    let params = message
        .get("params")
        .ok_or_else(|| anyhow!("Codex app-server request missing params"))?;

    match method {
        "item/commandExecution/requestApproval" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Command execution");
            let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or("");
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if cwd.is_empty() && reason.is_empty() {
                "Codex requested approval to execute a command.".to_owned()
            } else if reason.is_empty() {
                format!("Codex requested approval to execute this command in {cwd}.")
            } else if cwd.is_empty() {
                format!("Codex requested approval to execute this command. Reason: {reason}")
            } else {
                format!(
                    "Codex requested approval to execute this command in {cwd}. Reason: {reason}"
                )
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                command,
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::CommandExecution,
                    request_id,
                },
            )?;
        }
        "item/fileChange/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let detail = if reason.is_empty() {
                "Codex requested approval to apply file changes.".to_owned()
            } else {
                format!("Codex requested approval to apply file changes. Reason: {reason}")
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Apply file changes",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::FileChange,
                    request_id,
                },
            )?;
        }
        "item/permissions/requestApproval" => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            let permissions_summary = describe_codex_permission_request(
                params.get("permissions").unwrap_or(&Value::Null),
            );
            let detail = match (
                reason.trim().is_empty(),
                permissions_summary
                    .as_deref()
                    .filter(|value| !value.is_empty()),
            ) {
                (true, Some(summary)) => {
                    format!("Codex requested approval to grant additional permissions: {summary}.")
                }
                (false, Some(summary)) => format!(
                    "Codex requested approval to grant additional permissions: {summary}. Reason: {reason}"
                ),
                (true, None) => {
                    "Codex requested approval to grant additional permissions.".to_owned()
                }
                (false, None) => format!(
                    "Codex requested approval to grant additional permissions. Reason: {reason}"
                ),
            };

            recorder.push_codex_approval(
                "Codex needs approval",
                "Grant additional permissions",
                &detail,
                CodexPendingApproval {
                    kind: CodexApprovalKind::Permissions {
                        requested_permissions: params
                            .get("permissions")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    },
                    request_id,
                },
            )?;
        }
        "item/tool/requestUserInput" => {
            let questions: Vec<UserInputQuestion> = serde_json::from_value(
                params
                    .get("questions")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .context("failed to parse Codex request_user_input questions")?;
            let detail = describe_codex_user_input_request(&questions);

            recorder.push_codex_user_input_request(
                "Codex needs input",
                &detail,
                questions.clone(),
                CodexPendingUserInput {
                    questions,
                    request_id,
                },
            )?;
        }
        "mcpServer/elicitation/request" => {
            let request: McpElicitationRequestPayload = serde_json::from_value(params.clone())
                .context("failed to parse Codex MCP elicitation request")?;
            let detail = describe_codex_mcp_elicitation_request(&request);

            recorder.push_codex_mcp_elicitation_request(
                "Codex needs MCP input",
                &detail,
                request.clone(),
                CodexPendingMcpElicitation {
                    request,
                    request_id,
                },
            )?;
        }
        _ => {
            let (title, detail) = describe_codex_app_server_request(method, params);
            recorder.push_codex_app_request(
                &title,
                &detail,
                method,
                params.clone(),
                CodexPendingAppRequest { request_id },
            )?;
        }
    }

    Ok(())
}

/// Formats the sidebar title + detail for a generic app-server request
/// (anything that is not a built-in approval/user-input/MCP flow).
/// `item/tool/call` gets a dedicated copy mentioning the tool and
/// server names; everything else gets a generic "needs a JSON result"
/// placeholder.
fn describe_codex_app_server_request(method: &str, params: &Value) -> (String, String) {
    if method == "item/tool/call" {
        let tool = params
            .get("tool")
            .or_else(|| params.get("toolName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("tool");
        let server = params
            .get("server")
            .or_else(|| params.get("serverName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let scope = server
            .map(|server_name| format!(" from `{server_name}`"))
            .unwrap_or_default();
        return (
            "Codex needs a tool result".to_owned(),
            format!(
                "Codex requested a result for `{tool}`{scope}. Review the request payload and submit the JSON result to continue."
            ),
        );
    }

    (
        "Codex needs a response".to_owned(),
        format!(
            "Codex sent an app-server request `{method}` that needs a JSON result before it can continue."
        ),
    )
}

/// Surfaces `item/started` events by type: agent messages finalize
/// any in-flight streaming text; command executions and web searches
/// register a running entry in the recorder so the UI can show a
/// spinner until `item/completed` arrives.
fn handle_codex_app_server_item_started(
    item: &Value,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            recorder.finish_streaming_text()?;
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                recorder.command_started(key, command)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            recorder.command_started(key, &command)?;
        }
        _ => {}
    }

    Ok(())
}

/// Formats a human-readable summary of the permission scopes Codex is
/// requesting (file-system read/write paths, network, macOS
/// accessibility/calendar/preferences/automations) for display in the
/// approval prompt sidebar.
fn describe_codex_permission_request(permissions: &Value) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(read_paths) = permissions
        .pointer("/fileSystem/read")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = read_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("read access to `{joined}`"));
        }
    }

    if let Some(write_paths) = permissions
        .pointer("/fileSystem/write")
        .and_then(Value::as_array)
        .filter(|paths| !paths.is_empty())
    {
        let joined = write_paths
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if !joined.is_empty() {
            parts.push(format!("write access to `{joined}`"));
        }
    }

    if permissions
        .pointer("/network/enabled")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("network access".to_owned());
    }

    if permissions
        .pointer("/macos/accessibility")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS accessibility access".to_owned());
    }

    if permissions
        .pointer("/macos/calendar")
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push("macOS calendar access".to_owned());
    }

    if let Some(preferences) = permissions
        .pointer("/macos/preferences")
        .and_then(Value::as_str)
        .filter(|value| *value != "none")
    {
        parts.push(format!("macOS preferences access ({preferences})"));
    }

    if let Some(automations) = permissions.pointer("/macos/automations") {
        if let Some(scope) = automations.as_str() {
            if scope == "all" {
                parts.push("macOS automation access".to_owned());
            }
        } else if let Some(bundle_ids) = automations.get("bundle_ids").and_then(Value::as_array) {
            let joined = bundle_ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if !joined.is_empty() {
                parts.push(format!("macOS automation access for `{joined}`"));
            }
        }
    }

    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Formats the sidebar detail for a user-input request: names the
/// single question's header when there is only one, otherwise reports
/// the count.
fn describe_codex_user_input_request(questions: &[UserInputQuestion]) -> String {
    match questions.len() {
        0 => "Codex requested additional input.".to_owned(),
        1 => {
            let question = &questions[0];
            format!(
                "Codex requested additional input for \"{}\".",
                question.header.trim()
            )
        }
        count => format!("Codex requested additional input for {count} questions."),
    }
}

/// Formats the sidebar detail for an MCP elicitation request — the
/// structured-form flow shows the server-provided message; the URL
/// flow instructs the user to continue in a browser and includes the
/// URL.
fn describe_codex_mcp_elicitation_request(request: &McpElicitationRequestPayload) -> String {
    match &request.mode {
        McpElicitationRequestMode::Form { message, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested additional structured input.",
                    request.server_name
                )
            } else {
                format!(
                    "MCP server {} requested additional structured input. {}",
                    request.server_name, trimmed
                )
            }
        }
        McpElicitationRequestMode::Url { message, url, .. } => {
            let trimmed = message.trim();
            if trimmed.is_empty() {
                format!(
                    "MCP server {} requested that you continue in a browser: {}",
                    request.server_name, url
                )
            } else {
                format!(
                    "MCP server {} requested that you continue in a browser. {} {}",
                    request.server_name, trimmed, url
                )
            }
        }
    }
}

/// Surfaces `item/completed` events by type: agent messages are
/// reconciled against any streamed text via dedup; command executions
/// and web searches finalize their recorder entries with the exit
/// status; file changes are pushed as diff messages annotated with
/// create/edit change type. Other item types are intentionally
/// ignored.
fn handle_codex_app_server_item_completed(
    item: &Value,
    state: &AppState,
    session_id: &str,
    turn_state: &mut CodexTurnState,
    recorder: &mut impl TurnRecorder,
) -> Result<()> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => {
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                record_completed_codex_agent_message(
                    turn_state, recorder, state, session_id, item_id, text,
                )?;
            }
        }
        Some("commandExecution") => {
            if let Some(command) = item.get("command").and_then(Value::as_str) {
                let key = item.get("id").and_then(Value::as_str).unwrap_or(command);
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let status = match item.get("status").and_then(Value::as_str) {
                    Some("completed")
                        if item.get("exitCode").and_then(Value::as_i64) == Some(0) =>
                    {
                        CommandStatus::Success
                    }
                    Some("completed") => CommandStatus::Error,
                    Some("failed") | Some("declined") => CommandStatus::Error,
                    _ => CommandStatus::Running,
                };
                recorder.command_completed(key, command, output, status)?;
            }
        }
        Some("fileChange") => {
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return Ok(());
            }
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return Ok(());
            };
            for change in changes {
                let Some(file_path) = change.get("path").and_then(Value::as_str) else {
                    continue;
                };
                let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
                if diff.trim().is_empty() {
                    continue;
                }
                let change_type = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => ChangeType::Create,
                    _ => ChangeType::Edit,
                };
                let summary = match change_type {
                    ChangeType::Create => format!("Created {}", short_file_name(file_path)),
                    ChangeType::Edit => format!("Updated {}", short_file_name(file_path)),
                };
                recorder.push_diff(file_path, &summary, diff, change_type)?;
            }
        }
        Some("webSearch") => {
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("webSearch");
            let command = describe_codex_app_server_web_search_command(item);
            let output = summarize_codex_app_server_web_search_output(item);
            recorder.command_completed(key, &command, &output, CommandStatus::Success)?;
        }
        _ => {}
    }

    Ok(())
}

