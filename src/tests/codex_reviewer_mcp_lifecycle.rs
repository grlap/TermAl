// Owns completion, notification and stale-continuation lifecycle witnesses.
use super::controls::{
    archive_and_result, assert_observation, drive_ready, next_command, ready_page, respond_start,
};
use super::*;

fn notify(fixture: &ReviewerFixture, thread: Value, error: Value) {
    handle_shared_codex_app_server_message(
        &json!({"method": "mcpServer/startupStatus/updated",
        "params": {"threadId": thread, "name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "status": "failed", "error": error}}),
        &fixture.state,
        &fixture.runtime.runtime_id,
        &fixture.pending,
        &fixture.runtime.sessions,
        &fixture.runtime.thread_sessions,
        &fixture.runtime.input_tx,
    )
    .unwrap();
}

#[test]
fn codex_reviewer_mcp_late_failed_notification_retains_reason_and_exact_scope() {
    let fixture = ReviewerFixture::new("late-notification");
    let written = fixture.start();
    notify(&fixture, json!("other-thread"), json!("other failure"));
    notify(&fixture, Value::Null, json!("app-wide failure"));
    assert!(
        fixture
            .runtime
            .sessions
            .lock()
            .unwrap()
            .get(&fixture.child)
            .unwrap()
            .reviewer_mcp_failure
            .is_none()
    );
    assert!(fixture.input_rx.try_recv().is_err());
    notify(
        &fixture,
        json!(REVIEW_THREAD),
        json!("fixture bridge startup failed"),
    );
    // No status response is needed: the exact failed notification is itself
    // bounded failure evidence. The reader remains able to consume responses.
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "startup",
        CodexReviewerMcpOutcome::Failed,
    );
    assert_eq!(
        result.reviewer_mcp_observations[0].reason,
        "fixture bridge startup failed"
    );
    respond_start(&fixture, &written, ready_page());
    assert!(
        fixture.input_rx.try_recv().is_err(),
        "late ready response cannot restart a failed reviewer"
    );
}

#[test]
fn codex_reviewer_mcp_notification_secret_is_withheld_in_result_and_transcript() {
    let fixture = ReviewerFixture::new("notification-redaction");
    fixture.start();
    notify(
        &fixture,
        json!(REVIEW_THREAD),
        json!("config={routing-secret-private}"),
    );
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "startup",
        CodexReviewerMcpOutcome::Failed,
    );
    assert_eq!(
        result.reviewer_mcp_observations[0].reason,
        "Startup reason supplied; sensitive detail withheld."
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("routing-secret-private")
    );
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let child = saved
        .sessions
        .iter()
        .find(|r| r.session.id == fixture.child)
        .unwrap();
    assert!(
        !serde_json::to_string(&child.session.messages)
            .unwrap()
            .contains("routing-secret-private")
    );
}

#[test]
fn codex_reviewer_mcp_finish_duplicates_share_one_query_and_preserve_failure() {
    let fixture = ReviewerFixture::new("duplicate-completion");
    fixture.complete();
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = next_command(&fixture)
    else {
        panic!("finish status");
    };
    assert_eq!(method, "mcpServerStatus/list");
    fixture.complete();
    assert!(
        fixture.input_rx.try_recv().is_err(),
        "duplicate completion cannot issue another observation or archive early"
    );
    assert!(
        fixture.start().is_empty(),
        "a duplicate start cannot replace the owning finish observation"
    );
    assert!(fixture.input_rx.try_recv().is_err());
    response_tx
        .send(Err(CodexResponseError::Transport(
            "fixture query unavailable".to_owned(),
        )))
        .unwrap();
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "finish",
        CodexReviewerMcpOutcome::Unavailable,
    );
}

#[test]
fn codex_reviewer_mcp_error_completion_observes_before_terminal_cleanup() {
    let fixture = ReviewerFixture::new("error-completion");
    fixture.complete_with(json!({"message": "review execution failed"}));
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = next_command(&fixture)
    else {
        panic!("finish status before archive");
    };
    assert_eq!(method, "mcpServerStatus/list");
    response_tx.send(Ok(ready_page())).unwrap();
    let result = archive_and_result(&fixture);
    assert_observation(&fixture, &result, "finish", CodexReviewerMcpOutcome::Ready);
    assert!(
        result.summary.contains("review execution failed"),
        "original completion error must not be replaced by a ready bridge"
    );
}

