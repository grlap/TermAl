// Owns follow-up and terminal-route cuts missed by the initial local witnesses.
use super::controls::{
    archive_and_result, assert_observation, drive_ready, next_command, ready_page, respond_start,
};
use super::*;

pub(super) fn read_notification(fixture: &ReviewerFixture, notification: Value) {
    let state = fixture.state.clone();
    let runtime = fixture.runtime.clone();
    let pending = fixture.pending.clone();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = handle_shared_codex_app_server_message(
            &notification,
            &state,
            &runtime.runtime_id,
            &pending,
            &runtime.sessions,
            &runtime.thread_sessions,
            &runtime.input_tx,
        );
        let _ = done_tx.send(result);
    });
    await_event(
        &done_rx,
        "the real reader must return without waiting for its own response",
    )
    .unwrap();
}

fn failed_page() -> Value {
    json!({"data": [{"name": TERMAL_DELEGATION_MCP_SERVER_NAME,
        "runtimeStatus": "failed", "tools": {},
        "toolsError": "fixture bridge startup failed"}], "nextCursor": null})
}

#[test]
fn codex_reviewer_mcp_regression_followup_rearm_discards_previous_observation() {
    let fixture = ReviewerFixture::new("followup-rearm-observation");
    let written = fixture.start();
    respond_start(&fixture, &written, failed_page());
    let previous = archive_and_result(&fixture);
    assert_observation(
        &fixture,
        &previous,
        "startup",
        CodexReviewerMcpOutcome::Failed,
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&fixture.delegation).unwrap();
        let old_attempt = inner.delegations[index].review_result_submission_attempt;
        // This is the production follow-up owner called by committed follow-ups.
        // Runtime attachment below is arranged, not a public follow-up API witness.
        rearm_terminal_delegation_for_followup_locked(&mut inner, index).unwrap();
        assert_eq!(
            inner.delegations[index].review_result_submission_attempt,
            old_attempt + 1
        );
        assert!(
            inner.delegations[index].attempt.reviewer_mcp_observations.is_empty(),
            "the new attempt must not inherit the failed startup gate or finish diagnosis"
        );
        let index = inner.find_session_index(&fixture.child).unwrap();
        let child = &mut inner.sessions[index];
        child.session.status = SessionStatus::Active;
        child.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: fixture.runtime.runtime_id.clone(),
            input_tx: fixture.runtime.input_tx.clone(),
            process: fixture.process.clone(),
            shared_session: Some(SharedCodexSessionHandle {
                runtime: fixture.runtime.clone(),
                session_id: fixture.child.clone(),
            }),
        });
        fixture.state.commit_locked(&mut inner).unwrap();
    }
    fixture.runtime.sessions.lock().unwrap().insert(
        fixture.child.clone(),
        SharedCodexSessionState {
            thread_id: Some(REVIEW_THREAD.to_owned()),
            active_turn_generation: Some(0),
            ..SharedCodexSessionState::default()
        },
    );
    fixture.runtime.thread_sessions.lock().unwrap().insert(
        REVIEW_THREAD.to_owned(),
        fixture.child.clone(),
    );
    let written = fixture.start();
    assert!(!written.is_empty(), "a rearmed reviewer needs a fresh readiness query");
    respond_start(&fixture, &written, ready_page());
    let wire = drive_ready(&fixture, next_command(&fixture));
    let request: Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(request["method"], "turn/start");
    let inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_delegation_index(&fixture.delegation).unwrap();
    let observations = &inner.delegations[index].attempt.reviewer_mcp_observations;
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, CodexReviewerMcpOutcome::Ready);
}

#[test]
fn codex_reviewer_mcp_regression_queued_ready_yields_to_observed_failure() {
    let fixture = ReviewerFixture::new("queued-ready-then-failure");
    let written = fixture.start();
    respond_start(&fixture, &written, ready_page());
    // Taking the command fixes the cut AFTER the worker published Ready.
    let continuation = next_command(&fixture);
    assert!(matches!(&continuation, CodexRuntimeCommand::ReviewerMcpReady { .. }));
    read_notification(&fixture, json!({
        "method": "mcpServer/startupStatus/updated", "params": {
            "threadId": REVIEW_THREAD, "name": TERMAL_DELEGATION_MCP_SERVER_NAME,
            "status": "failed", "error": "failure after ready publication"
        }
    }));
    assert_eq!(
        fixture.runtime.sessions.lock().unwrap().get(&fixture.child).unwrap()
            .reviewer_mcp_failure.as_deref(),
        Some("failure after ready publication")
    );
    let wire = drive_ready(&fixture, continuation);
    assert!(wire.is_empty(), "an observed failure must prevent the queued turn/start");
    let result = archive_and_result(&fixture);
    assert_eq!(result.status, DelegationStatus::Failed);
    let latest = result.reviewer_mcp_observations.last().unwrap();
    assert_eq!(latest.phase, "startup");
    assert_eq!(latest.outcome, CodexReviewerMcpOutcome::Failed);
    assert_eq!(latest.reason, "failure after ready publication");
    let saved = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    let delegation = saved.delegations.iter().find(|d| d.id == fixture.delegation).unwrap();
    assert_eq!(delegation.result.as_ref(), Some(&result));
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err());
}

