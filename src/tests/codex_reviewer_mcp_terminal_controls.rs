// Owns exact post-readiness save cuts, public output and temporary-fence duties.
// SQLite faults affect only the fixture store; no live store or timing injection.
use super::controls::{archive_and_result, drive_ready, next_command, ready_page, respond_start};
use super::*;

fn startup_failed(fixture: &ReviewerFixture) {
    handle_shared_codex_app_server_message(
        &json!({"method": "mcpServer/startupStatus/updated", "params": {
            "threadId": REVIEW_THREAD, "name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "status": "failed", "error": "failure after ready publication"
        }}),
        &fixture.state, &fixture.runtime.runtime_id, &fixture.pending,
        &fixture.runtime.sessions, &fixture.runtime.thread_sessions, &fixture.runtime.input_tx,
    ).unwrap();
}

pub(super) fn saved_delegation(fixture: &ReviewerFixture) -> DelegationRecord {
    let connection = rusqlite::Connection::open_with_flags(
        fixture.state.persistence_path.as_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).unwrap();
    let encoded: String = connection.query_row(
        "SELECT value_json FROM delegations WHERE id = ?1", [&fixture.delegation],
        |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn post_ready_save_failure(observation_save: bool) {
    let fixture = ReviewerFixture::new("post-ready-save-failure");
    let written = fixture.start();
    respond_start(&fixture, &written, ready_page());
    let continuation = next_command(&fixture);
    assert!(matches!(&continuation, CodexRuntimeCommand::ReviewerMcpReady { .. }));
    assert_eq!(saved_delegation(&fixture).attempt.reviewer_mcp_observations[0].outcome,
        CodexReviewerMcpOutcome::Ready, "Ready must already be durably saved at this cut");
    startup_failed(&fixture);
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    if observation_save {
        let ready: String = connection.query_row(
            "SELECT json_extract(value_json, '$.reviewerMcpObservations[0].outcome') FROM delegations WHERE id = ?1",
            [&fixture.delegation], |row| row.get(0),
        ).unwrap();
        assert_eq!(ready, "ready", "the raw trigger path must read the saved Ready observation");
        // The intervening runtime-config save still contains Ready and succeeds.
        // Only publishing the new Failed observation (and its later terminal save)
        // reaches this trigger; this is not a config-save refusal witness.
        connection.execute_batch(
            "CREATE TRIGGER reject_failed_observation BEFORE INSERT ON delegations
             WHEN json_extract(NEW.value_json, '$.reviewerMcpObservations[0].outcome') = 'failed'
             BEGIN SELECT RAISE(ABORT, 'fixture observation save unavailable'); END;",
        ).unwrap();
    } else {
        // Observation publication writes Active. Only the subsequent terminal
        // mutation writes Error, so the observation itself remains durable.
        connection.execute_batch(
            "CREATE TRIGGER reject_error_terminal BEFORE INSERT ON sessions
             WHEN json_extract(NEW.value_json, '$.session.status') = 'error'
             BEGIN SELECT RAISE(ABORT, 'fixture terminal save unavailable'); END;",
        ).unwrap();
    }
    drop(connection);
    let wire = drive_ready(&fixture, continuation);
    assert!(wire.is_empty(), "no turn/start after the exact-thread failure");
    let durable = saved_delegation(&fixture);
    assert_eq!(durable.attempt.reviewer_mcp_observations[0].outcome,
        if observation_save { CodexReviewerMcpOutcome::Ready } else { CodexReviewerMcpOutcome::Failed },
        "the two controls must discriminate failed observation vs failed later terminal save");
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Error);
    let delegation = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(delegation.attempt.reviewer_mcp_observations[0].outcome, CodexReviewerMcpOutcome::Failed,
        "the real send-boundary observation must be reached");
    assert_eq!(delegation.status, DelegationStatus::Failed,
        "the parent must not stay Running after the child terminal save fails");
    let result = delegation.result.as_ref().expect("publish an in-memory terminal result");
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.reviewer_mcp_observations[0].outcome, CodexReviewerMcpOutcome::Failed);
    assert!(durable.result.is_none(), "do not claim an unsaved terminal result was durable");
    drop(inner);
    assert!(fixture.pending.lock().unwrap().is_empty());
    // Cleanup may request an archive, but it must never request model work.
    while let Ok(command) = fixture.input_rx.try_recv() {
        let CodexRuntimeCommand::JsonRpcRequest { method, response_tx, .. } = command else {
            panic!("no model continuation after a failed boundary");
        };
        assert_eq!(method, "thread/archive");
        let _ = response_tx.send(Ok(json!({})));
    }
}

