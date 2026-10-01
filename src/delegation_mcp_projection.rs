// Model-facing response projections introduced beside delegation_mcp.rs.
// Owns compact spawn metadata and own-message body omission. Does not change
// HTTP responses, durable session/mailbox records, or receipt issuance/ack.

fn mcp_selected_fields(value: &Value, fields: &[&str]) -> Value {
    Value::Object(
        fields
            .iter()
            .filter_map(|key| {
                value
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

fn compact_mcp_spawn_result(response: Value) -> Value {
    let mut result = mcp_selected_fields(&response, &["revision", "serverInstanceId"]);
    let delegation = &response["delegation"];
    result["delegation"] = mcp_selected_fields(
        delegation,
        &[
            "id",
            "parentSessionId",
            "childSessionId",
            "mode",
            "status",
            "title",
            "agent",
            "model",
            "writePolicy",
            "createdAt",
            "startedAt",
            "completedAt",
            "reviewResultRequired",
            "postSubmissionTransportError",
            "reviewResultRecoveryError",
            "hold",
        ],
    );
    if let Some(first_turn) = response.get("firstTurn") {
        result["firstTurn"] = first_turn.clone();
    }
    if delegation["status"]
        .as_str()
        .is_some_and(is_terminal_delegation_status)
    {
        if let Some(summary) = delegation["result"]["summary"].as_str() {
            let mut terminal_result = mcp_selected_fields(&delegation["result"], &["status"]);
            terminal_result["summary"] = Value::String(truncate_chars(summary, 500));
            result["delegation"]["result"] = terminal_result;
        }
    }
    result["childSession"] = mcp_selected_fields(
        &response["childSession"],
        &["id", "name", "agent", "model", "status", "workdir"],
    );
    result["delegationId"] = delegation["id"].clone();
    result["childSessionId"] = delegation
        .get("childSessionId")
        .or_else(|| response["childSession"].get("id"))
        .or_else(|| response.get("childSessionId"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(prompt) = delegation.get("prompt").and_then(Value::as_str) {
        result["preview"] = Value::String(prompt.chars().take(160).collect());
    }
    result
}

fn mcp_mailbox_messages(messages: &[MailboxMessage], reader_session_id: &str) -> Result<Value> {
    let mut projected =
        serde_json::to_value(messages).context("failed to encode mailbox messages")?;
    if let Some(messages) = projected.as_array_mut() {
        for message in messages {
            if message["senderSessionId"].as_str() == Some(reader_session_id) {
                if let Some(object) = message.as_object_mut() {
                    object.remove("body");
                    object.insert("bodyOmitted".to_owned(), Value::Bool(true));
                }
            }
        }
    }
    Ok(projected)
}
