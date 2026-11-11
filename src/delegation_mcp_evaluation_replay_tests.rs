//! The parent bridge transports a retained reference, never a caller verdict.
use super::*;

#[test]
fn replay_bridge_preserves_pending_at_both_persistence_boundaries() {
    for boundary in [
        "open write acknowledgment",
        "recorded receipt acknowledgment",
    ] {
        let envelope = json!({
            "error": format!("{boundary} exceeded its persistence deadline"),
            "contract":"termal-durability-pending", "version":1,
            "code":"durability_pending", "outcome":"pending",
            "operation":"acceptance_evaluation_submission",
            "identity":{"delegationId":"retained", "attemptKey":"original-attempt",
                "evaluatorSessionId":"original-evaluator"},
            "retry":{"mode":"same_request"}
        });
        let reply = envelope.clone();
        let (url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(
                request.path,
                "/api/sessions/parent/acceptance-evaluation-replay"
            );
            (503, reply.clone())
        });
        let bridge = TermalDelegationMcpBridge::new("parent".into(), url).unwrap();
        bridge.caller_is_delegation_child.set(false).unwrap();
        let response = bridge
            .handle_message(json!({"jsonrpc":"2.0", "id":1,
            "method":"tools/call", "params":{"name":TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME,
                "arguments":{"delegationId":"retained"}}}))
            .unwrap()
            .unwrap();
        server.join().unwrap();
        assert_eq!(
            response["result"]["isError"], true,
            "{boundary}: {response}"
        );
        assert_eq!(
            response["result"]["structuredContent"], envelope,
            "{boundary}"
        );
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 1, "no bridge retry at {boundary}");
        assert_eq!(
            serde_json::from_str::<Value>(&sent[0].body).unwrap(),
            json!({"delegationId":"retained"})
        );
    }
}

#[test]
fn replay_bridge_rejects_foreign_or_missing_pending_identity() {
    for identity in [
        json!({"delegationId":"foreign", "attemptKey":"original", "evaluatorSessionId":"child"}),
        json!({"delegationId":"retained", "attemptKey":"", "evaluatorSessionId":"child"}),
        json!({"delegationId":"retained", "attemptKey":"original", "evaluatorSessionId":"parent"}),
    ] {
        let envelope = json!({"error":"pending", "contract":"termal-durability-pending", "version":1,
            "code":"durability_pending", "outcome":"pending", "operation":"acceptance_evaluation_submission",
            "identity":identity, "retry":{"mode":"same_request"}});
        let (url, requests, server) =
            spawn_test_mcp_http_server(1, move |_| (503, envelope.clone()));
        let bridge = TermalDelegationMcpBridge::new("parent".into(), url).unwrap();
        let error = bridge
            .tool_replay_acceptance_evaluation(json!({"delegationId":"retained"}))
            .unwrap_err();
        server.join().unwrap();
        assert!(
            error
                .downcast_ref::<TermalDurabilityPendingError>()
                .is_none(),
            "{identity}: {error}"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn parent_bridge_posts_only_the_retained_delegation_reference() {
    let (url, requests, server) = spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            "/api/sessions/parent/acceptance-evaluation-replay"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&request.body).unwrap(),
            json!({"delegationId":"retained"})
        );
        (200, json!({"state":"recorded", "attemptKey":"original"}))
    });
    let bridge = TermalDelegationMcpBridge::new("parent".into(), url).unwrap();
    bridge.caller_is_delegation_child.set(false).unwrap();
    bridge
        .handle_tool_call(
            json!({"name": TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME,
        "arguments":{"delegationId":"retained"}}),
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn child_bridge_cannot_request_a_retained_replay() {
    let bridge =
        TermalDelegationMcpBridge::new("child".into(), "http://127.0.0.1:1".into()).unwrap();
    bridge.caller_is_delegation_child.set(true).unwrap();
    let error = bridge
        .handle_tool_call(
            json!({"name": TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME,
        "arguments":{"delegationId":"retained"}}),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not available to delegation-child"),
        "{error}"
    );
}

#[test]
fn bridge_rejects_injected_verdicts_before_any_http_request() {
    let bridge =
        TermalDelegationMcpBridge::new("parent".into(), "http://127.0.0.1:1".into()).unwrap();
    bridge.caller_is_delegation_child.set(false).unwrap();
    let error = bridge
        .handle_tool_call(
            json!({"name": TERMAL_REPLAY_ACCEPTANCE_EVALUATION_TOOL_NAME,
        "arguments":{"delegationId":"retained", "verdicts":[]}}),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid acceptance evaluation replay request"),
        "{error}"
    );
}