#[test]
fn codex_reviewer_mcp_submission_during_finish_query_is_immutable() {
    let fixture = ReviewerFixture::new("submission-during-query");
    fixture.complete();
    let CodexRuntimeCommand::JsonRpcRequest {
        method,
        response_tx,
        ..
    } = next_command(&fixture)
    else {
        panic!("finish status");
    };
    assert_eq!(method, "mcpServerStatus/list");
    fixture
        .state
        .submit_delegation_review_result(&fixture.child, structured_review_request())
        .unwrap();
    response_tx
        .send(Err(CodexResponseError::Transport(
            "late transport failure".to_owned(),
        )))
        .unwrap();
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Completed);
    assert_eq!(result.summary, "One medium issue found.");
    assert_eq!(result.findings.len(), 1);
    assert!(
        result.reviewer_mcp_observations.is_empty(),
        "late failure cannot mutate authoritative submission"
    );
    let saved = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let d = saved
        .delegations
        .iter()
        .find(|d| d.id == fixture.delegation)
        .unwrap();
    assert_eq!(d.result.as_ref(), Some(&result));
}

#[test]
fn codex_reviewer_mcp_ready_continuation_is_inert_after_stop_reset_and_replacement() {
    for cause in [
        "stop",
        "reset",
        "generation",
        "runtime",
        "thread",
        "attempt",
        "gate",
        "mode",
    ] {
        let fixture = ReviewerFixture::new(cause);
        let written = fixture.start();
        respond_start(&fixture, &written, ready_page());
        let continuation = next_command(&fixture);
        if cause == "stop" {
            // Use the real public Stop owner claim, without spawning an
            // interrupt worker; the readiness callback must respect that fence.
            let (_, claim) = fixture
                .state
                .begin_requested_stop_session(&fixture.child, &StopSessionOptions::default())
                .unwrap();
            assert!(claim.is_some());
        } else if cause == "gate" {
            fixture
                .runtime
                .sessions
                .lock()
                .unwrap()
                .get_mut(&fixture.child)
                .unwrap()
                .reviewer_mcp_gate = Some("replacement-query".to_owned());
        } else {
            // Explicitly source-arranged invalidations of the existing exact
            // identity fields, not claims of independently executed reset workers.
            let mut inner = fixture.state.inner.lock().unwrap();
            let i = inner.find_session_index(&fixture.child).unwrap();
            match cause {
                "reset" => inner.sessions[i].runtime_reset_required = true,
                "generation" => inner.sessions[i].active_turn_generation += 1,
                "runtime" => inner.sessions[i].runtime = SessionRuntime::None,
                "thread" => {
                    inner.sessions[i].external_session_id = Some("replacement-thread".to_owned())
                }
                "attempt" => {
                    let d = inner.find_delegation_index(&fixture.delegation).unwrap();
                    inner.delegations[d].review_result_submission_attempt += 1;
                }
                "mode" => {
                    let d = inner.find_delegation_index(&fixture.delegation).unwrap();
                    inner.delegations[d].mode = DelegationMode::Explorer;
                }
                _ => unreachable!(),
            }
        }
        let before = fixture
            .state
            .inner
            .lock()
            .unwrap()
            .delegations
            .iter()
            .find(|d| d.id == fixture.delegation)
            .unwrap()
            .attempt
            .reviewer_mcp_observations
            .clone();
        assert!(
            drive_ready(&fixture, continuation).is_empty(),
            "{cause}: stale continuation must not send review work"
        );
        assert!(fixture.pending.lock().unwrap().is_empty());
        if cause == "reset" {
            {
                let inner = fixture.state.inner.lock().unwrap();
                let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
                let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
                assert_eq!(child.session.status, SessionStatus::Error,
                    "reset must retain terminal duty, not merely refuse model work");
                assert_eq!(d.status, DelegationStatus::Failed);
                assert_eq!(d.attempt.reviewer_mcp_observations, before);
            }
            // Terminal reset cleanup may enqueue only this exact archive;
            // successor and Stop controls keep their immediate empty check.
            let result = archive_and_result(&fixture);
            assert_eq!(result.status, DelegationStatus::Failed);
            assert!(result.summary.contains(
                "Reviewer MCP lost send eligibility before model work started."));
        }
        assert!(fixture.input_rx.try_recv().is_err());
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
        assert_eq!(d.attempt.reviewer_mcp_observations, before);
        if cause != "reset" {
            assert_eq!(child.session.status,
                if cause == "stop" { SessionStatus::Stopping } else { SessionStatus::Active });
            assert_eq!(d.status, DelegationStatus::Running);
            if cause == "stop" {
                assert!(child.deferred_stop_callbacks.is_empty(),
                    "the public Stop owner remains untouched by a stale Ready continuation");
            }
        }
    }
}

