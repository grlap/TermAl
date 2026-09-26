// TermAl's Kimi auto-approve policy (`kimiApprovalMode: auto-approve`).
//
// Owns: which Kimi permission requests TermAl may answer itself for a session
// whose policy is auto-approve, and sending that answer under the same lock as
// Stop and runtime replacement. Also records the mode Kimi reports, for
// display, and validates the app default effort (`defaultKimiEffort`).
//
// Does not own: the read-only gate for delegation children
// (kimi_read_only.rs), which always runs first and always wins; Kimi's own
// mode and its pre-prompt ACK (kimi.rs, `configure_kimi_mode`); or the manual
// approval card (acp.rs, `handle_acp_request`).
//
// New module, not split from another file. Contract:
// docs/features/kimi-cli-integration.md, "Approvals and mode". Evidence
// (Kimi Code 2.0.2 captures): AskUserQuestion arrives as a permission request
// whose answers are `allow_once` options, and ExitPlanMode's plan approval is
// `allow_once` too, so a policy that picked the first `allow_once` would
// answer the user's question or approve a plan.

/// Longest Kimi effort value the app default accepts. Kimi advertises short
/// tokens (`low`, `high`, `max`); the value is checked against the model's
/// advertised list only when a session prompts.
const MAX_DEFAULT_KIMI_EFFORT_CHARS: usize = 64;

/// The app-wide default Kimi effort: `auto` (leave the CLI's choice) or one
/// effort token. Empty text means `auto`. Pure validation: callers run it
/// before they change any preference, so a rejected request changes nothing.
fn normalize_default_kimi_effort(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return Ok(default_kimi_effort_preference());
    }
    if trimmed.chars().count() > MAX_DEFAULT_KIMI_EFFORT_CHARS
        || trimmed
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(ApiError::bad_request(
            "defaultKimiEffort must be `auto` or one effort value without spaces",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Tools Kimi Code 2.0.2 asks permission for that TermAl may auto-approve.
/// Any other title (AskUserQuestion, ExitPlanMode, a future tool) stays a
/// manual card: an allowlist fails safe where a denylist would not.
fn kimi_auto_approvable_title(title: &str) -> bool {
    matches!(title, "Bash" | "Write" | "Edit" | "CronCreate") || kimi_is_mcp_tool_title(title)
}

/// `mcp__<server>__<tool>`, the form Kimi names MCP tools in.
fn kimi_is_mcp_tool_title(title: &str) -> bool {
    let Some(rest) = title.strip_prefix("mcp__") else {
        return false;
    };
    let Some((server, tool)) = rest.split_once("__") else {
        return false;
    };
    let word = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    };
    word(server) && word(tool)
}

/// The one option a request offers whose kind is exactly `allow_once`. Zero
/// or two or more mean the request is a choice (a question's answers) or
/// unknown, and it stays manual. `allow_always` is never selected.
fn kimi_single_allow_once_option(options: &[Value]) -> Option<String> {
    let mut allow_once = options.iter().filter(|option| {
        option.get("kind").and_then(Value::as_str) == Some("allow_once")
    });
    let option = allow_once.next()?;
    if allow_once.next().is_some() {
        return None;
    }
    option
        .get("optionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// Whether TermAl may answer a Kimi request itself right now. Mirrors
/// `opencode_auto_approval_allowed_locked`: the session is Active (so a
/// pending manual card, which moves it to Approval, suspends auto-approve),
/// no Stop is in progress, the policy is auto-approve, and it is not a
/// read-only delegation child.
fn kimi_auto_approval_allowed_locked(inner: &StateInner, session_id: &str) -> bool {
    inner.find_session_index(session_id).is_some_and(|index| {
        let record = &inner.sessions[index];
        record.session.agent == Agent::Kimi
            && record.session.status == SessionStatus::Active
            && !record.runtime_stop_in_progress
            && record.session.kimi_approval_mode.unwrap_or_default()
                == KimiApprovalMode::AutoApprove
            && read_only_session_delegation_block_locked(inner, Some(session_id)).is_none()
    })
}

impl AppState {
    /// Answers a Kimi permission request under the session's auto-approve
    /// policy and returns `true`, or returns `false` to leave it a manual
    /// card. Runs after the read-only gate, in the fenced branch of
    /// `handle_acp_message`: the decision and the send happen under the lock
    /// that also guards Stop, and a stale or stopping runtime of an
    /// auto-approve session is answered `cancelled`, never approved.
    fn answer_kimi_auto_approval(
        &self,
        message: &Value,
        session_id: &str,
        runtime_token: &RuntimeToken,
        input_tx: &Sender<AcpRuntimeCommand>,
    ) -> Result<bool> {
        let params = message.get("params").unwrap_or(&Value::Null);
        let title = params
            .pointer("/toolCall/title")
            .and_then(Value::as_str)
            .unwrap_or("");
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let inner = self.inner.lock().expect("state mutex poisoned");
        let Some(record) = inner
            .find_session_index(session_id)
            .map(|index| &inner.sessions[index])
        else {
            return Ok(false);
        };
        if record.session.kimi_approval_mode.unwrap_or_default() != KimiApprovalMode::AutoApprove {
            return Ok(false);
        }
        let current = record.runtime.matches_runtime_token(runtime_token)
            && !record.runtime_stop_in_progress
            && !matches!(
                record.session.status,
                SessionStatus::Stopping | SessionStatus::Idle
            );
        let outcome = if !current {
            json!({"outcome": "cancelled"})
        } else if let Some(option_id) = kimi_auto_approvable_title(title)
            .then(|| kimi_single_allow_once_option(&options))
            .flatten()
            .filter(|_| kimi_auto_approval_allowed_locked(&inner, session_id))
        {
            json!({"outcome": "selected", "optionId": option_id})
        } else {
            return Ok(false);
        };
        input_tx
            .send(AcpRuntimeCommand::JsonRpcMessage(
                json_rpc_result_response_message(
                    message.get("id").cloned().unwrap_or(Value::Null),
                    json!({"outcome": outcome}),
                ),
            ))
            .map_err(|err| anyhow!("failed delivering Kimi permission response: {err}"))?;
        Ok(true)
    }

    /// Records the mode Kimi reports, for display only. It never changes the
    /// session's `kimiMode`, and it never decides an approval: the effective
    /// mode comes only from the pre-prompt ACK.
    fn record_kimi_current_mode(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
        mode: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let record = &inner.sessions[index];
        if record.session.agent != Agent::Kimi
            || !record.runtime.matches_runtime_token(runtime_token)
            || record.session.kimi_current_mode.as_deref() == Some(mode)
        {
            return Ok(());
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.session.kimi_current_mode = Some(mode.to_owned());
        self.commit_locked(&mut inner)?;
        Ok(())
    }
}
