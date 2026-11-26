// Owns the typed MCP rendering of a mailbox call whose outcome the host
// reported as unknown (a relay or central mailbox deadline, or a lost reply),
// so a client parses a code instead of matching tool text.
// Does not decide what is unknown (`delegation_mcp_timeouts.rs` and the host's
// unknown-outcome clause do), retry anything, or change other tool errors.
// New module beside `delegation_mcp_durability_pending.rs`, whose structured
// error shape it follows; `delegation_mcp.rs` is past its size limit.

use super::*;

/// The structured envelope for an error in which the host itself reported a
/// mailbox outcome unknown, or `None` for any other error. Only the host's
/// typed answer counts, never the error's text: a refusal the bridge or a
/// caller worded keeps its plain tool error even if it echoes the clause.
pub(crate) fn mailbox_unknown_outcome_envelope(err: &anyhow::Error) -> Option<Value> {
    let status = match err.downcast_ref::<TermalDelegationApiError>() {
        Some(error) => host_reported_mailbox_unknown_outcome(error).then_some(error.status)?,
        // A send's bridge wraps that host answer as an unknown outcome, which
        // keeps its status only when the host reported it.
        None => {
            err.downcast_ref::<TermalDelegationUnknownOutcome>()?
                .status?
        }
    };
    Some(json!({
        "contract": "termal-mailbox-outcome",
        "version": 1,
        "code": "outcome_unknown",
        "outcome": "unknown",
        "status": status.as_u16(),
        "error": err.to_string(),
    }))
}

pub(crate) fn mcp_json_rpc_unknown_outcome_error(id: Value, envelope: Value) -> Value {
    let mut result = mcp_tool_text_result(&envelope, true);
    result["structuredContent"] = envelope;
    mcp_json_rpc_result(id, result)
}

#[cfg(test)]
#[path = "delegation_mcp_unknown_outcome_tests.rs"]
mod delegation_mcp_unknown_outcome_tests;
