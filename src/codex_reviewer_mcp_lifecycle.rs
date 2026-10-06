// Owns bounded reviewer status workers and finish/start continuations.
// Waits run off the JSON-RPC reader/writer and revalidate exact ownership.
struct CodexReviewerMcpRequestCleanup(CodexPendingRequestMap, String);

fn settle_codex_reviewer_mcp(state: &AppState, scope: &CodexReviewerMcpScope, failure: Option<&str>) {
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        if !codex_reviewer_mcp_identity(&inner, scope)
            || !inner.sessions.iter().any(|r| r.session.id == scope.session_id
                && matches!(r.session.status, SessionStatus::Active | SessionStatus::Approval)) {
            return;
        }
        let index = inner.find_session_index(&scope.session_id).expect("current child");
        let record = inner.session_mut_by_index(index).expect("current child");
        if record.runtime_stop_in_progress {
            if failure.is_some() { record.deferred_stop_callbacks.retain(|c| !matches!(c,
                DeferredStopCallback::TurnCompleted { active_turn_generation } if *active_turn_generation == scope.generation)); }
            if record.deferred_stop_callbacks.iter().any(|c| match c {
                DeferredStopCallback::TurnFailed { active_turn_generation, .. } => *active_turn_generation == scope.generation,
                DeferredStopCallback::TurnCompleted { active_turn_generation } => failure.is_none() && *active_turn_generation == scope.generation,
                _ => false,
            }) { return; }
        }
    }
    #[cfg(test)]
    tests::run_settlement_gap_hook(&scope.gate);
    let token = RuntimeToken::Codex(scope.runtime_id.clone());
    let result = match failure {
        Some(detail) => state.fail_turn_if_runtime_and_generation_match(
            &scope.session_id, &token, scope.generation, detail),
        None => state.finish_turn_ok_if_runtime_matches_guarded(
            &scope.session_id, &token, Some(scope.generation)),
    };
    if let Err(error) = result {
        eprintln!("reviewer MCP terminal persistence failed: {error:#}");
    }
    // Publish the exact terminal child's parent result even when saving failed.
    // Do not consume waits here; publishing does not claim a durably saved result.
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    if codex_reviewer_mcp_identity(&inner, scope) && inner.sessions.iter().any(|r|
        r.session.id == scope.session_id && matches!(r.session.status, SessionStatus::Error | SessionStatus::Idle)) {
        let d = inner.find_delegation_index(&scope.delegation_id).expect("current delegation");
        refresh_delegation_from_child_locked(&mut inner, d);
        state.publish_state_locked(&inner);
    }
}

fn fail_codex_reviewer_mcp(state: &AppState, scope: &CodexReviewerMcpScope, detail: &str) {
    settle_codex_reviewer_mcp(state, scope, Some(detail));
}

impl Drop for CodexReviewerMcpRequestCleanup {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.0.lock() {
            pending.remove(&self.1);
        }
    }
}

fn wait_codex_reviewer_mcp_response(
    state: &AppState,
    sessions: &SharedCodexSessionMap,
    scope: &CodexReviewerMcpScope,
    response: mpsc::Receiver<std::result::Result<Value, CodexResponseError>>,
    deadline: std::time::Instant,
) -> std::result::Result<Value, CodexResponseError> {
    loop {
        if !codex_reviewer_mcp_current(&state.inner.lock().expect("state mutex poisoned"), scope) {
            return Err(CodexResponseError::Transport(
                "reviewer owner changed".to_owned(),
            ));
        }
        {
            let shared = sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let Some(s) = shared
                .get(&scope.session_id)
                .filter(|s| s.reviewer_mcp_gate.as_deref() == Some(&scope.gate))
            else {
                return Err(CodexResponseError::Transport(
                    "reviewer query superseded".to_owned(),
                ));
            };
            if s.reviewer_mcp_failure.is_some() {
                return Err(CodexResponseError::Transport(
                    "startup failure observed".to_owned(),
                ));
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(CodexResponseError::Timeout(
                "reviewer status budget elapsed".to_owned(),
            ));
        }
        match response.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(v) => return v,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CodexResponseError::Transport(
                    "reviewer status response unavailable".to_owned(),
                ));
            }
        }
    }
}