#[test]
fn codex_reviewer_mcp_post_ready_observation_save_failure_settles_parent() {
    post_ready_save_failure(true);
}

#[test]
fn codex_reviewer_mcp_post_ready_terminal_save_failure_settles_parent() {
    post_ready_save_failure(false);
}

#[test]
fn codex_reviewer_mcp_failed_public_output_keeps_reviewer_prose() {
    let fixture = ReviewerFixture::new("failed-public-review-output");
    let prose = "## Result\nA complete human-readable review remains available without a submission.";
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let id = inner.next_message_id();
        let i = inner.find_session_index(&fixture.child).unwrap();
        push_message_on_record(inner.session_mut_by_index(i).unwrap(), Message::Text {
            id, timestamp: stamp_now(), author: Author::Assistant, text: prose.to_owned(),
            attachments: Vec::new(), expanded_text: None, source: None,
        });
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    fixture.complete();
    let CodexRuntimeCommand::JsonRpcRequest { method, response_tx, .. } = next_command(&fixture) else {
        panic!("the real completion must query before archive");
    };
    assert_eq!(method, "mcpServerStatus/list");
    response_tx.send(Ok(ready_page())).unwrap();
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.reviewer_mcp_observations[0].phase, "finish");
    let output = fixture.state.get_delegation_result_output(
        &fixture.parent, &fixture.delegation, 0, 256,
    ).unwrap();
    assert_eq!(output.output, prose, "public paging must not substitute host bookkeeping for reviewer prose");
    assert!(output.complete);
    assert!(!output.summary_fallback);
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert!(child.session.messages.iter().any(|message| matches!(message,
        Message::Text { author: Author::System, text, .. } if text.starts_with("Reviewer MCP observation:"))),
        "the host observation stays in the transcript as host-authored metadata");
}

pub(super) fn join_worker(worker: std::thread::JoinHandle<()>) {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || { let _ = done_tx.send(worker.join().is_ok()); });
    assert!(done_rx.recv_timeout(Duration::from_secs(3)).expect("bounded observation worker must finish"));
}

pub(super) fn worker_at_status_wait(fixture: &ReviewerFixture, startup: bool) -> std::thread::JoinHandle<()> {
    let scope = codex_reviewer_mcp_scope(
        &fixture.state, &fixture.child, REVIEW_THREAD, &fixture.runtime.runtime_id,
    ).unwrap();
    worker_with_scope(fixture, startup, scope)
}

fn worker_with_scope(fixture: &ReviewerFixture, startup: bool, scope: CodexReviewerMcpScope) -> std::thread::JoinHandle<()> {
    fixture.runtime.sessions.lock().unwrap().get_mut(&fixture.child).unwrap()
        .reviewer_mcp_gate = Some(scope.gate.clone());
    let start = startup.then(|| (CodexPromptCommand {
        active_turn_generation: 0, approval_policy: CodexApprovalPolicy::Never,
        attachments: Vec::new(), cwd: fixture.state.default_workdir.clone(),
        model: "gpt-5.4".to_owned(), prompt: "Review local source.".to_owned(),
        reasoning_effort: CodexReasoningEffort::Medium, service_tier: None,
        resume_thread_id: None, sandbox_mode: CodexSandboxMode::ReadOnly,
    }, None));
    // Drive the actual worker, not a replacement model. The direct attachment
    // arranges the phase; the queried request proves its current-owner loop ran.
    spawn_codex_reviewer_mcp_observation(
        fixture.state.clone(), fixture.runtime.sessions.clone(), scope, None, start, None,
    )
}

