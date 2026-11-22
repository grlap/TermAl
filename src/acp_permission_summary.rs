// Bounded, readable previews for the shared ACP approval card. Extracted
// from acp.rs; supports flat requests and nested ACP tool-call content.
// This projection only formats transcript text. The pending-request owner
// keeps provider payloads and opaque decision IDs unchanged.

use super::*;

const MAX_ACP_APPROVAL_TITLE_CHARS: usize = 1_000;
const MAX_ACP_APPROVAL_NAME_CHARS: usize = 200;
const MAX_ACP_APPROVAL_DETAIL_CHARS: usize = 8_000;
const MAX_ACP_APPROVAL_DIFF_SIDE_CHARS: usize = 3_000;
const MAX_ACP_APPROVAL_ENTRIES: usize = 32;
pub(crate) const MAX_ACP_APPROVAL_REFUSAL_CHARS: usize = 1_600;

pub(crate) fn summarize_acp_permission_request(
    params: &Value,
    agent: AcpAgent,
) -> (String, String) {
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let raw_tool_name = params
        .get("toolName")
        .and_then(Value::as_str)
        .or_else(|| tool_call.get("title").and_then(Value::as_str))
        .or_else(|| tool_call.get("kind").and_then(Value::as_str))
        .unwrap_or("Tool");
    let tool_name = bounded_acp_approval_text(raw_tool_name, MAX_ACP_APPROVAL_NAME_CHARS, "Tool");
    let raw_description = params
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| tool_call.get("title").and_then(Value::as_str))
        .unwrap_or(&tool_name);
    let description =
        bounded_acp_approval_text(raw_description, MAX_ACP_APPROVAL_TITLE_CHARS, &tool_name);
    let mut details = vec![format!(
        "{} requested approval for `{tool_name}`.",
        agent.label()
    )];
    if let Some(kind) = tool_call.get("kind").and_then(Value::as_str) {
        let kind = bounded_acp_approval_text(kind, MAX_ACP_APPROVAL_NAME_CHARS, "tool");
        if !kind.eq_ignore_ascii_case(&tool_name) {
            details.push(format!("Kind: `{kind}`"));
        }
    }
    if let Some(content) = tool_call.get("content").filter(|value| !value.is_null()) {
        let rendered = acp_permission_content_preview(content);
        if !rendered.is_empty() {
            details.push(rendered);
        }
    }
    if let Some(locations) = tool_call.get("locations").filter(|value| !value.is_null()) {
        let rendered = acp_permission_locations_preview(locations);
        if !rendered.is_empty() {
            details.push(rendered);
        }
    }
    (
        description,
        truncate_acp_approval_detail(&details.join("\n\n")),
    )
}

fn acp_permission_content_preview(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return truncate_acp_approval_detail(text);
    }
    let Some(entries) = value.as_array() else {
        if value.is_object() {
            return truncate_acp_approval_detail(&acp_permission_entry_preview(value));
        }
        return "Tool content could not be displayed.".to_owned();
    };
    let mut parts = entries
        .iter()
        .take(MAX_ACP_APPROVAL_ENTRIES)
        .map(acp_permission_entry_preview)
        .collect::<Vec<_>>();
    if entries.len() > MAX_ACP_APPROVAL_ENTRIES {
        parts.push("Additional tool content omitted.".to_owned());
    }
    truncate_acp_approval_detail(&parts.join("\n\n"))
}

fn acp_permission_entry_preview(entry: &Value) -> String {
    match entry.get("type").and_then(Value::as_str) {
        Some("diff") => {
            let path = bounded_acp_approval_text(
                entry.get("path").and_then(Value::as_str).unwrap_or(""),
                MAX_ACP_APPROVAL_TITLE_CHARS,
                "file",
            );
            let mut lines = vec![format!("File: {path}")];
            // Reserve space for each side and put the proposed write first.
            // A large old file must not hide the action being approved.
            if let Some(new) = entry.get("newText").and_then(Value::as_str) {
                lines.push(format!(
                    "After:\n{}",
                    truncate_acp_approval_text(new, MAX_ACP_APPROVAL_DIFF_SIDE_CHARS)
                ));
            }
            if let Some(old) = entry.get("oldText").and_then(Value::as_str) {
                lines.push(format!(
                    "Before:\n{}",
                    truncate_acp_approval_text(old, MAX_ACP_APPROVAL_DIFF_SIDE_CHARS)
                ));
            }
            lines.join("\n")
        }
        Some("terminal") => format!(
            "Terminal: {}",
            bounded_acp_approval_text(
                entry
                    .get("terminalId")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                MAX_ACP_APPROVAL_NAME_CHARS,
                "unavailable",
            )
        ),
        Some("content") => {
            acp_permission_embedded_preview(entry.get("content").unwrap_or(&Value::Null))
        }
        _ => acp_permission_embedded_preview(entry),
    }
}

