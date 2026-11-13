use super::*;

fn request() -> Value {
    json!({"mode":"focused", "targets":[{"workRef":"work", "use":{
        "kind":"committed_input", "expected_commit":"a".repeat(40), "purpose":"verification"
    }}]})
}

#[test]
fn typed_send_bridge_preserves_the_validated_request_without_claim_injection() {
    let expected = request();
    let wire = expected.clone();
    let (url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/send");
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["testRequest"], wire);
        assert!(body.get("binding").is_none());
        (
            202,
            json!({"mailboxId":"mailbox-1", "messageId":"mailbox-message-1",
            "sequence":1, "unreadDepth":1, "notificationDisposition":"queuedBehindActiveTurn",
            "duplicate":false, "senderProcessedThrough":1, "senderCursorAdvanced":true}),
        )
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
    bridge
        .tool_send_to_session(
            json!({"sessionId":"session-peer", "message":"run the check",
        "idempotencyKey":"typed-key", "testRequest":expected}),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn malformed_typed_send_is_rejected_before_discovery_or_network() {
    let bridge =
        TermalDelegationMcpBridge::new("session-parent".into(), "http://127.0.0.1:0".into())
            .unwrap();
    let mut injected = request();
    injected["targets"][0]["binding"] = json!({"claim_fence":999});
    for value in [
        Value::Null,
        json!({}),
        injected,
        json!({"mode":"live", "targets":[]}),
    ] {
        let error = bridge
            .tool_send_to_session(json!({"sessionId":"peer requiring discovery",
            "message":"do not append", "idempotencyKey":"typed-invalid", "testRequest":value}))
            .unwrap_err();
        assert!(error.to_string().contains("testRequest"), "{error:#}");
    }
}