fn temporary_revocation_finishes(startup: bool, invalidate: Option<&str>) {
    let fixture = ReviewerFixture::new("reviewer-temporary-revocation");
    let worker = worker_at_status_wait(&fixture, startup);
    let CodexRuntimeCommand::JsonRpcRequest { method, response_tx, .. } = next_command(&fixture) else {
        panic!("worker must own a real bounded status wait before revocation");
    };
    assert_eq!(method, "mcpServerStatus/list");
    let submitted = if invalidate == Some("submission") {
        fixture.state.submit_delegation_review_result(&fixture.child, structured_review_request()).unwrap();
        let inner = fixture.state.inner.lock().unwrap();
        Some(inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()]
            .submitted_review_result.clone().unwrap())
    } else { None };
    let batch = {
        let mut inner = fixture.state.inner.lock().unwrap();
        if invalidate == Some("replacement") {
            let i = inner.find_session_index(&fixture.child).unwrap();
            inner.session_mut_by_index(i).unwrap().external_session_id = Some("replacement-thread".to_owned());
        }
        let batch = claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()]);
        assert_eq!(batch.targets.len(), 1, "use the actual revocation owner");
        batch
    };
    // Keep the real owner fenced until the worker has yielded its duty. The
    // retained response sender makes this an owner invalidation, not disconnect.
    join_worker(worker);
    let buffered = {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert!(child.runtime_stop_in_progress);
        assert_eq!(child.session.status, SessionStatus::Active, "the worker may not clear/override a stop owner");
        if submitted.is_some() {
            assert!(matches!(child.deferred_stop_callbacks.as_slice(),
                [DeferredStopCallback::TurnFailed { active_turn_generation: 0, .. }]));
        }
        child.deferred_stop_callbacks.len()
    };
    let release = fixture.state.release_engram_mcp_runtime_revocations_without_teardown(batch);
    for (session, token, callbacks) in release.deferred_callbacks {
        fixture.state.replay_deferred_runtime_stop_callbacks(&session, &token, callbacks);
    }
    drop(response_tx);
    if invalidate == Some("submission") {
        assert_eq!(buffered, 1, "submission preserves its payload, not an abandoned completion duty");
        assert_eq!(archive_and_result(&fixture), submitted.unwrap());
        let saved = saved_delegation(&fixture);
        assert_eq!(saved.status, DelegationStatus::Completed);
        assert!(saved.submitted_review_result.is_none());
        assert!(saved.attempt.reviewer_mcp_observations.is_empty());
        assert!(saved.post_submission_transport_error.as_deref().is_some_and(|detail|
            detail.contains(if startup { "Reviewer MCP bridge is unavailable" } else { "Reviewer MCP status wait interrupted" })));
        assert!(fixture.input_rx.try_recv().is_err(), "promotion cannot restart model work");
    } else if invalidate.is_some() {
        assert_eq!(buffered, 0, "replacement cannot receive a stale failure");
        let inner = fixture.state.inner.lock().unwrap();
        let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
        assert!(d.attempt.reviewer_mcp_observations.is_empty());
        assert_eq!(d.submitted_review_result, submitted);
        assert!(fixture.input_rx.try_recv().is_err(), "no stale model command or cleanup");
    } else {
        assert_eq!(buffered, 1, "the temporary fence must own a guarded deferred failure, not drop the turn");
        let result = archive_and_result(&fixture);
        assert_eq!(result.status, DelegationStatus::Failed);
        assert_eq!(result.reviewer_mcp_observations.len(), 1);
        assert_eq!(result.reviewer_mcp_observations[0].phase, if startup { "startup" } else { "finish" });
        assert_eq!(result.reviewer_mcp_observations[0].outcome, CodexReviewerMcpOutcome::Unavailable);
        assert!(fixture.input_rx.try_recv().is_err(), "never send or reauthorize the stopped review turn");
    }
}

