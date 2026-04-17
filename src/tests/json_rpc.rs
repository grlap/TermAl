// JSON-RPC 2.0 is the wire protocol for every TermAl ↔ Codex shared-app-server
// exchange over stdio framing. Each outbound frame must carry `jsonrpc: "2.0"`
// exactly; Codex silently drops frames with a missing or wrong version, so a
// regression here would break every turn without a visible error.
// `json_rpc_request_message` builds id + method + params frames,
// `json_rpc_notification_message` builds method-only frames with no id, and
// `codex_json_rpc_response_message` dispatches a `CodexJsonRpcResponseCommand`
// to either a result body or an error body. The helpers live in the Codex
// runtime alongside production code; see `src/runtime.rs` for definitions.

use super::*;

// Pins the exact shape of a request frame: `jsonrpc`, `id`, `method`, and
// `params` all present with the string `"2.0"` version marker. Guards against
// a regression that drops the `jsonrpc` field or mangles method/params,
// either of which would make Codex silently ignore every client-initiated call.
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

// Pins notifications as id-less frames: `jsonrpc` + `method` only, no `id`
// and no `params` when none are supplied. Guards against a regression that
// adds a null id (which Codex treats as a request expecting a response) or
// omits the version marker, either of which desynchronizes the session.
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

// Pins the success branch of `codex_json_rpc_response_message`: a
// `Result` payload emits `jsonrpc` + `id` + `result` with no `error` key.
// Guards against a regression that wraps successful replies in an error
// envelope or drops the version, which would make Codex reject the reply
// and leave the originating request hanging forever.
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

// Pins the failure branch: an `Error` payload emits `jsonrpc` + `id` +
// `error { code, message }` with no `result` key, preserving the caller's
// numeric code and message verbatim. Guards against a regression that
// swallows error codes, mixes `result` and `error` in one frame, or drops
// the version marker — any of which would break client-side error handling.
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
