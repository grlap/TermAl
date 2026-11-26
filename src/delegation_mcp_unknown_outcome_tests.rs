use super::delegation_mcp_tests::spawn_test_mcp_http_server;
use super::*;

const MAILBOX_ID: &str = "mailbox-00000000-0000-4000-8000-000000000001";

/// Calls `tool` through the bridge against a host that answers its one
/// request with `status` and `body`, and returns the MCP result.
fn tool_result(tool: &str, endpoint: String, arguments: Value, status: u16, body: Value) -> Value {
    let (base_url, _requests, server) = spawn_test_mcp_http_server(2, move |request| {
        if request.method == "GET" {
            assert_eq!(request.path, "/api/state");
            (
                200,
                json!({"sessions": [{"id": "session-parent"}], "delegations": []}),
            )
        } else {
            assert_eq!(
                request.path,
                format!("/api/sessions/session-parent/{endpoint}")
            );
            (status, body.clone())
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
    let response = bridge
        .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}}))
        .unwrap()
        .unwrap();
    server.join().unwrap();
    response["result"].clone()
}

/// A mailbox send or read whose deadline left the outcome unknown reaches the
/// MCP client as a typed result it can parse, not only as tool text.
#[test]
fn mailbox_send_and_read_deadline_errors_carry_a_typed_code() {
    let unknown = json!({"error": format!(
        "native mailbox relay handler deadline; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME}; \
         retry the original request with the same idempotencyKey"
    )});
    for (tool, endpoint, arguments) in [
        (
            "termal_send_to_session",
            "mailboxes/send".to_owned(),
            json!({"sessionId": "session-target", "message": "hello",
                "idempotencyKey": "unknown-outcome-key"}),
        ),
        (
            "termal_read_mailbox",
            format!("mailboxes/{MAILBOX_ID}/read"),
            json!({"mailboxId": MAILBOX_ID}),
        ),
    ] {
        let result = tool_result(tool, endpoint, arguments, 503, unknown.clone());
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let typed = &result["structuredContent"];
        assert_eq!(typed["code"], "outcome_unknown", "{tool}: {result}");
        assert_eq!(typed["outcome"], "unknown", "{tool}: {result}");
        assert_eq!(typed["status"], 503, "{tool}: {result}");
        assert!(
            typed["error"]
                .as_str()
                .unwrap()
                .contains("same idempotencyKey"),
            "{tool}: {result}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            *typed,
            "{tool}: the text carries the same envelope"
        );
    }
}

/// An acknowledgement whose outcome the host reported unknown reaches the MCP
/// client as the same typed result, still telling it to retry the receipt.
#[test]
fn an_acknowledgement_deadline_error_carries_a_typed_code() {
    let result = tool_result(
        "termal_acknowledge_mailbox",
        format!("mailboxes/{MAILBOX_ID}/acknowledge"),
        json!({"mailboxId": MAILBOX_ID, "receipt": "receipt-unknown-outcome"}),
        503,
        json!({"error": format!(
            "native mailbox relay handler deadline; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME}"
        )}),
    );
    assert_eq!(result["isError"], true, "{result}");
    let typed = &result["structuredContent"];
    assert_eq!(typed["code"], "outcome_unknown", "{result}");
    assert_eq!(typed["outcome"], "unknown", "{result}");
    assert_eq!(typed["status"], 503, "{result}");
    assert!(
        typed["error"]
            .as_str()
            .unwrap()
            .contains("retry the same receipt unchanged"),
        "{result}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        *typed,
        "the text carries the same envelope"
    );
}

/// A refusal whose outcome is known keeps its plain tool error, including a
/// non-server-error answer that still carries the clause.
#[test]
fn a_known_mailbox_refusal_keeps_its_plain_tool_error() {
    for (status, error) in [
        (404, "mailbox not found".to_owned()),
        (
            409,
            format!(
                "central mailbox authority_changed; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME};                  reconcile the original request"
            ),
        ),
    ] {
        let result = tool_result(
            "termal_read_mailbox",
            format!("mailboxes/{MAILBOX_ID}/read"),
            json!({"mailboxId": MAILBOX_ID}),
            status,
            json!({"error": error}),
        );
        assert_eq!(result["isError"], true, "{status}: {result}");
        assert!(
            result.get("structuredContent").is_none(),
            "{status}: {result}"
        );
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(error.as_str()),
            "{status}: {result}"
        );
    }
}

/// Text the bridge or a caller worded is never read as the host's report,
/// even when it echoes the clause.
#[test]
fn an_echoed_clause_in_a_local_error_is_not_typed() {
    let echoed = anyhow::anyhow!("unknown mode `all; {FEDERATION_MAILBOX_UNKNOWN_OUTCOME}; any`");
    assert!(mailbox_unknown_outcome_envelope(&echoed).is_none());
    let wrapped = termal_delegation_unknown_outcome(
        &echoed,
        format!("mailbox send receipt was not received: {echoed}"),
    );
    assert!(mailbox_unknown_outcome_envelope(&wrapped).is_none());
}