#[test]
fn codex_reviewer_mcp_startup_revocation_release_replays_terminal_duty() {
    temporary_revocation_finishes(true, None);
}

#[test]
fn codex_reviewer_mcp_finish_revocation_release_replays_terminal_duty() {
    temporary_revocation_finishes(false, None);
}

#[test]
fn codex_reviewer_mcp_revocation_does_not_settle_replacement() {
    for startup in [true, false] { temporary_revocation_finishes(startup, Some("replacement")); }
}

#[test]
fn codex_reviewer_mcp_revocation_keeps_authoritative_submission() {
    for startup in [true, false] { temporary_revocation_finishes(startup, Some("submission")); }
}

#[test]
fn codex_reviewer_mcp_released_fence_still_finishes_old_wait() {
    for startup in [true, false] {
        let fixture = ReviewerFixture::new("reviewer-already-released-revocation");
        let scope = codex_reviewer_mcp_scope(
            &fixture.state, &fixture.child, REVIEW_THREAD, &fixture.runtime.runtime_id,
        ).unwrap();
        let batch = {
            let mut inner = fixture.state.inner.lock().unwrap();
            claim_engram_mcp_runtime_revocations_locked(&mut inner, &[fixture.child.clone()])
        };
        assert_eq!(batch.targets.len(), 1);
        let release = fixture.state.release_engram_mcp_runtime_revocations_without_teardown(batch);
        assert!(release.deferred_callbacks.is_empty());
        // Arrange the delayed worker entry after the actual owner release. Its
        // original scope is retained, not silently renewed to the new fence.
        join_worker(worker_with_scope(&fixture, startup, scope));
        let result = archive_and_result(&fixture);
        assert_eq!(result.status, DelegationStatus::Failed);
        assert_eq!(result.reviewer_mcp_observations[0].outcome, CodexReviewerMcpOutcome::Unavailable);
        assert_eq!(result.reviewer_mcp_observations[0].phase, if startup { "startup" } else { "finish" });
        assert!(fixture.input_rx.try_recv().is_err(), "an old wait cannot start model work after release");
    }
}

