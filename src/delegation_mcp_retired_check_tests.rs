use super::*;

#[test]
fn retired_check_disposition_crosses_the_real_evaluation_post_without_replay() {
    let expected = json!({"workRef":"work", "agent":"Codex",
        "retiredCheckDispositions":[{"runId":"test-old", "reason":"The old worktree was retired."}]});
    let wire = expected.clone();
    let (url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/acceptance-evaluations");
        assert_eq!(serde_json::from_str::<Value>(&request.body).unwrap(), wire);
        (409, json!({"error":"retired check disposition write outcome unknown; no evaluator started"}))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
    let result = bridge.tool_evaluate_acceptance(expected).unwrap_err();
    assert!(result.to_string().contains("retired check disposition"), "{result:#}");
    server.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1, "no duplicate audit write or spawn request");
}

#[test]
fn retired_check_request_is_closed_and_refuses_invalid_selections() {
    for entries in [Value::Null, json!([{"runId":"test-old","reason":"","author":"invented"}]),
        json!([{"runId":"test-old","reason":"ok","binding":{}}])] {
        assert!(serde_json::from_value::<RequestAcceptanceEvaluationRequest>(json!({
            "workRef":"work", "retiredCheckDispositions":entries})).is_err());
    }
    for entries in [
        json!([{"runId":"test-old","reason":" "}]),
        json!([{"runId":"test-old","reason":"ok"},{"runId":"test-old","reason":"again"}]),
        json!([{"runId":"test-old","reason":"x".repeat(1025)}]),
    ] {
        let request: RequestAcceptanceEvaluationRequest = serde_json::from_value(json!({
            "workRef":"work", "retiredCheckDispositions":entries})).unwrap();
        assert!(validate_acceptance_retired_check_requests(&request.retired_check_dispositions).is_err());
    }
}