#[test]
fn codex_reviewer_mcp_failed_notification_wins_a_racing_ready_response() {
    let fixture = ReviewerFixture::new("failed-versus-ready");
    let written = fixture.start();
    let request: Value = serde_json::from_slice(&written).unwrap();
    // Hold the observation fence while publishing both local facts, so neither
    // worker scheduling order can turn an observed startup failure into Ready.
    let mut shared = fixture.runtime.sessions.lock().unwrap();
    fixture
        .pending
        .lock()
        .unwrap()
        .remove(request["id"].as_str().unwrap())
        .unwrap()
        .send(Ok(ready_page()))
        .unwrap();
    shared.get_mut(&fixture.child).unwrap().reviewer_mcp_failure =
        Some("racing startup failed".to_owned());
    drop(shared);
    let result = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &result,
        "startup",
        CodexReviewerMcpOutcome::Failed,
    );
    assert_eq!(
        result.reviewer_mcp_observations[0].reason,
        "racing startup failed"
    );
}

#[test]
fn codex_reviewer_mcp_wait_deadline_is_bounded_and_owner_changes_abort_without_waiting() {
    let fixture = ReviewerFixture::new("wait-deadline-and-stop");
    let scope = codex_reviewer_mcp_scope(
        &fixture.state,
        &fixture.child,
        REVIEW_THREAD,
        &fixture.runtime.runtime_id,
    )
    .unwrap();
    fixture
        .runtime
        .sessions
        .lock()
        .unwrap()
        .get_mut(&fixture.child)
        .unwrap()
        .reviewer_mcp_gate = Some(scope.gate.clone());
    let (_tx, rx) = mpsc::channel();
    let result = wait_codex_reviewer_mcp_response(
        &fixture.state,
        &fixture.runtime.sessions,
        &scope,
        rx,
        std::time::Instant::now(),
    );
    assert!(
        matches!(result, Err(CodexResponseError::Timeout(_))),
        "injected elapsed deadline, not a claim of a real sixty-second wait"
    );
    assert_eq!(CODEX_REVIEWER_MCP_BUDGET, Duration::from_secs(60));
    let (_, claim) = fixture
        .state
        .begin_requested_stop_session(&fixture.child, &StopSessionOptions::default())
        .unwrap();
    assert!(claim.is_some());
    let (_tx, rx) = mpsc::channel();
    let result = wait_codex_reviewer_mcp_response(
        &fixture.state,
        &fixture.runtime.sessions,
        &scope,
        rx,
        std::time::Instant::now() + CODEX_REVIEWER_MCP_BUDGET,
    );
    assert!(matches!(result, Err(CodexResponseError::Transport(_))));
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn codex_reviewer_mcp_superseded_worker_cannot_publish_or_start() {
    let fixture = ReviewerFixture::new("superseded-observation");
    // Both requests are real starts on the same attachment. Writer serialization
    // does not serialize their off-lock observations; the newer query owns them.
    let first = fixture.start();
    let second = fixture.start();
    respond_start(&fixture, &first, ready_page());
    respond_start(&fixture, &second, ready_page());
    let continuation = next_command(&fixture);
    let CodexRuntimeCommand::ReviewerMcpReady { scope, .. } = &continuation else {
        panic!("new exact ready continuation");
    };
    assert_eq!(
        fixture
            .runtime
            .sessions
            .lock()
            .unwrap()
            .get(&fixture.child)
            .unwrap()
            .reviewer_mcp_gate
            .as_deref(),
        Some(scope.gate.as_str())
    );
    let wire = drive_ready(&fixture, continuation);
    let request: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(request["method"], "turn/start");
    assert!(fixture.input_rx.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(d.attempt.reviewer_mcp_observations.len(), 1);
    assert_eq!(
        d.attempt.reviewer_mcp_observations[0].outcome,
        CodexReviewerMcpOutcome::Ready
    );
}