fn reset_during_status_wait(startup: bool) {
    let fixture = ReviewerFixture::new("reviewer-reset-during-status-wait");
    // Finish must acquire its policy through the real completion admission,
    // not the startup-shaped scope used by the revocation-only controls.
    let worker = if startup { Some(worker_at_status_wait(&fixture, true)) } else {
        fixture.complete();
        None
    };
    let CodexRuntimeCommand::JsonRpcRequest { method, params, response_tx, .. } = next_command(&fixture) else {
        panic!("the worker must issue its exact-thread status request before reset");
    };
    assert_eq!(method, "mcpServerStatus/list");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
    {
        // Arrange only the public setters' marker, not a public reset execution.
        // Hold the observation gate while making the marker and Ready response
        // available, so recording cannot win before the reset becomes visible.
        let shared = fixture.runtime.sessions.lock().unwrap();
        let mut inner = fixture.state.inner.lock().unwrap();
        let i = inner.find_session_index(&fixture.child).unwrap();
        assert_eq!(inner.sessions[i].session.status, SessionStatus::Active);
        inner.sessions[i].runtime_reset_required = true;
        response_tx.send(Ok(ready_page())).unwrap();
        drop(inner);
        drop(shared);
    }
    if let Some(worker) = worker { join_worker(worker); }
    // Exact archive is also the real finish worker's completion barrier.
    let result = archive_and_result(&fixture);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Error,
            "a reset marker cannot abandon the original generation's terminal duty");
        assert_eq!(d.status, DelegationStatus::Failed);
        assert!(child.deferred_stop_callbacks.is_empty(), "reset has no deferred stop owner");
        let result = d.result.as_ref().expect("the parent must receive the terminal result");
        assert_eq!(result.reviewer_mcp_observations.len(), 1);
        let observation = &result.reviewer_mcp_observations[0];
        assert_eq!(observation.phase, if startup { "startup" } else { "finish" });
        if startup {
            assert_eq!(observation.outcome, CodexReviewerMcpOutcome::Unavailable);
            assert_eq!(observation.reason,
                "Reviewer status wait interrupted by a runtime stop or reset fence.");
            assert!(child.session.messages.iter().any(|message| matches!(message,
                Message::Text { author: Author::System, text, .. }
                    if text.contains("runtime stop or reset fence"))));
        } else {
            assert_eq!(observation.outcome, CodexReviewerMcpOutcome::Ready);
            assert!(!observation.reason.contains("runtime stop or reset fence"));
            assert_eq!(result.summary, "child finished without a result packet");
            assert_eq!(child.session.preview, "child finished without a result packet");
            assert!(!child.session.messages.iter().any(|message| matches!(message,
                Message::Text { text, .. } if text.contains("Turn failed"))),
                "the clean finish is failed only by the missing-packet delegation refresh");
            assert!(child.session.messages.iter().any(|message| matches!(message,
                Message::Text { author: Author::System, text, .. }
                    if text.contains("Reviewer MCP observation:") && text.contains("ready"))));
        }
    }
    // Neither a Ready continuation nor a model turn may survive the reset.
    let saved = saved_delegation(&fixture);
    assert_eq!(saved.result.as_ref(), Some(&result));
    assert_eq!(saved.status, DelegationStatus::Failed);
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn codex_reviewer_mcp_startup_wait_reset_settles_original_generation() {
    reset_during_status_wait(true);
}

#[test]
fn codex_reviewer_mcp_finish_wait_reset_settles_original_generation() {
    reset_during_status_wait(false);
}

fn mark_reset_before_finish(fixture: &ReviewerFixture) {
    // Arrange the setters' marker only, not a public reset API execution.
    let mut inner = fixture.state.inner.lock().unwrap();
    let i = inner.find_session_index(&fixture.child).unwrap();
    let child = &mut inner.sessions[i];
    assert_eq!(child.session.status, SessionStatus::Active);
    assert_eq!(child.external_session_id.as_deref(), Some(REVIEW_THREAD));
    assert_eq!(child.active_turn_generation, 0);
    assert!(child.runtime.matches_runtime_token(&RuntimeToken::Codex(fixture.runtime.runtime_id.clone())));
    child.runtime_reset_required = true;
}

fn answer_exact_finish(fixture: &ReviewerFixture, page: Value) {
    let CodexRuntimeCommand::JsonRpcRequest { method, params, response_tx, .. } = next_command(fixture) else {
        panic!("a reset-marked finish must query before archive");
    };
    assert_eq!(method, "mcpServerStatus/list", "finish admission is the discriminating witness");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
    assert_eq!(params["detail"], "toolsAndAuthOnly");
    response_tx.send(Ok(page)).unwrap();
}

#[test]
fn codex_reviewer_mcp_completed_admits_preexisting_reset_for_finish_status() {
    let fixture = ReviewerFixture::new("reset-before-completed");
    mark_reset_before_finish(&fixture);
    fixture.complete();
    answer_exact_finish(&fixture, ready_page());
    let result = archive_and_result(&fixture);
    super::controls::assert_observation(&fixture, &result, "finish", CodexReviewerMcpOutcome::Ready);
    assert_eq!(result.summary, "child finished without a result packet");
    assert!(!result.reviewer_mcp_observations[0].reason.contains("runtime stop or reset fence"));
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Error);
    assert_eq!(child.session.preview, "child finished without a result packet");
    assert!(!child.session.messages.iter().any(|message| matches!(message,
        Message::Text { text, .. } if text.contains("Turn failed"))),
        "the clean finish is failed only by the missing-packet delegation refresh");
    assert!(child.deferred_stop_callbacks.is_empty());
}

