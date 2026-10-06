// Owns bounded status transport controls and reusable local reviewer witnesses.
use super::*;

pub(super) fn ready_page() -> Value {
    json!({"data": [{"name": TERMAL_DELEGATION_MCP_SERVER_NAME,
        "runtimeStatus": "connected", "tools": {"namespaced-key": {
            "name": TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME}}, "toolsError": null}],
        "nextCursor": null})
}

pub(super) fn respond_start(fixture: &ReviewerFixture, written: &[u8], response: Value) {
    let request: Value = serde_json::from_slice(written).unwrap();
    assert_eq!(request["method"], "mcpServerStatus/list");
    assert_eq!(request["params"]["threadId"], REVIEW_THREAD);
    handle_shared_codex_app_server_message(
        &json!({"id": request["id"], "result": response}),
        &fixture.state,
        &fixture.runtime.runtime_id,
        &fixture.pending,
        &fixture.runtime.sessions,
        &fixture.runtime.thread_sessions,
        &fixture.runtime.input_tx,
    )
    .unwrap();
}

pub(super) fn next_command(fixture: &ReviewerFixture) -> CodexRuntimeCommand {
    fixture
        .input_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("bounded worker continuation")
}

pub(super) fn drive_ready(fixture: &ReviewerFixture, command: CodexRuntimeCommand) -> Vec<u8> {
    let CodexRuntimeCommand::ReviewerMcpReady {
        scope,
        command,
        watchdog,
    } = command
    else {
        panic!("expected readiness continuation");
    };
    let mut wire = Vec::new();
    handle_shared_codex_start_turn_inner(
        &mut wire,
        &fixture.pending,
        &fixture.state,
        &fixture.runtime.runtime_id,
        &fixture.runtime.sessions,
        &fixture.runtime.thread_sessions,
        None,
        &fixture.child,
        REVIEW_THREAD,
        watchdog,
        command,
        Some(&scope),
    )
    .unwrap();
    wire
}

pub(super) fn archive_and_result(fixture: &ReviewerFixture) -> DelegationResult {
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        params,
        response_tx,
        ..
    } = next_command(fixture)
    else {
        panic!("expected exact child archive after terminal persistence");
    };
    assert_eq!(method, "thread/archive");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    response_tx.send(Ok(json!({}))).unwrap();
    fixture
        .state
        .get_delegation_result(&fixture.parent, &fixture.delegation)
        .unwrap()
        .result
}

pub(super) fn assert_observation(
    fixture: &ReviewerFixture,
    result: &DelegationResult,
    phase: &str,
    outcome: CodexReviewerMcpOutcome,
) {
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.reviewer_mcp_observations.len(), 1);
    let observation = &result.reviewer_mcp_observations[0];
    assert_eq!(observation.phase, phase);
    assert_eq!(observation.outcome, outcome);
    assert_eq!(observation.budget_ms, 60_000);
    assert!(!observation.measurement.is_empty());
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let delegation = saved
        .delegations
        .iter()
        .find(|d| d.id == fixture.delegation)
        .unwrap();
    assert_eq!(delegation.result.as_ref(), Some(result));
    let child = saved
        .sessions
        .iter()
        .find(|r| r.session.id == fixture.child)
        .unwrap();
    assert!(
        serde_json::to_string(&child.session.messages)
            .unwrap()
            .contains(
                &serde_json::to_string(observation)
                    .unwrap()
                    .replace('"', "\\\"")
            )
    );
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(
        fixture.input_rx.try_recv().is_err(),
        "no model start or duplicate cleanup"
    );
}

