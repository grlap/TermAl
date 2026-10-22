use super::delegation_mcp_tests::spawn_test_mcp_http_server;
use super::*;

#[test]
fn durability_pending_documentation_links_the_contract_and_its_consumers() {
    let contract = include_str!("../docs/features/durability-pending.md");
    assert!(contract.contains("[Engram host adapter](engram-host-adapter.md)"));
    assert!(contract.contains("[test plan and launcher](../test.md)"));
    assert!(
        include_str!("../docs/features/engram-host-adapter.md")
            .contains("[Durability pending](durability-pending.md)")
    );
    assert!(
        include_str!("../docs/test.md")
            .contains("[durability-pending contract](features/durability-pending.md)")
    );
    for operation in [
        "test_run_registration",
        "acceptance_evaluation_submission",
        "named_root_naming",
        "named_root_release",
    ] {
        assert!(contract.contains(operation), "{operation}");
    }
}

fn pending_bridge_envelope(operation: &str, identity: Value) -> Value {
    json!({
        "error": "the enqueued operation may still commit; retry the unchanged original request",
        "contract": "termal-durability-pending", "version": 1,
        "code": "durability_pending", "outcome": "pending", "operation": operation,
        "identity": identity, "retry": {"mode": "same_request"},
    })
}

fn pending_bridge_naming(path: Option<&str>) -> Value {
    pending_bridge_envelope(
        "named_root_naming",
        json!({
            "workId": "work-fixture", "claimId": "claim-fixture", "generation": 7,
            "ownerId": "original-owner", "ownerVersion": 3,
            "action": if path.is_some() {"name"} else {"clear"},
            "requestedWork": "work-fixture", "requestedPath": path,
            "sessionId": "session-parent",
        }),
    )
}

fn pending_bridge_release(prior: bool) -> Value {
    let mut identity = json!({
        "workId": "work-fixture", "workRef": "fixture-ref", "claimId": "claim-fixture",
        "generation": 7, "auditId": "original-audit", "phase": "enqueued",
        "reason": "finished lane", "requesterSessionId": "session-parent",
    });
    if prior {
        identity["auditId"] = Value::Null;
        identity["phase"] = json!("candidate");
        identity["releaseStage"] = json!("prior_authority");
        identity["ownerId"] = json!("original-owner");
        identity["ownerVersion"] = json!(3);
        identity["generation"] = json!(6);
        identity["requestedSelection"] = json!({
            "workId": "work-fixture", "claimId": "claim-fixture",
            "generation": 7, "root": "C:/repo/retained",
        });
    }
    pending_bridge_envelope("named_root_release", identity)
}

fn pending_bridge_evaluation() -> Value {
    pending_bridge_envelope(
        "acceptance_evaluation_submission",
        json!({
            "delegationId": "original-delegation", "attemptKey": "original-attempt",
            "evaluatorSessionId": "session-parent",
        }),
    )
}

#[test]
fn durability_pending_bridge_all_boundaries_are_structured_mcp_errors_without_replay() {
    for case in 0..5 {
        let (tool, endpoint, request, envelope) = match case {
            0 | 1 => {
                let path = (case == 0).then_some(".worktrees/fixture");
                let mut request = json!({"work": "work-fixture"});
                if let Some(path) = path {
                    request["path"] = json!(path);
                }
                (
                    "termal_name_source_root",
                    "engram-source-root",
                    request,
                    pending_bridge_naming(path),
                )
            }
            2 => (
                "termal_submit_acceptance_evaluation",
                "acceptance-evaluation",
                json!({"schemaVersion": 1, "attemptKey": "original-attempt",
                    "verdicts": [{"criterion": 1, "verdict": "pass", "basis": "judgment",
                        "rationale": "unchanged judgment", "evidence": []}]}),
                pending_bridge_evaluation(),
            ),
            _ => (
                "termal_release_named_root",
                "engram-named-root-release",
                json!({"work": "work-fixture", "generation": 7, "reason": "finished lane"}),
                pending_bridge_release(case == 4),
            ),
        };
        let reply = envelope.clone();
        let (base_url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            if request.method == "GET" {
                assert_eq!(request.path, "/api/state");
                (
                    200,
                    if case == 2 {
                        json!({"sessions": [{"id": "session-parent", "parentDelegationId": "original-delegation"}],
                        "delegations": [{"id": "original-delegation", "childSessionId": "session-parent",
                            "mode": "evaluator", "acceptanceEvaluationAllowed": true}]})
                    } else {
                        json!({"sessions": [{"id": "session-parent"}], "delegations": []})
                    },
                )
            } else {
                assert_eq!(
                    request.path,
                    format!("/api/sessions/session-parent/{endpoint}")
                );
                (503, reply.clone())
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
        let response = bridge
            .handle_message(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": request}}))
            .unwrap()
            .unwrap();
        server.join().unwrap();
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert_eq!(response["result"]["structuredContent"], envelope);
        assert_eq!(
            serde_json::from_str::<Value>(
                response["result"]["content"][0]["text"].as_str().unwrap()
            )
            .unwrap(),
            envelope
        );
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "one classification read and exactly one POST"
        );
        assert_eq!(requests[1].method, "POST");
        assert_eq!(
            serde_json::from_str::<Value>(&requests[1].body).unwrap(),
            request
        );
    }
}

#[test]
fn durability_pending_bridge_submission_preserves_distinct_error_and_original_body() {
    let envelope = pending_bridge_evaluation();
    let reply = envelope.clone();
    let (base_url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(
            request.path,
            "/api/sessions/session-parent/acceptance-evaluation"
        );
        (503, reply.clone())
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
    let request = json!({"schemaVersion": 1, "attemptKey": "original-attempt", "verdicts": []});
    let error = bridge
        .tool_submit_acceptance_evaluation(request.clone())
        .unwrap_err();
    server.join().unwrap();
    assert_eq!(
        error
            .downcast_ref::<TermalDurabilityPendingError>()
            .unwrap()
            .envelope,
        envelope
    );
    assert_eq!(
        serde_json::from_str::<Value>(&error.to_string()).unwrap(),
        envelope
    );
    assert!(!error_is_safe_replay_service_unavailable(&error));
    assert!(!termal_delegation_outcome_is_unknown(&error));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&requests[0].body).unwrap(),
        request
    );
}

fn parse_pending_bridge(
    request: &Value,
    response: &Value,
    endpoint: &str,
) -> Option<TermalDurabilityPendingError> {
    durability_pending_for_request(
        StatusCode::SERVICE_UNAVAILABLE,
        "POST",
        &format!("/api/sessions/session-parent/{endpoint}"),
        "session-parent",
        request,
        response,
    )
}

#[test]
fn durability_pending_bridge_rejects_wrong_contract_endpoint_or_naming_owner() {
    let request = json!({"work": "work-fixture", "path": ".worktrees/fixture"});
    let original = pending_bridge_naming(Some(".worktrees/fixture"));
    assert!(parse_pending_bridge(&request, &original, "engram-source-root").is_some());
    for (pointer, replacement) in [
        ("/contract", json!("other")),
        ("/version", json!(2)),
        ("/code", json!("unavailable")),
        ("/outcome", json!("not_applied")),
        ("/operation", json!("named_root_release")),
        ("/retry/mode", json!("automatic")),
        ("/identity/sessionId", json!("another-session")),
        ("/identity/requestedWork", json!("another-work")),
        ("/identity/requestedPath", Value::Null),
        ("/identity/action", json!("clear")),
        ("/identity/ownerId", json!("")),
        ("/identity/ownerVersion", json!("3")),
        ("/identity/generation", json!(-1)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            parse_pending_bridge(&request, &changed, "engram-source-root").is_none(),
            "{pointer}"
        );
    }
    for (status, method, endpoint) in [
        (StatusCode::OK, "POST", "engram-source-root"),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "POST",
            "engram-source-root",
        ),
        (StatusCode::SERVICE_UNAVAILABLE, "GET", "engram-source-root"),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "POST",
            "acceptance-evaluations",
        ),
        (StatusCode::SERVICE_UNAVAILABLE, "POST", "other"),
    ] {
        assert!(
            durability_pending_for_request(
                status,
                method,
                &format!("/api/sessions/session-parent/{endpoint}"),
                "session-parent",
                &request,
                &original
            )
            .is_none()
        );
    }
}