#[test]
fn codex_reviewer_mcp_error_callbacks_admit_preexisting_reset_and_keep_original_detail() {
    for fatal_notification in [false, true] {
        let fixture = ReviewerFixture::new("reset-before-error-callback");
        mark_reset_before_finish(&fixture);
        let detail = "review execution fatally failed before reset-aware observation";
        if fatal_notification {
            super::regressions::read_notification(&fixture, json!({"method": "error", "params": {
                "threadId": REVIEW_THREAD, "turnId": REVIEW_TURN,
                "error": {"message": detail}, "willRetry": false
            }}));
        } else {
            fixture.complete_with(json!({"message": detail}));
        }
        answer_exact_finish(&fixture, json!({"data": [], "nextCursor": null}));
        let result = archive_and_result(&fixture);
        super::controls::assert_observation(&fixture, &result, "finish", CodexReviewerMcpOutcome::MissingServer);
        assert_eq!(result.summary, detail, "the original execution detail must win over the status page");
        assert!(!result.reviewer_mcp_observations[0].reason.contains("runtime stop or reset fence"));
        let inner = fixture.state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
        assert_eq!(child.session.status, SessionStatus::Error);
        assert!(child.deferred_stop_callbacks.is_empty());
        assert!(child.session.messages.iter().any(|message| matches!(message,
            Message::Text { text, .. } if text.contains(&format!("Turn failed: {detail}")))));
    }
}

#[test]
fn codex_reviewer_mcp_preexisting_reset_keeps_prior_submission_without_finish_query() {
    let fixture = ReviewerFixture::new("reset-before-submitted-completion");
    fixture.state.submit_delegation_review_result(&fixture.child, structured_review_request()).unwrap();
    let submitted = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()]
            .submitted_review_result.clone().unwrap()
    };
    mark_reset_before_finish(&fixture);
    fixture.complete();
    let result = archive_and_result(&fixture);
    assert_eq!(result, submitted, "the full immutable payload must survive the marker");
    assert_eq!(result.status, DelegationStatus::Completed);
    assert!(result.reviewer_mcp_observations.is_empty());
    let saved = saved_delegation(&fixture);
    assert_eq!(saved.result.as_ref(), Some(&result));
    assert!(saved.submitted_review_result.is_none());
    assert_eq!(saved.review_result_schema_version, Some(DELEGATION_REVIEW_RESULT_SCHEMA_VERSION));
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err(), "no status query or model work after direct archive");
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Idle);
    assert!(child.deferred_stop_callbacks.is_empty());
}

#[test]
fn codex_reviewer_mcp_preexisting_reset_refuses_initial_model_start() {
    let fixture = ReviewerFixture::new("reset-before-start-admission");
    mark_reset_before_finish(&fixture);
    assert!(fixture.start().is_empty(), "no status or model wire under a startup reset marker");
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.summary, "Reviewer MCP startup scope is unavailable; model work was not started.");
    assert!(result.reviewer_mcp_observations.is_empty());
    assert_eq!(saved_delegation(&fixture).result.as_ref(), Some(&result));
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err());
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(child.deferred_stop_callbacks.is_empty());
}