#[test]
fn codex_reviewer_mcp_regression_fatal_reader_error_observes_before_archive() {
    let fixture = ReviewerFixture::new("fatal-reader-error");
    read_notification(&fixture, json!({"method": "error", "params": {
        "threadId": REVIEW_THREAD, "turnId": REVIEW_TURN,
        "error": {"message": "review execution fatally failed"}, "willRetry": false
    }}));
    let CodexRuntimeCommand::JsonRpcRequest { method, params, response_tx, .. } =
        next_command(&fixture)
    else {
        panic!("fatal reviewer error requires a finish observation before archive");
    };
    assert_eq!(method, "mcpServerStatus/list", "fatal errors must use the finish owner");
    assert_eq!(params["threadId"], REVIEW_THREAD);
    assert_eq!(params["serverName"], TERMAL_DELEGATION_MCP_SERVER_NAME);
    response_tx.send(Ok(ready_page())).unwrap();
    let result = archive_and_result(&fixture);
    assert_observation(&fixture, &result, "finish", CodexReviewerMcpOutcome::Ready);
    assert!(result.summary.contains("review execution fatally failed"));
}

#[test]
fn codex_reviewer_mcp_authentication_required_is_a_failed_bridge() {
    let fixture = ReviewerFixture::new("schema-authentication-required");
    let written = fixture.start();
    let mut page = failed_page();
    page["data"][0]["runtimeStatus"] = json!("authenticationRequired");
    respond_start(&fixture, &written, page);
    let result = archive_and_result(&fixture);
    assert_observation(&fixture, &result, "startup", CodexReviewerMcpOutcome::Failed);
}

#[test]
fn codex_reviewer_mcp_initial_request_is_removed_on_every_early_worker_exit() {
    for cause in ["superseded", "stopped", "expired"] {
        let fixture = ReviewerFixture::new(cause);
        let mut scope = codex_reviewer_mcp_scope(
            &fixture.state, &fixture.child, REVIEW_THREAD, &fixture.runtime.runtime_id
        ).unwrap();
        if cause == "expired" {
            // Inject an already elapsed budget, not a real sixty-second wait.
            scope.query_started -= CODEX_REVIEWER_MCP_BUDGET + Duration::from_secs(1);
        }
        fixture.runtime.sessions.lock().unwrap().get_mut(&fixture.child).unwrap()
            .reviewer_mcp_gate = Some(if cause == "superseded" {
                "replacement-query".to_owned()
            } else { scope.gate.clone() });
        let mut wire = Vec::new();
        let request = start_codex_json_rpc_request(&mut wire, &fixture.pending,
            "mcpServerStatus/list", codex_reviewer_mcp_params(&scope, None));
        assert!(request.is_ok());
        assert_eq!(fixture.pending.lock().unwrap().len(), 1);
        if cause == "stopped" {
            // Actual public Stop ownership, arranged before worker entry.
            let (_, claim) = fixture.state.begin_requested_stop_session(
                &fixture.child, &StopSessionOptions::default()
            ).unwrap();
            assert!(claim.is_some());
        }
        let worker = spawn_codex_reviewer_mcp_observation(
            fixture.state.clone(), fixture.runtime.sessions.clone(), scope,
            Some((fixture.pending.clone(), request)),
            Some((CodexPromptCommand {
                active_turn_generation: 0, approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(), cwd: fixture.state.default_workdir.clone(),
                model: "gpt-5.4".to_owned(), prompt: "Review local source.".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium, service_tier: None,
                resume_thread_id: None, sandbox_mode: CodexSandboxMode::ReadOnly,
            }, None)), None,
        );
        // Join the actual worker, awaiting its finish instead of a polling
        // snapshot that could precede its cleanup destructor.
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || { let _ = done_tx.send(worker.join().is_ok()); });
        assert!(await_event(&done_rx, "early-exit worker must finish"));
        assert!(fixture.pending.lock().unwrap().is_empty(),
            "the registered initial request must not survive {cause} worker exit");
        if cause == "expired" {
            let result = archive_and_result(&fixture);
            assert_observation(&fixture, &result, "startup", CodexReviewerMcpOutcome::Timeout);
        } else {
            assert!(fixture.input_rx.try_recv().is_err(), "no model work for a stale owner");
        }
    }
}