#[test]
fn durability_pending_bridge_rejects_foreign_attempt_and_release_identity() {
    let evaluation = json!({"attemptKey": "original-attempt"});
    for (pointer, value) in [
        ("/identity/attemptKey", json!("other-attempt")),
        ("/identity/evaluatorSessionId", json!("other-evaluator")),
        ("/identity/delegationId", Value::Null),
    ] {
        let mut changed = pending_bridge_evaluation();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            parse_pending_bridge(&evaluation, &changed, "acceptance-evaluation").is_none(),
            "{pointer}"
        );
    }
    let release = json!({"work": "fixture-ref", "generation": 7, "reason": "finished lane"});
    for prior in [false, true] {
        let original = pending_bridge_release(prior);
        assert!(parse_pending_bridge(&release, &original, "engram-named-root-release").is_some());
        for (pointer, value) in [
            ("/identity/workRef", json!("other-work")),
            ("/identity/requesterSessionId", json!("other-requester")),
            ("/identity/reason", json!("other reason")),
            (
                if prior {
                    "/identity/requestedSelection/generation"
                } else {
                    "/identity/generation"
                },
                json!(8),
            ),
            (
                if prior {
                    "/identity/ownerId"
                } else {
                    "/identity/auditId"
                },
                json!(""),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                parse_pending_bridge(&release, &changed, "engram-named-root-release").is_none(),
                "{pointer}"
            );
        }
        if prior {
            let mut invented = original;
            invented["identity"]["auditId"] = json!("invented-audit");
            assert!(
                parse_pending_bridge(&release, &invented, "engram-named-root-release").is_none()
            );
        }
    }
}

#[test]
fn durability_pending_bridge_strips_unversioned_extensions_and_keeps_nullable_identity() {
    for (request, original, endpoint) in [
        (
            json!({"work": "work-fixture"}),
            pending_bridge_naming(None),
            "engram-source-root",
        ),
        (
            json!({"work": "work-fixture", "generation": 7, "reason": "finished lane"}),
            pending_bridge_release(true),
            "engram-named-root-release",
        ),
    ] {
        let mut extended = original.clone();
        extended["extra"] = json!("ignored extension");
        extended["identity"]["extra"] = json!("ignored identity extension");
        if endpoint == "engram-named-root-release" {
            extended["identity"]["requestedSelection"]["extra"] = json!("ignored nested extension");
        }
        assert_eq!(
            parse_pending_bridge(&request, &extended, endpoint)
                .unwrap()
                .envelope,
            original
        );
    }
    let mut original = pending_bridge_naming(Some(".worktrees/fixture"));
    original["identity"]["generation"] = Value::Null;
    assert!(
        parse_pending_bridge(
            &json!({"work": "work-fixture", "path": ".worktrees/fixture"}),
            &original,
            "engram-source-root"
        )
        .is_some(),
        "a pre-generation guard must not invent a generation"
    );
}