#[test]
fn codex_reviewer_mcp_public_stop_before_completion_keeps_owner_without_finish_query() {
    let fixture = ReviewerFixture::new("public-stop-before-completed");
    let (_, claim) = fixture.state.begin_requested_stop_session(&fixture.child, &StopSessionOptions::default()).unwrap();
    assert!(claim.is_some());
    fixture.complete();
    assert!(fixture.input_rx.try_recv().is_err(), "no query, archive or model work while UserStop owns the turn");
    assert!(fixture.pending.lock().unwrap().is_empty());
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Stopping);
    assert!(child.runtime_stop_is_owned_by(RuntimeStopOwnerKind::UserStop,
        &RuntimeToken::Codex(fixture.runtime.runtime_id.clone()), child.runtime_stop_generation));
    assert!(matches!(child.deferred_stop_callbacks.as_slice(),
        [DeferredStopCallback::TurnCompleted { active_turn_generation: 0 }]));
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(d.status, DelegationStatus::Running);
    assert!(d.result.is_none());
    assert!(d.attempt.reviewer_mcp_observations.is_empty());
}

#[test]
fn codex_reviewer_mcp_public_stop_before_initial_start_keeps_owner_without_callback() {
    let fixture = ReviewerFixture::new("public-stop-before-start-admission");
    let (_, claim) = fixture.state.begin_requested_stop_session(&fixture.child, &StopSessionOptions::default()).unwrap();
    assert!(claim.is_some());
    assert!(fixture.start().is_empty(), "the stopped startup must write neither status nor model work");
    assert!(fixture.input_rx.try_recv().is_err());
    assert!(fixture.pending.lock().unwrap().is_empty());
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Stopping);
    assert!(child.runtime_stop_is_owned_by(RuntimeStopOwnerKind::UserStop,
        &RuntimeToken::Codex(fixture.runtime.runtime_id.clone()), child.runtime_stop_generation));
    assert!(child.deferred_stop_callbacks.is_empty(), "the or-missing failure preserves this live Stop owner by refusing");
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(d.status, DelegationStatus::Running);
    assert!(d.result.is_none());
    assert!(d.attempt.reviewer_mcp_observations.is_empty());
}

#[test]
fn codex_reviewer_mcp_public_stop_during_finish_wait_preserves_owner_after_worker_exit() {
    let fixture = ReviewerFixture::new("public-stop-during-finish-wait");
    let before = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()]
            .attempt.reviewer_mcp_observations.clone()
    };
    // This direct finish worker is the wait-phase witness, not finish admission.
    // The separate pre-completion Stop control exercises the actual admission.
    let scope = codex_reviewer_mcp_scope(
        &fixture.state, &fixture.child, REVIEW_THREAD, &fixture.runtime.runtime_id,
    ).unwrap();
    let worker = worker_with_scope(&fixture, false, scope);
    let CodexRuntimeCommand::JsonRpcRequest { method, params, response_tx, .. } = next_command(&fixture) else {
        panic!("the actual finish worker must query before UserStop");
    };
    assert_eq!(method, "mcpServerStatus/list");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
    assert_eq!(params["detail"], "toolsAndAuthOnly");
    {
        let shared = fixture.runtime.sessions.lock().unwrap();
        let (_, claim) = fixture.state.begin_requested_stop_session(
            &fixture.child, &StopSessionOptions::default(),
        ).unwrap();
        assert!(claim.is_some());
        let inner = fixture.state.inner.lock().unwrap();
        // Ready may be delivered or the wait may already have refused the Stop.
        // Either way recording remains behind this shared->state gate, and the
        // actual worker join below (not this send) proves the refusal completed.
        let _ = response_tx.send(Ok(ready_page()));
        drop(inner);
        drop(shared);
    }
    join_worker(worker);
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err(), "no archive, status recheck or model work after worker exit");
    let inner = fixture.state.inner.lock().unwrap();
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Stopping);
    assert_eq!(child.active_turn_generation, 0);
    assert!(child.runtime_stop_is_owned_by(RuntimeStopOwnerKind::UserStop,
        &RuntimeToken::Codex(fixture.runtime.runtime_id.clone()), child.runtime_stop_generation));
    assert!(child.deferred_stop_callbacks.is_empty());
    let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(d.status, DelegationStatus::Running);
    assert!(d.result.is_none());
    assert_eq!(d.attempt.reviewer_mcp_observations, before);
}