async fn observation_persistence_failure_finishes(phase: &str) {
    let mut fixture = ReviewerFixture::new("observation-persistence-failure");
    let saved_path = fixture.state.persistence_path.clone();
    let mut publications = fixture.state.subscribe_events();
    // The fixture's persistence channel is disconnected. Its real synchronous
    // fallback cannot open an existing directory as SQLite; no live store changes.
    fixture.state.persistence_path = Arc::new(
        fixture.state.test_temp_root.as_ref().unwrap().path().to_path_buf()
    );
    if phase == "startup" {
        let written = fixture.start();
        respond_start(&fixture, &written, ready_page());
    } else {
        fixture.complete();
        let CodexRuntimeCommand::JsonRpcRequest { method, response_tx, .. } =
            next_command(&fixture)
        else {
            panic!("finish observation request");
        };
        assert_eq!(method, "mcpServerStatus/list");
        response_tx.send(Ok(ready_page())).unwrap();
    }
    // Block on the existing terminal-state publication channel, not sleeps or
    // a busy spin. The only bound is the fixtures' liveness guard
    // (`await_event`): it limits a broken worker, not a slow host.
    let publication =
        tokio::time::timeout(crate::TEST_PHASE_DEADLOCK_GUARD, publications.recv()).await;
    let inner = fixture.state.inner.lock().unwrap();
    let delegation = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
    assert_eq!(delegation.attempt.reviewer_mcp_observations.len(), 1,
        "the injected error must reach the observation commit, not an earlier refusal");
    assert_eq!(delegation.attempt.reviewer_mcp_observations[0].phase, phase);
    let child = &inner.sessions[inner.find_session_index(&fixture.child).unwrap()];
    assert_eq!(child.session.status, SessionStatus::Error,
        "a failed observation commit must terminalize instead of silently leaving Active");
    assert!(serde_json::to_string(&child.session.messages).unwrap().to_lowercase()
        .contains("persist"), "the in-memory failure must identify persistence");
    drop(inner);
    publication
        .expect("terminal failure must be published even when storage is unavailable")
        .expect("the state channel must remain connected");
    let deadline = tokio::time::Instant::now() + crate::TEST_PHASE_DEADLOCK_GUARD;
    loop {
        {
            let inner = fixture.state.inner.lock().unwrap();
            let d = &inner.delegations[inner.find_delegation_index(&fixture.delegation).unwrap()];
            if d.status == DelegationStatus::Failed { break; }
        }
        // The generic Error publication can precede the worker's guarded
        // delegation refresh. Await its publication, not a timing-based sleep.
        tokio::time::timeout_at(deadline, publications.recv())
            .await.expect("the parent must also observe a terminal delegation")
            .expect("state publication remains connected");
    }
    let result = {
        let inner = fixture.state.inner.lock().unwrap();
        let delegation = &inner.delegations[
            inner.find_delegation_index(&fixture.delegation).unwrap()
        ];
        delegation.result.clone().expect("the failed worker must publish its in-memory result")
    };
    assert_eq!(result.status, DelegationStatus::Failed);
    assert_eq!(result.reviewer_mcp_observations.len(), 1);
    // The failure primitive already attempted the recovery diagnostic save.
    // Public lookup now attempts terminal detach/result refresh; unavailable
    // storage must still produce a persistence error, not a saved result.
    let lookup_error = fixture.state.get_delegation_result(&fixture.parent, &fixture.delegation)
        .err().expect("public recovery cannot save through the unavailable persistence path");
    assert_eq!(lookup_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(lookup_error.message.contains("failed to persist delegation result refresh"));
    assert!(fixture.pending.lock().unwrap().is_empty());
    assert!(fixture.input_rx.try_recv().is_err(), "no model work after failed persistence");
    // load_state runs boot recovery and rewrites Active to Error in memory.
    // Inspect only this fixture's stored row, without recovery or SQLite writes.
    let connection = rusqlite::Connection::open_with_flags(
        saved_path.as_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).unwrap();
    let encoded: String = connection.query_row(
        "SELECT value_json FROM sessions WHERE id = ?1",
        rusqlite::params![fixture.child], |row| row.get(0),
    ).unwrap();
    let child: PersistedSessionRecord = serde_json::from_str(&encoded).unwrap();
    assert_eq!(child.session.status, SessionStatus::Active,
        "the injected failed commit must not be reported as a durable terminal save");
}

#[test]
#[cfg(any(windows, unix))]
fn codex_reviewer_mcp_environment_filter_skips_nonunicode_without_global_mutation() {
    #[cfg(windows)]
    let invalid = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800])
    };
    #[cfg(unix)]
    let invalid = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    };
    let values = vec![(invalid.clone(), "visible-value".into()),
        ("TOKEN".into(), invalid), ("TOKEN".into(), "fixture-private-value".into())];
    assert!(!codex_reviewer_mcp_has_environment_secret("ordinary startup failure", values.clone().into_iter()));
    assert!(codex_reviewer_mcp_has_environment_secret("fixture-private-value", values.into_iter()),
        "a valid secret after invalid entries must still be withheld");
}

#[tokio::test]
async fn codex_reviewer_mcp_regression_startup_observation_persistence_failure_finishes() {
    observation_persistence_failure_finishes("startup").await;
}

#[tokio::test]
async fn codex_reviewer_mcp_regression_finish_observation_persistence_failure_finishes() {
    observation_persistence_failure_finishes("finish").await;
}