fn acp_permission_embedded_preview(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return truncate_acp_approval_detail(text);
    }
    match value.get("type").and_then(Value::as_str) {
        Some("text") => value
            .get("text")
            .and_then(Value::as_str)
            .map(truncate_acp_approval_detail)
            .unwrap_or_else(|| "Text content is unavailable.".to_owned()),
        Some("image" | "audio") => {
            let label = if value["type"] == "image" {
                "Image"
            } else {
                "Audio"
            };
            match value.get("mimeType").and_then(Value::as_str) {
                Some(mime) => format!(
                    "{label} ({})",
                    bounded_acp_approval_text(mime, MAX_ACP_APPROVAL_NAME_CHARS, "media")
                ),
                None => label.to_owned(),
            }
        }
        Some("resource") => {
            let resource = value.get("resource").unwrap_or(&Value::Null);
            let uri = bounded_acp_approval_text(
                resource.get("uri").and_then(Value::as_str).unwrap_or(""),
                MAX_ACP_APPROVAL_TITLE_CHARS,
                "unavailable",
            );
            let mut text = format!("Resource: {uri}");
            if let Some(body) = resource.get("text").and_then(Value::as_str) {
                text.push('\n');
                text.push_str(&truncate_acp_approval_detail(body));
            }
            text
        }
        Some("resource_link") => {
            let name = bounded_acp_approval_text(
                value.get("name").and_then(Value::as_str).unwrap_or(""),
                MAX_ACP_APPROVAL_NAME_CHARS,
                "resource",
            );
            let uri = bounded_acp_approval_text(
                value.get("uri").and_then(Value::as_str).unwrap_or(""),
                MAX_ACP_APPROVAL_TITLE_CHARS,
                "unavailable",
            );
            let mut text = format!("Resource: {name}\nURI: {uri}");
            if let Some(description) = value.get("description").and_then(Value::as_str) {
                text.push('\n');
                text.push_str(&truncate_acp_approval_detail(description));
            }
            text
        }
        kind => format!(
            "Preview unavailable for tool content ({}).",
            bounded_acp_approval_text(
                kind.unwrap_or("unknown"),
                MAX_ACP_APPROVAL_NAME_CHARS,
                "unknown"
            )
        ),
    }
}

fn acp_permission_locations_preview(value: &Value) -> String {
    let Some(entries) = value.as_array() else {
        return "Location details could not be displayed.".to_owned();
    };
    let mut parts = entries
        .iter()
        .take(MAX_ACP_APPROVAL_ENTRIES)
        .map(|entry| {
            let path = bounded_acp_approval_text(
                entry.get("path").and_then(Value::as_str).unwrap_or(""),
                MAX_ACP_APPROVAL_TITLE_CHARS,
                "unavailable",
            );
            match entry.get("line").and_then(Value::as_u64) {
                Some(line) => format!("Location: {path}:{line}"),
                None => format!("Location: {path}"),
            }
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_ACP_APPROVAL_ENTRIES {
        parts.push("Additional locations omitted.".to_owned());
    }
    truncate_acp_approval_detail(&parts.join("\n"))
}

/// Bounds single-line approval metadata before transcript persistence.
pub(crate) fn bounded_acp_approval_text(value: &str, max_chars: usize, fallback: &str) -> String {
    let bounded = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect::<String>();
    let bounded = bounded.trim();
    if bounded.is_empty() {
        fallback.to_owned()
    } else {
        bounded.to_owned()
    }
}

/// Bounds agent-provided approval detail before storing it in the transcript.
pub(crate) fn truncate_acp_approval_detail(value: &str) -> String {
    truncate_acp_approval_text(value, MAX_ACP_APPROVAL_DETAIL_CHARS)
}

fn truncate_acp_approval_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}\n…")
    } else {
        truncated
    }
}