#[test]
fn codex_reviewer_mcp_ready_starts_only_after_exact_status_and_keeps_readonly_policy() {
    let fixture = ReviewerFixture::new("reviewer-ready-control");
    fixture
        .runtime
        .sessions
        .lock()
        .unwrap()
        .get_mut(&fixture.child)
        .unwrap()
        .reviewer_mcp_setup_started = Some(std::time::Instant::now());
    let written = fixture.start();
    assert!(!String::from_utf8_lossy(&written).contains("turn/start"));
    respond_start(&fixture, &written, ready_page());
    let continuation = next_command(&fixture);
    let wire = drive_ready(&fixture, continuation);
    let request: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(request["method"], "turn/start");
    assert_eq!(request["params"]["threadId"], REVIEW_THREAD);
    assert_eq!(request["params"]["approvalPolicy"], "never");
    assert_eq!(request["params"]["sandboxPolicy"]["type"], "readOnly");
    assert_eq!(
        request["params"]["input"][0]["text"],
        "Review the supplied source."
    );
    let inner = fixture.state.inner.lock().unwrap();
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    let observation = &d.attempt.reviewer_mcp_observations[0];
    assert_eq!(observation.outcome, CodexReviewerMcpOutcome::Ready);
    assert!(
        observation
            .measurement
            .starts_with("thread setup initiated")
    );
    assert!(observation.measurement.contains("not isolated"));
    drop(inner);
    handle_shared_codex_app_server_message(
        &json!({"id": request["id"], "result": {
        "turn": {"id": REVIEW_TURN}}}),
        &fixture.state,
        &fixture.runtime.runtime_id,
        &fixture.pending,
        &fixture.runtime.sessions,
        &fixture.runtime.thread_sessions,
        &fixture.runtime.input_tx,
    )
    .unwrap();
    fixture
        .state
        .submit_delegation_review_result(&fixture.child, structured_review_request())
        .unwrap();
    fixture.complete();
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Completed);
    assert_eq!(result.summary, "One medium issue found.");
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn codex_reviewer_mcp_start_failure_matrix_preserves_typed_durable_diagnostics() {
    for (label, page, outcome) in [
        (
            "failed",
            json!({"data": [{"name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "runtimeStatus": "failed", "tools": {}, "toolsError": "bridge discovery failed"}]}),
            CodexReviewerMcpOutcome::Failed,
        ),
        (
            "missing-server",
            json!({"data": [{"name": "sibling-server", "runtimeStatus": "connected",
            "tools": {"private-inventory": {"name": TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME}}}]}),
            CodexReviewerMcpOutcome::MissingServer,
        ),
        (
            "missing-tool",
            json!({"data": [{"name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "runtimeStatus": "connected", "tools": {TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME: {"name": "other-tool"}}}]}),
            CodexReviewerMcpOutcome::MissingTool,
        ),
        (
            "invalid",
            json!({"data": "not an array", "private-inventory": "secret-config"}),
            CodexReviewerMcpOutcome::Invalid,
        ),
    ] {
        let fixture = ReviewerFixture::new(label);
        fixture.runtime.sessions.lock().unwrap().insert(
            "sibling-session".to_owned(),
            SharedCodexSessionState {
                thread_id: Some("sibling-thread".to_owned()),
                turn_id: Some("sibling-turn".to_owned()),
                turn_started: true,
                ..Default::default()
            },
        );
        let written = fixture.start();
        respond_start(&fixture, &written, page);
        let result = archive_and_result(&fixture);
        assert_observation(&fixture, &result, "startup", outcome);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-inventory")
        );
        let shared = fixture.runtime.sessions.lock().unwrap();
        let sibling = shared.get("sibling-session").unwrap();
        assert_eq!(sibling.thread_id.as_deref(), Some("sibling-thread"));
        assert_eq!(sibling.turn_id.as_deref(), Some("sibling-turn"));
        assert!(sibling.turn_started);
    }
}

#[test]
fn codex_reviewer_mcp_query_errors_and_timeout_are_observed_without_shared_teardown() {
    for (label, error, outcome) in [
        (
            "rpc",
            CodexResponseError::JsonRpc("raw config secret must not leak".to_owned()),
            CodexReviewerMcpOutcome::Unavailable,
        ),
        (
            "transport",
            CodexResponseError::Transport("routing secret must not leak".to_owned()),
            CodexReviewerMcpOutcome::Unavailable,
        ),
        (
            "timeout",
            CodexResponseError::Timeout("bounded fixture timeout".to_owned()),
            CodexReviewerMcpOutcome::Timeout,
        ),
    ] {
        let fixture = ReviewerFixture::new(label);
        let written = fixture.start();
        let request: Value = serde_json::from_slice(&written).unwrap();
        let id = request["id"].as_str().unwrap();
        fixture
            .pending
            .lock()
            .unwrap()
            .remove(id)
            .unwrap()
            .send(Err(error))
            .unwrap();
        let result = archive_and_result(&fixture);
        assert_observation(&fixture, &result, "startup", outcome);
        assert_eq!(
            result.reviewer_mcp_observations[0].reason,
            "No startup reason supplied."
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("must not leak")
        );
    }
}

#[test]
fn codex_reviewer_mcp_initial_send_failure_is_a_persisted_observation() {
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture pipe closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = ReviewerFixture::new("initial-query-send-error");
    fixture.start_with(&mut BrokenWriter);
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "startup",
        CodexReviewerMcpOutcome::Unavailable,
    );
}

#[test]
fn codex_reviewer_mcp_starting_rechecks_and_exact_pagination_do_not_fail_early() {
    for first in [
        json!({"data": [{"name": TERMAL_DELEGATION_MCP_SERVER_NAME,
        "runtimeStatus": "starting", "tools": {}}], "nextCursor": null}),
        json!({"data": [], "nextCursor": "exact-next-page"}),
    ] {
        let fixture = ReviewerFixture::new("starting-or-pagination");
        let written = fixture.start();
        respond_start(&fixture, &written, first.clone());
        let CodexRuntimeCommand::JsonRpcRequest {
            method,
            params,
            timeout,
            response_tx,
        } = next_command(&fixture)
        else {
            panic!("still starting must recheck rather than fail or start");
        };
        assert_eq!(method, "mcpServerStatus/list");
        assert_eq!(params["threadId"], REVIEW_THREAD);
        assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
        assert!(timeout <= CODEX_REVIEWER_MCP_BUDGET);
        assert_eq!(
            params.get("cursor"),
            first.get("nextCursor").filter(|v| !v.is_null())
        );
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()].status,
            DelegationStatus::Running
        );
        assert!(
            inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()]
                .attempt
                .reviewer_mcp_observations
                .is_empty()
        );
        drop(inner);
        response_tx.send(Ok(ready_page())).unwrap();
        let continuation = next_command(&fixture);
        let CodexRuntimeCommand::ReviewerMcpReady { scope, .. } = continuation else {
            panic!("ready continuation");
        };
        assert!(codex_reviewer_mcp_current(
            &fixture.state.inner.lock().unwrap(),
            &scope
        ));
    }
}

#[test]
fn codex_reviewer_mcp_repeated_cursor_is_bounded_invalid_not_a_ready_sibling() {
    let fixture = ReviewerFixture::new("pagination-loop");
    let written = fixture.start();
    let page = json!({"data": [], "nextCursor": "same"});
    respond_start(&fixture, &written, page.clone());
    let CodexRuntimeCommand::JsonRpcRequest { response_tx, .. } = next_command(&fixture) else {
        panic!("status page");
    };
    response_tx.send(Ok(page)).unwrap();
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "startup",
        CodexReviewerMcpOutcome::Invalid,
    );
}

#[test]
fn codex_reviewer_mcp_reason_sanitization_bounds_and_omits_secret_inventories() {
    for raw in [
        "config={private}",
        "Bearer routing-credential",
        "https://private/path",
        "TOKEN=private",
        "password private",
    ] {
        let reason = codex_reviewer_mcp_reason(Some(raw));
        assert_eq!(
            reason,
            "Startup reason supplied; sensitive detail withheld."
        );
        assert!(!reason.contains("private"));
    }
    let long = "safe diagnostic ".repeat(1000);
    assert!(codex_reviewer_mcp_reason(Some(&long)).chars().count() <= 512);
    assert_eq!(
        codex_reviewer_mcp_reason(None),
        "No startup reason supplied."
    );
    assert_eq!(
        codex_reviewer_mcp_reason(Some("bridge discovery failed")),
        "bridge discovery failed"
    );
}