fn spawn_codex_reviewer_mcp_observation(
    state: AppState,
    sessions: SharedCodexSessionMap,
    scope: CodexReviewerMcpScope,
    initial: Option<(
        CodexPendingRequestMap,
        std::result::Result<PendingCodexJsonRpcRequest, CodexResponseError>,
    )>,
    start: Option<(
        CodexPromptCommand,
        Option<SharedCodexTurnStartedWatchdogConfig>,
    )>,
    finish_error: Option<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let _cleanup = initial.as_ref().and_then(|(pending, request)| request.as_ref().ok()
            .map(|request| CodexReviewerMcpRequestCleanup(pending.clone(), request.request_id.clone())));
        let query_started = scope.query_started;
        let (started, measurement) = {
            let sessions = sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let Some(s) = sessions.get(&scope.session_id).filter(|s| {
                s.thread_id.as_deref() == Some(&scope.thread_id)
                    && s.reviewer_mcp_gate.as_deref() == Some(&scope.gate)
            }) else {
                return;
            };
            match (start.is_some(), s.reviewer_mcp_setup_started) {
                (true, Some(setup)) => (
                    setup,
                    "thread setup initiated to observed readiness; not isolated bridge initialization",
                ),
                (true, None) => (
                    query_started,
                    "readiness check to observed status; thread setup boundary unavailable",
                ),
                (false, _) => (query_started, "finish status query to observed status"),
            }
        };
        let deadline = query_started + CODEX_REVIEWER_MCP_BUDGET;
        let mut initial = initial;
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut reason = None;
        let mut outcome = loop {
            if !codex_reviewer_mcp_current(
                &state.inner.lock().expect("state mutex poisoned"),
                &scope,
            ) {
                break CodexReviewerMcpOutcome::Unavailable;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break CodexReviewerMcpOutcome::Timeout;
            }
            let response = if let Some((pending, request)) = initial.take() {
                request.and_then(|request| {
                    let result = wait_codex_reviewer_mcp_response(
                        &state,
                        &sessions,
                        &scope,
                        request.response_rx,
                        deadline,
                    );
                    pending
                        .lock()
                        .expect("Codex pending requests mutex poisoned")
                        .remove(&request.request_id);
                    result
                })
            } else {
                let (tx, rx) = mpsc::channel();
                if scope
                    .input_tx
                    .send(CodexRuntimeCommand::JsonRpcRequest {
                        method: "mcpServerStatus/list".to_owned(),
                        params: codex_reviewer_mcp_params(&scope, cursor.as_deref()),
                        timeout: remaining,
                        response_tx: tx,
                    })
                    .is_err()
                {
                    break CodexReviewerMcpOutcome::Unavailable;
                }
                wait_codex_reviewer_mcp_response(&state, &sessions, &scope, rx, deadline)
            };
            let value = match response {
                Ok(v) => v,
                Err(CodexResponseError::Timeout(_)) => break CodexReviewerMcpOutcome::Timeout,
                Err(_) => {
                    let shared = sessions
                        .lock()
                        .expect("shared Codex session mutex poisoned");
                    break if shared.get(&scope.session_id).is_some_and(|s| {
                        s.reviewer_mcp_gate.as_deref() == Some(&scope.gate)
                            && s.reviewer_mcp_failure.is_some()
                    }) {
                        CodexReviewerMcpOutcome::Failed
                    } else {
                        CodexReviewerMcpOutcome::Unavailable
                    };
                }
            };
            let (outcome, page_reason, next) = match codex_reviewer_mcp_page(&value) {
                Ok(v) => v,
                Err(_) => break CodexReviewerMcpOutcome::Invalid,
            };
            if page_reason.is_some() {
                reason = page_reason;
            }
            if outcome == Some(CodexReviewerMcpOutcome::MissingServer) && next.is_some() {
                if seen.len() >= 50 || !seen.insert(next.clone()) {
                    break CodexReviewerMcpOutcome::Invalid;
                }
                cursor = next;
                continue;
            }
            if let Some(outcome) = outcome {
                break outcome;
            }
            cursor = None;
            seen.clear();
            // Only the worker waits. Bound starting rechecks without spinning or
            // consuming the reader/writer or a sibling thread's pending request.
            std::thread::sleep(remaining.min(Duration::from_millis(100)));
        };
        let shared = sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let Some(s) = shared.get(&scope.session_id).filter(|s| {
            s.thread_id.as_deref() == Some(&scope.thread_id)
                && s.reviewer_mcp_gate.as_deref() == Some(&scope.gate)
        }) else {
            return;
        };
        if s.reviewer_mcp_failure.is_some() {
            outcome = CodexReviewerMcpOutcome::Failed;
            reason = s.reviewer_mcp_failure.clone();
        }
        let observation = CodexReviewerMcpObservation {
            phase: if start.is_some() { "startup" } else { "finish" }.to_owned(),
            outcome: outcome.clone(),
            elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            budget_ms: CODEX_REVIEWER_MCP_BUDGET.as_millis() as u64,
            measurement: measurement.to_owned(),
            reason: reason.unwrap_or_else(|| codex_reviewer_mcp_reason(None)),
        };
        let saved = record_codex_reviewer_mcp_observation(&state, &scope, observation);
        drop(shared);
        match saved {
            Ok(false) => return,
            Err(error) => {
                eprintln!("reviewer MCP observation persistence failed: {error:#}");
                fail_codex_reviewer_mcp(&state, &scope,
                    "Failed to persist reviewer MCP observation; model work was not started or resumed.");
                return;
            }
            Ok(true) => {}
        }
        let interrupted = !codex_reviewer_mcp_current(&state.inner.lock().expect("state mutex poisoned"), &scope);
        if let Some((command, watchdog)) = start {
            if outcome == CodexReviewerMcpOutcome::Ready && !interrupted {
                let _ = scope.input_tx.send(CodexRuntimeCommand::ReviewerMcpReady {
                    scope: scope.clone(),
                    command,
                    watchdog,
                });
            } else {
                fail_codex_reviewer_mcp(
                    &state, &scope,
                    "Reviewer MCP bridge is unavailable; see the persisted typed startup observation.",
                );
            }
        } else if finish_error.is_some() || interrupted {
            fail_codex_reviewer_mcp(&state, &scope, finish_error.as_deref()
                .unwrap_or("Reviewer MCP status wait interrupted; see the typed observation."));
        } else {
            settle_codex_reviewer_mcp(&state, &scope, None);
        }
    })
}

fn defer_codex_reviewer_mcp_finish(
    state: &AppState,
    sessions: &SharedCodexSessionMap,
    session: &str,
    token: &RuntimeToken,
    thread: Option<&str>,
    gate: &mut Option<String>,
    error: Option<String>,
) -> bool {
    let (RuntimeToken::Codex(runtime), Some(thread)) = (token, thread) else {
        return false;
    };
    let Some(mut scope) = codex_reviewer_mcp_scope_with_stop(state, session, thread, runtime, false, true) else {
        return false;
    };
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        if inner
            .delegations
            .iter()
            .any(|d| d.id == scope.delegation_id && d.submitted_review_result.is_some())
        {
            return false;
        }
    }
    if gate.as_deref().is_some_and(|g| g.starts_with("finish:")) {
        return true;
    }
    scope.gate = format!("finish:{}", scope.gate);
    *gate = Some(scope.gate.clone());
    spawn_codex_reviewer_mcp_observation(state.clone(), sessions.clone(), scope, None, None, error);
    true
}
