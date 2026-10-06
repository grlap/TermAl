// Owns exact reviewer scopes, typed observations and sanitized status parsing.
// Reviewer readiness is thread-scoped; neither a profile catalog nor another
// thread's startup notification is evidence for this delegation.
const CODEX_REVIEWER_MCP_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum CodexReviewerMcpOutcome {
    Ready,
    Failed,
    MissingServer,
    MissingTool,
    Invalid,
    Unavailable,
    Timeout,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodexReviewerMcpObservation {
    phase: String,
    outcome: CodexReviewerMcpOutcome,
    elapsed_ms: u64,
    budget_ms: u64,
    measurement: String,
    reason: String,
}

#[derive(Clone)]
struct CodexReviewerMcpScope {
    session_id: String,
    thread_id: String,
    runtime_id: String,
    delegation_id: String,
    attempt: u32,
    generation: u64,
    stop_generation: u64,
    allow_reset: bool,
    gate: String,
    query_started: std::time::Instant,
    input_tx: Sender<CodexRuntimeCommand>,
}

fn codex_reviewer_mcp_scope(
    state: &AppState,
    session: &str,
    thread: &str,
    runtime: &str,
) -> Option<CodexReviewerMcpScope> {
    codex_reviewer_mcp_scope_with_stop(state, session, thread, runtime, false, false)
}

fn codex_reviewer_mcp_scope_with_stop(
    state: &AppState, session: &str, thread: &str, runtime: &str, allow_stop: bool, allow_reset: bool,
) -> Option<CodexReviewerMcpScope> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(session)?];
    let delegation = inner.delegations.iter().find(|d| {
        d.child_session_id == session
            && d.mode == DelegationMode::Reviewer
            && d.status == DelegationStatus::Running
    })?;
    let SessionRuntime::Codex(handle) = &record.runtime else {
        return None;
    };
    if handle.runtime_id != runtime
        || delegation
            .attempt
            .reviewer_mcp_observations
            .iter()
            .any(|o| o.phase == "startup" && o.outcome != CodexReviewerMcpOutcome::Ready)
        || record.external_session_id.as_deref() != Some(thread)
        || (!allow_stop && record.runtime_stop_in_progress)
        || (!allow_reset && record.runtime_reset_required)
        || !matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval
        )
    {
        return None;
    }
    Some(CodexReviewerMcpScope {
        session_id: session.to_owned(),
        thread_id: thread.to_owned(),
        runtime_id: runtime.to_owned(),
        delegation_id: delegation.id.clone(),
        attempt: delegation.review_result_submission_attempt,
        generation: record.active_turn_generation,
        stop_generation: record.runtime_stop_generation,
        allow_reset,
        gate: Uuid::new_v4().to_string(),
        query_started: std::time::Instant::now(),
        input_tx: handle.input_tx.clone(),
    })
}

fn codex_reviewer_mcp_identity(inner: &StateInner, scope: &CodexReviewerMcpScope) -> bool {
    let Some(i) = inner.find_session_index(&scope.session_id) else {
        return false;
    };
    let r = &inner.sessions[i];
    r.runtime
        .matches_runtime_token(&RuntimeToken::Codex(scope.runtime_id.clone()))
        && r.external_session_id.as_deref() == Some(&scope.thread_id)
        && r.active_turn_generation == scope.generation
        && inner.delegations.iter().any(|d| {
            d.id == scope.delegation_id
                && d.mode == DelegationMode::Reviewer
                && d.child_session_id == scope.session_id
                && d.status == DelegationStatus::Running
                && d.review_result_submission_attempt == scope.attempt
        })
}

fn codex_reviewer_mcp_current(inner: &StateInner, scope: &CodexReviewerMcpScope) -> bool {
    codex_reviewer_mcp_identity(inner, scope) && inner.sessions.iter().any(|r| {
        r.session.id == scope.session_id && r.runtime_stop_generation == scope.stop_generation
            && !r.runtime_stop_in_progress
            && (scope.allow_reset || !r.runtime_reset_required)
            && matches!(r.session.status, SessionStatus::Active | SessionStatus::Approval)
    })
}

fn codex_reviewer_mcp_params(scope: &CodexReviewerMcpScope, cursor: Option<&str>) -> Value {
    let mut params = json!({"threadId": scope.thread_id, "serverName": TERMAL_DELEGATION_MCP_SERVER_NAME,
        "detail": "toolsAndAuthOnly", "limit": 100});
    if let Some(cursor) = cursor {
        params["cursor"] = json!(cursor);
    }
    params
}

fn codex_reviewer_mcp_has_environment_secret(
    raw: &str,
    values: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> bool {
    values.filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .any(|(k, v)| v.len() >= 4 && raw.contains(&v)
            && ["TOKEN", "KEY", "SECRET", "PASSWORD", "CREDENTIAL"].iter()
                .any(|s| k.to_uppercase().contains(s)))
}

fn codex_reviewer_mcp_reason(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return "No startup reason supplied.".to_owned();
    };
    // Never copy config, assignments, URLs or credential-bearing diagnostic
    // lines. Known environment values are removed even from innocuous prose.
    let safe = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect::<String>();
    if codex_reviewer_mcp_has_environment_secret(raw, std::env::vars_os()) {
        return "Startup reason supplied; sensitive detail withheld.".to_owned();
    }
    let lower = safe.to_lowercase();
    if safe.contains(['=', '{', '}', '\\'])
        || safe.split_whitespace().any(|word| word.len() > 32)
        || lower.contains("http")
        || [
            "token",
            "password",
            "secret",
            "authorization",
            "environment",
            "config",
            "sk-",
            "routing",
        ]
        .iter()
        .any(|s| lower.contains(s))
    {
        return "Startup reason supplied; sensitive detail withheld.".to_owned();
    }
    safe
}

