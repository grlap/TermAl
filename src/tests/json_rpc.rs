//! JSON-RPC 2.0 framing tests for the Codex protocol (request, notification,
//! response result, response error). Extracted from `tests.rs` so each domain
//! lives in its own sibling module under `tests/`.

use super::*;

#[test]
fn json_rpc_request_message_includes_jsonrpc() {
    assert_eq!(
        json_rpc_request_message(
            "request-1".to_owned(),
            "model/list",
            json!({
                "limit": 100,
            }),
        ),
        json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "method": "model/list",
            "params": {
                "limit": 100,
            }
        })
    );
}

#[test]
fn json_rpc_notification_message_includes_jsonrpc() {
    assert_eq!(
        json_rpc_notification_message("initialized"),
        json!({
            "jsonrpc": "2.0",
            "method": "initialized",
        })
    );
}

#[test]
fn codex_json_rpc_response_message_includes_jsonrpc_for_result_payload() {
    let response = CodexJsonRpcResponseCommand {
        request_id: json!("request-ok"),
        payload: CodexJsonRpcResponsePayload::Result(json!({
            "decision": "accept",
        })),
    };

    assert_eq!(
        codex_json_rpc_response_message(&response),
        json!({
            "jsonrpc": "2.0",
            "id": "request-ok",
            "result": {
                "decision": "accept",
            }
        })
    );
}

#[test]
fn codex_json_rpc_response_message_includes_jsonrpc_for_error_payload() {
    let response = CodexJsonRpcResponseCommand {
        request_id: json!("request-error"),
        payload: CodexJsonRpcResponsePayload::Error {
            code: -32001,
            message: "Session unavailable; request could not be delivered.".to_owned(),
        },
    };

    assert_eq!(
        codex_json_rpc_response_message(&response),
        json!({
            "jsonrpc": "2.0",
            "id": "request-error",
            "error": {
                "code": -32001,
                "message": "Session unavailable; request could not be delivered.",
            }
        })
    );
}