// None means genuinely starting, not missing/failed. Pagination is bounded and
// exact-server filtering still validates the response, rather than trusting it.
fn codex_reviewer_mcp_page(
    value: &Value,
) -> Result<(
    Option<CodexReviewerMcpOutcome>,
    Option<String>,
    Option<String>,
)> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("invalid status page"))?;
    let cursor = match value.get("nextCursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => return Err(anyhow!("invalid status cursor")),
    };
    let entries = data
        .iter()
        .filter(|e| {
            e.get("name").and_then(Value::as_str) == Some(TERMAL_DELEGATION_MCP_SERVER_NAME)
        })
        .collect::<Vec<_>>();
    if entries.len() > 1 {
        return Err(anyhow!("duplicate server status"));
    }
    let Some(entry) = entries.first() else {
        return Ok((Some(CodexReviewerMcpOutcome::MissingServer), None, cursor));
    };
    let tools = entry
        .get("tools")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("invalid tool catalog"))?;
    let reason = entry
        .get("toolsError")
        .and_then(Value::as_str)
        .map(|raw| codex_reviewer_mcp_reason(Some(raw)));
    let outcome = match entry.get("runtimeStatus").and_then(Value::as_str) {
        Some("starting" | "notStarted") => None,
        Some("failed" | "cancelled" | "disabled" | "authenticationRequired") => {
            Some(CodexReviewerMcpOutcome::Failed)
        }
        Some("connected") => Some(
            if tools.values().any(|tool| {
                tool.get("name").and_then(Value::as_str)
                    == Some(TERMAL_SUBMIT_REVIEW_RESULT_TOOL_NAME)
            }) {
                CodexReviewerMcpOutcome::Ready
            } else {
                CodexReviewerMcpOutcome::MissingTool
            },
        ),
        _ => Some(CodexReviewerMcpOutcome::Invalid),
    };
    Ok((outcome, reason, cursor))
}

fn record_codex_reviewer_mcp_observation(
    state: &AppState,
    scope: &CodexReviewerMcpScope,
    mut observation: CodexReviewerMcpObservation,
) -> Result<bool> {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    if !codex_reviewer_mcp_identity(&inner, scope) {
        return Ok(false);
    }
    let i = inner.find_session_index(&scope.session_id).expect("current child");
    let r = &inner.sessions[i];
    if !matches!(r.session.status, SessionStatus::Active | SessionStatus::Approval)
        || (r.runtime_stop_in_progress && !r.runtime_stop_is_owned_by(
            RuntimeStopOwnerKind::EngramMcpRevocation,
            &RuntimeToken::Codex(scope.runtime_id.clone()), r.runtime_stop_generation)) {
        return Ok(false);
    }
    let interrupted = r.runtime_stop_generation != scope.stop_generation || r.runtime_stop_in_progress || (!scope.allow_reset && r.runtime_reset_required);
    let deferred = r.runtime_stop_in_progress;
    let d = inner
        .find_delegation_index(&scope.delegation_id)
        .expect("current delegation");
    // A submission received while the query ran wins, unchanged.
    if inner.delegations[d].submitted_review_result.is_some() {
        return Ok(true);
    }
    if interrupted {
        observation.outcome = CodexReviewerMcpOutcome::Unavailable;
        observation.reason = "Reviewer status wait interrupted by a runtime stop or reset fence.".to_owned();
    }
    let observations = &mut inner.delegations[d].attempt.reviewer_mcp_observations;
    observations.retain(|o| o.phase != observation.phase);
    observations.push(observation.clone());
    inner.mark_delegation_mutated(d);
    let id = inner.next_message_id();
    let i = inner
        .find_session_index(&scope.session_id)
        .expect("current child");
    let record = inner.session_mut_by_index(i).expect("current child");
    push_message_on_record(
        record,
        Message::Text {
            id,
            timestamp: stamp_now(),
            author: Author::System,
            text: format!(
                "Reviewer MCP observation: {}",
                serde_json::to_string(&observation)?
            ),
            attachments: Vec::new(),
            expanded_text: None,
            source: None,
        },
    );
    if deferred {
        record.deferred_stop_callbacks.retain(|callback| !matches!(callback,
            DeferredStopCallback::TurnCompleted { active_turn_generation } if *active_turn_generation == scope.generation));
        if !record.deferred_stop_callbacks.iter().any(|callback| matches!(callback,
            DeferredStopCallback::TurnFailed { active_turn_generation, .. } if *active_turn_generation == scope.generation)) {
            record.deferred_stop_callbacks.push(DeferredStopCallback::TurnFailed {
                active_turn_generation: scope.generation,
                message: "Reviewer MCP observation interrupted; see the typed observation.".to_owned(),
            });
        }
    }
    state.commit_locked(&mut inner)?;
    Ok(!deferred)
}
