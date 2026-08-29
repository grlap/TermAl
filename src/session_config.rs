// Per-session settings overrides and on-demand model-option refresh.
//
// Settings live in three tiers that compose at session-create time and
// diverge after: global app settings (the default model, approval mode,
// and effort per agent), per-project defaults (inherited into freshly
// created sessions), and per-session overrides (this file). See
// `src/tests/session_settings.rs` for the tier composition pins.
//
// Live-update vs restart-required semantics vary by agent. Codex hot-
// swaps the model on the next `thread/resume` and applies effort, Fast mode,
// approval_policy, and sandbox_mode to the next turn without a restart.
// Claude has no hot reconfig path: model + effort changes flip
// `runtime_reset_required = true` so the next `send_message` in
// `state.rs` re-spawns the CLI — except that a live Claude runtime
// accepts an in-process `SetModel` command when the CLI understands the
// new model arg, which lets the model change without a restart. Cursor
// and Gemini are ACP-hosted: `cursor_mode` and the selected model
// propagate to the live session via `session/set_config_option`
// JSON-RPC messages (see `src/acp.rs::handle_acp_session_config_refresh`
// for the writer side); Gemini approval-mode changes require a restart.
//
// Codex reasoning-effort normalization: changing the model can
// invalidate the current effort. `normalized_codex_reasoning_effort` in
// `src/runtime.rs` inspects the new model's
// `supported_reasoning_efforts` and either preserves, reduces, or (when
// the request set effort directly) returns a "model does not support
// ... reasoning effort; choose ..." error.
//
// `refresh_session_model_options` takes three distinct handshake paths:
// Codex paginated `model/list` JSON-RPC (see
// `src/codex_rpc.rs::fire_codex_model_list_page`); ACP agents
// (Cursor, Gemini, OpenCode) re-trigger the session setup that emits
// model options on first session creation via
// `AcpRuntimeCommand::RefreshSessionConfig`; Claude CLI re-spawns and
// parses the initialize NDJSON response through `claude_model_options`
// in `src/runtime.rs`.
//
// Cross-refs: `src/turn_lifecycle.rs` clears the flag on runtime stop;
// `src/wire.rs::UpdateSessionSettingsRequest` and `SessionModelOption`
// pin the wire shape; `src/tests/session_settings.rs` and
// `src/tests/codex_threads.rs` cover the behaviour.

impl AppState {
    /// Applies a user-initiated settings change to a live session.
    ///
    /// The per-agent match validates that only fields that agent
    /// supports are present, then mutates the record:
    /// - Codex: updates `model`, `codex_sandbox_mode`,
    ///   `codex_approval_policy`, `codex_reasoning_effort`, and catalog-gated
    ///   Fast mode in place;
    ///   model hot-swaps on the next turn and effort is re-validated
    ///   against the target model's `supported_reasoning_efforts`.
    /// - Claude: sets `runtime_reset_required` on effort change; model
    ///   changes queue a `SetModel` command on the live runtime when
    ///   the CLI supports the model arg, otherwise also flip the reset
    ///   flag so the next `send_message` re-spawns.
    /// - Cursor (ACP): model and `cursor_mode` propagate live via
    ///   `session/set_config_option` JSON-RPC messages queued for the
    ///   ACP writer.
    /// - Gemini (ACP): model updates in place; approval-mode changes
    ///   flip `runtime_reset_required` for the next send.
    /// Remote-hosted sessions proxy the entire call unchanged.
    fn update_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_session_settings(session_id, request);
        }
        self.ensure_read_only_delegation_allows_session_write_action(
            Some(session_id),
            "session settings updates",
        )?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let mut claude_model_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut claude_permission_mode_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut acp_config_updates: Vec<(AcpRuntimeHandle, Value)> = Vec::new();
        let mut opencode_config_update: Option<(AcpRuntimeHandle, OpenCodeConfigSelections)> = None;

        match record.session.agent {
            agent if agent.supports_opencode_settings() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.codex_fast_mode.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "OpenCode sessions only support model, reasoning variant, and mode settings",
                    ));
                }
            }
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.opencode_effort.is_some()
                    || request.opencode_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude/OpenCode settings can only be changed for their matching sessions",
                    ));
                }
                if request.cursor_mode.is_some() || request.gemini_approval_mode.is_some() {
                    return Err(ApiError::bad_request(
                        "Codex sessions do not support Cursor or Gemini settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.codex_fast_mode.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                    || request.opencode_effort.is_some()
                    || request.opencode_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model, mode, and effort settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.codex_fast_mode.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.gemini_approval_mode.is_some()
                    || request.opencode_effort.is_some()
                    || request.opencode_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support model and mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.codex_fast_mode.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.opencode_effort.is_some()
                    || request.opencode_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support model and approval mode settings",
                    ));
                }
            }
            agent => {
                if request.model.is_some()
                    || request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.codex_fast_mode.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                    || request.opencode_effort.is_some()
                    || request.opencode_mode.is_some()
                {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support prompt settings yet",
                        agent.name()
                    )));
                }
            }
        }

        if let Some(name) = request.name.as_deref() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session name cannot be empty"));
            }
            record.session.name = trimmed.to_owned();
        }

        if let Some(model) = request.model.as_deref() {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session model cannot be empty"));
            }
        }
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                matching_session_model_option_value(value, &record.session.model_options)
                    .unwrap_or_else(|| value.to_owned())
            });
        let requested_opencode_model = if record.session.agent.supports_opencode_settings() {
            request
                .model
                .as_deref()
                .map(normalize_opencode_model)
                .transpose()
                .map_err(|err| ApiError::bad_request(err.to_string()))?
                .map(|value| {
                    if value == OPENCODE_CONFIG_AUTO {
                        return Ok(value);
                    }
                    if record.session.model_options.is_empty() {
                        return Ok(value);
                    }
                    matching_session_model_option_value(&value, &record.session.model_options)
                        .ok_or_else(|| {
                            ApiError::bad_request(format!(
                                "OpenCode no longer offers model `{value}`"
                            ))
                        })
                })
                .transpose()?
        } else {
            None
        };
        let opencode_model_change_requested = requested_opencode_model
            .as_deref()
            .is_some_and(|model| record.session.opencode_model.as_deref() != Some(model));
        let requested_opencode_effort = if record.session.agent.supports_opencode_settings() {
            request
                .opencode_effort
                .as_deref()
                .map(normalize_opencode_effort)
                .transpose()
                .map_err(|err| ApiError::bad_request(err.to_string()))?
                .map(|value| {
                    if value == OPENCODE_CONFIG_AUTO {
                        return Ok(value);
                    }
                    // Effort lists are model-specific. A combined model+effort
                    // request must be validated by the serialized writer only
                    // after OpenCode advertises the new model's options.
                    if opencode_model_change_requested
                        || record.session.opencode_effort_options.is_empty()
                    {
                        return Ok(value);
                    }
                    matching_session_model_option_value(
                        &value,
                        &record.session.opencode_effort_options,
                    )
                    .ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "OpenCode no longer offers reasoning variant `{value}`"
                        ))
                    })
                })
                .transpose()?
        } else {
            None
        };
        let requested_opencode_mode = if record.session.agent.supports_opencode_settings() {
            request
                .opencode_mode
                .as_deref()
                .map(normalize_opencode_mode)
                .transpose()
                .map_err(|err| ApiError::bad_request(err.to_string()))?
                .map(|value| {
                    if value == OPENCODE_CONFIG_AUTO {
                        return Ok(value);
                    }
                    if opencode_model_change_requested
                        || record.session.opencode_mode_options.is_empty()
                    {
                        return Ok(value);
                    }
                    matching_session_model_option_value(
                        &value,
                        &record.session.opencode_mode_options,
                    )
                    .ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "OpenCode no longer offers mode `{value}`"
                        ))
                    })
                })
                .transpose()?
        } else {
            None
        };

        match record.session.agent {
            agent if agent.supports_opencode_settings() => {
                let live_handle = match (&record.runtime, record.external_session_id.as_deref()) {
                    (SessionRuntime::Acp(handle), Some(_)) => Some(handle.clone()),
                    _ => None,
                };
                let changed_model = requested_opencode_model.filter(|model| {
                    record.session.opencode_model.as_deref() != Some(model.as_str())
                });
                let changed_effort = requested_opencode_effort.clone().filter(|effort| {
                    record.session.opencode_effort.as_deref() != Some(effort.as_str())
                });
                let changed_mode = requested_opencode_mode.clone().filter(|mode| {
                    record.session.opencode_mode.as_deref() != Some(mode.as_str())
                });
                if let Some(handle) = live_handle
                    && (changed_model.is_some()
                        || changed_effort.is_some()
                        || changed_mode.is_some())
                {
                    let selections = if changed_model.is_some() {
                        // Model changes can replace both dependent option
                        // lists. Re-apply the existing explicit authority even
                        // when the request did not change its stored string.
                        OpenCodeConfigSelections {
                            model: changed_model,
                            effort: requested_opencode_effort.or_else(|| {
                                record.session.opencode_effort.clone()
                            }),
                            mode: requested_opencode_mode
                                .or_else(|| record.session.opencode_mode.clone()),
                        }
                    } else {
                        OpenCodeConfigSelections {
                            model: None,
                            effort: changed_effort,
                            mode: changed_mode,
                        }
                    };
                    opencode_config_update = Some((handle, selections));
                } else {
                    if let Some(model) = changed_model {
                        record.session.opencode_model = Some(model.clone());
                        if model != OPENCODE_CONFIG_AUTO {
                            record.session.model = model;
                        }
                    }
                    if let Some(effort) = changed_effort {
                        record.session.opencode_effort = Some(effort);
                    }
                    if let Some(mode) = changed_mode {
                        record.session.opencode_mode = Some(mode.clone());
                    }
                }
            }
            agent if agent.supports_codex_prompt_settings() => {
                let next_model = requested_model
                    .clone()
                    .unwrap_or_else(|| record.session.model.clone());
                let model_changed = next_model != record.session.model;
                let next_model_supports_fast =
                    codex_model_supports_fast(&next_model, &record.session.model_options);
                let actively_enabling_fast = request.codex_fast_mode == Some(true)
                    && !record.session.codex_fast_mode;
                if actively_enabling_fast && !next_model_supports_fast {
                    if record.session.model_options.is_empty() {
                        return Err(ApiError::bad_request(
                            "refresh models before enabling Fast mode",
                        ));
                    } else {
                        return Err(ApiError::bad_request(format!(
                            "model `{next_model}` does not advertise Codex Fast mode"
                        )));
                    }
                }
                let next_fast_mode = match request.codex_fast_mode {
                    Some(true) if model_changed && !record.session.model_options.is_empty() => {
                        // A client may carry a previously enabled Fast value
                        // with a stale catalog snapshot. The current server
                        // catalog owns model capability: switch the model and
                        // safely clear Fast instead of rejecting the whole
                        // settings request for an authority the user did not
                        // explicitly change.
                        next_model_supports_fast
                    }
                    Some(enabled) => enabled,
                    None if model_changed && !record.session.model_options.is_empty() => {
                        record.session.codex_fast_mode && next_model_supports_fast
                    }
                    None => record.session.codex_fast_mode,
                };
                let next_reasoning_effort = request
                    .reasoning_effort
                    .unwrap_or(record.codex_reasoning_effort);
                let normalized_reasoning_effort = normalized_codex_reasoning_effort(
                    &next_model,
                    next_reasoning_effort,
                    &record.session.model_options,
                );
                if request.reasoning_effort.is_some() {
                    if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                        if normalized_reasoning_effort != next_reasoning_effort {
                            if let Some(option) =
                                codex_model_option(&next_model, &record.session.model_options)
                            {
                                return Err(ApiError::bad_request(format!(
                                    "model `{}` does not support `{}` reasoning effort; choose {}",
                                    option.label,
                                    next_reasoning_effort.as_api_value(),
                                    format_codex_reasoning_efforts(
                                        &option.supported_reasoning_efforts
                                    )
                                )));
                            }
                        }
                    }
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                    }
                }
                record.session.codex_fast_mode = next_fast_mode;
                if let Some(sandbox_mode) = request.sandbox_mode {
                    record.codex_sandbox_mode = sandbox_mode;
                    record.session.sandbox_mode = Some(sandbox_mode);
                }
                if let Some(approval_policy) = request.approval_policy {
                    record.codex_approval_policy = approval_policy;
                    record.session.approval_policy = Some(approval_policy);
                }
                if let Some(reasoning_effort) = request.reasoning_effort {
                    record.codex_reasoning_effort = reasoning_effort;
                    record.session.reasoning_effort = Some(reasoning_effort);
                } else if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                    if record.codex_reasoning_effort != normalized_reasoning_effort {
                        record.codex_reasoning_effort = normalized_reasoning_effort;
                        record.session.reasoning_effort = Some(normalized_reasoning_effort);
                    }
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                let should_restart_for_effort =
                    request.claude_effort.is_some_and(|claude_effort| {
                        record.session.claude_effort != Some(claude_effort)
                    });
                if should_restart_for_effort {
                    record.runtime_reset_required = true;
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if should_restart_for_effort {
                            record.runtime_reset_required = true;
                        } else if let SessionRuntime::Claude(handle) = &record.runtime {
                            if claude_cli_model_arg(model).is_some() {
                                claude_model_update = Some((handle.clone(), model.to_owned()));
                            } else {
                                record.runtime_reset_required = true;
                            }
                        }
                    }
                }
                if let Some(claude_approval_mode) = request.claude_approval_mode {
                    record.session.claude_approval_mode = Some(claude_approval_mode);
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        claude_permission_mode_update = Some((
                            handle.clone(),
                            claude_approval_mode
                                .session_cli_permission_mode()
                                .to_owned(),
                        ));
                    }
                }
                if let Some(claude_effort) = request.claude_effort {
                    record.session.claude_effort = Some(claude_effort);
                }
            }
            agent if agent.supports_cursor_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json_rpc_request_message(
                                    Uuid::new_v4().to_string(),
                                    "session/set_config_option",
                                    json!({
                                        "sessionId": external_session_id,
                                        "optionId": "model",
                                        "value": model,
                                    }),
                                ),
                            ));
                        }
                    }
                }
                if let Some(cursor_mode) = request.cursor_mode {
                    if record.session.cursor_mode != Some(cursor_mode) {
                        record.session.cursor_mode = Some(cursor_mode);
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json_rpc_request_message(
                                    Uuid::new_v4().to_string(),
                                    "session/set_config_option",
                                    json!({
                                        "sessionId": external_session_id,
                                        "optionId": "mode",
                                        "value": cursor_mode.as_acp_value(),
                                    }),
                                ),
                            ));
                        }
                    }
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    record.session.model = model.to_owned();
                }
                if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                    if record.session.gemini_approval_mode != Some(gemini_approval_mode) {
                        record.runtime_reset_required = true;
                    }
                    record.session.gemini_approval_mode = Some(gemini_approval_mode);
                }
            }
            _ => {}
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);

        if let Some((handle, model)) = claude_model_update {
            let _ = handle.input_tx.send(ClaudeRuntimeCommand::SetModel(model));
        }
        if let Some((handle, permission_mode)) = claude_permission_mode_update {
            let _ = handle
                .input_tx
                .send(ClaudeRuntimeCommand::SetPermissionMode(permission_mode));
        }
        for (handle, request) in acp_config_updates {
            let _ = handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(request));
        }

        let Some((handle, selections)) = opencode_config_update else {
            return Ok(snapshot);
        };
        const REQUEST_TIMEOUT_SECONDS: u64 = 55;
        const RESPONSE_HANDOFF_SLACK_SECONDS: u64 = 1;
        const REQUEST_TIMEOUT: Duration = Duration::from_secs(REQUEST_TIMEOUT_SECONDS);
        const EXECUTION_TIMEOUT: Duration = Duration::from_secs(
            REQUEST_TIMEOUT_SECONDS - RESPONSE_HANDOFF_SLACK_SECONDS,
        );
        let request_started_at = std::time::Instant::now();
        let deadline = request_started_at + REQUEST_TIMEOUT;
        let execution_deadline = request_started_at + EXECUTION_TIMEOUT;
        let (started_tx, started_rx) = mpsc::channel();
        let (proceed_tx, proceed_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        handle
            .input_tx
            .send(AcpRuntimeCommand::ApplyOpenCodeConfig {
                selections,
                execution_deadline,
                started_tx,
                proceed_rx,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to queue acknowledged OpenCode config update: {err}"
                ))
            })?;
        let scheduling_budget = deadline
            .saturating_duration_since(std::time::Instant::now())
            .min(Duration::from_secs(5));
        match started_rx.recv_timeout(scheduling_budget) {
            Ok(()) => {
                proceed_tx.send(()).map_err(|_| {
                    ApiError::internal(
                        "OpenCode runtime closed before the config update could proceed",
                    )
                })?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(ApiError::conflict(
                    "OpenCode runtime remained busy before the config update could start; retry the setting change",
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(ApiError::internal(
                    "OpenCode runtime closed before the config update could start",
                ));
            }
        }

        // The writer receives an execution deadline one second earlier than
        // this response deadline. Its three 15-second acknowledgements plus a
        // four-second model-options notification fit after the bounded
        // five-second scheduling window, while the reserved second prevents a
        // completed/expired writer result from racing the API timeout.
        let acknowledgement_budget =
            deadline.saturating_duration_since(std::time::Instant::now());
        match response_rx.recv_timeout(acknowledgement_budget) {
            Ok(Ok(())) => Ok(self.snapshot()),
            Ok(Err(detail)) => Err(ApiError::conflict(format!(
                "OpenCode rejected the config update: {detail}"
            ))),
            Err(err) => Err(ApiError::internal(format!(
                "timed out waiting for OpenCode config acknowledgement: {err}"
            ))),
        }
    }

    /// Asks the agent runtime for its current model list and syncs the
    /// returned `SessionModelOption`s onto the session record. Invoked
    /// when the UI opens the model picker.
    ///
    /// Three handshake paths:
    /// - Codex: sends `CodexRuntimeCommand::RefreshModelList` which
    ///   drives the paginated `model/list` JSON-RPC walk (see
    ///   `fire_codex_model_list_page`), waiting up to 30s for the
    ///   accumulated result.
    /// - Claude CLI (native, not ACP): kills and re-spawns the runtime
    ///   with a response channel so the initialize NDJSON's
    ///   `response.response.models` array is parsed by
    ///   `claude_model_options` and forwarded back.
    /// - ACP agents (Claude-ACP / Cursor / Gemini): reuses the existing
    ///   ACP runtime (or spawns one) and sends
    ///   `AcpRuntimeCommand::RefreshSessionConfig`, which re-triggers
    ///   the session-setup path that emits model options on first
    ///   creation.
    ///
    /// All three paths honour `runtime_reset_required` by tearing down
    /// the current runtime before refreshing. Remote-hosted sessions
    /// proxy the entire call unchanged. Local sessions return `409 Conflict`
    /// while Active/Approval or while a Stop/revocation owner holds the runtime
    /// fence, preventing refresh from replacing a runtime being torn down.
    fn refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_refresh_session_model_options(session_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if inner.sessions[index].runtime_stop_in_progress
            || matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            )
        {
            return Err(ApiError::conflict(
                "session model options cannot be refreshed while the session is active or stopping",
            ));
        }
        let agent = inner.sessions[index].session.agent;
        let engram_mcp = if agent == Agent::Claude {
            engram_mcp_runtime_config_for_session_locked(&inner, session_id)
        } else {
            None
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if agent == Agent::Claude {
            if record.runtime_reset_required {
                if let SessionRuntime::Claude(handle) = &record.runtime {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                }
                record.clear_runtime();
                record.pending_claude_approvals.clear();
                record.pending_claude_user_inputs.clear();
                record.clear_runtime_reset();
            }

            match &record.runtime {
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::internal(
                        "unexpected Codex runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Claude(handle) => {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                    record.clear_runtime();
                    record.pending_claude_approvals.clear();
                    record.pending_claude_user_inputs.clear();
                }
                SessionRuntime::None => {}
            }

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            let delegation_mcp_config = self
                .termal_delegation_mcp_claude_config_json_with_engram(
                    session_id,
                    engram_mcp.as_ref().map(|config| &config.stdio),
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to build Claude delegation MCP config: {err:#}"
                    ))
                })?;
            let handle = spawn_claude_runtime(
                self.clone(),
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
                delegation_mcp_config,
                Some(response_tx),
            )
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to start persistent Claude session: {err:#}"
                ))
            })?;
            record.runtime = SessionRuntime::Claude(handle);
            record.engram_mcp_installed = engram_mcp.map(|config| config.installed);
            drop(inner);

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Claude model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Claude model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Claude model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Claude model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        if agent == Agent::Codex {
            if record.runtime_reset_required {
                if let SessionRuntime::Codex(handle) = &record.runtime {
                    if let Some(shared_session) = &handle.shared_session {
                        shared_session.detach();
                    } else {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Codex session runtime: {err:#}"
                            ))
                        })?;
                    }
                }
                record.clear_runtime();
                record.pending_codex_approvals.clear();
                record.pending_codex_user_inputs.clear();
                record.pending_codex_mcp_elicitations.clear();
                record.pending_codex_app_requests.clear();
                record.clear_runtime_reset();
            }

            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Codex session",
                    ));
                }
                SessionRuntime::Claude(_) => {
                    return Err(ApiError::internal(
                        "unexpected Claude runtime attached to Codex session",
                    ));
                }
                SessionRuntime::None => {
                    let handle = spawn_codex_runtime(
                        self.clone(),
                        record.session.id.clone(),
                        record.session.workdir.clone(),
                    )
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to start persistent Codex session: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::Codex(handle.clone());
                    handle
                }
            };
            drop(inner);

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            handle
                .input_tx
                .send(CodexRuntimeCommand::RefreshModelList { response_tx })
                .map_err(|err| {
                    ApiError::internal(format!("failed to queue Codex model refresh: {err}"))
                })?;

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Codex model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Codex model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Codex model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Codex model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        let expected_acp_agent = agent.acp_runtime().ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} sessions do not expose live model options",
                agent.name()
            ))
        })?;

        if agent == Agent::OpenCode
            && matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            )
        {
            return Err(ApiError::conflict(
                "OpenCode model options cannot be refreshed during an active interaction",
            ));
        }

        if record.runtime_reset_required {
            if let SessionRuntime::Acp(handle) = &record.runtime {
                handle.kill().map_err(|err| {
                    ApiError::internal(format!(
                        "failed to restart {} session runtime: {err:#}",
                        agent.name()
                    ))
                })?;
            }
            record.clear_runtime();
            record.pending_acp_approvals.clear();
            record.pending_acp_approval_order.clear();
            record.clear_runtime_reset();
        }

        // ACP has no standalone "get config options" request. For OpenCode,
        // refresh therefore performs a controlled runtime restart and resumes
        // the persisted external session on the new connection. That fresh
        // handshake is the authoritative source of configOptions; queueing the
        // setup command on an already-ready runtime would be a false-success
        // no-op because `ensure_acp_session_ready` returns immediately.
        if agent == Agent::OpenCode {
            match &record.runtime {
                SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart OpenCode session runtime for model refresh: {err:#}"
                        ))
                    })?;
                    record.clear_runtime();
                    record.pending_acp_approvals.clear();
                    record.pending_acp_approval_order.clear();
                }
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to OpenCode session",
                    ));
                }
                SessionRuntime::Claude(_) | SessionRuntime::Codex(_) => {
                    return Err(ApiError::internal(
                        "unexpected non-ACP runtime attached to OpenCode session",
                    ));
                }
                SessionRuntime::None => {}
            }
        }

        let handle = match &record.runtime {
            SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => handle.clone(),
            SessionRuntime::Acp(_) => {
                return Err(ApiError::internal(
                    "unexpected ACP runtime attached to session",
                ));
            }
            SessionRuntime::Claude(_) => {
                return Err(ApiError::internal(
                    "unexpected Claude runtime attached to ACP session",
                ));
            }
            SessionRuntime::Codex(_) => {
                return Err(ApiError::internal(
                    "unexpected Codex runtime attached to ACP session",
                ));
            }
            SessionRuntime::None => {
                let handle = self.start_acp_runtime_for_turn(
                    record.session.id.clone(),
                    record.session.workdir.clone(),
                    expected_acp_agent,
                    record.session.gemini_approval_mode,
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to start persistent {} session: {err:#}",
                        agent.name()
                    ))
                })?;
                record.runtime = SessionRuntime::Acp(handle.clone());
                handle
            }
        };

        let command = AcpPromptCommand {
            cwd: record.session.workdir.clone(),
            cursor_mode: record.session.cursor_mode,
            model: record
                .session
                .opencode_model
                .clone()
                .unwrap_or_else(|| record.session.model.clone()),
            opencode_effort: record.session.opencode_effort.clone(),
            opencode_mode: record.session.opencode_mode.clone(),
            prompt: String::new(),
            resume_session_id: record.external_session_id.clone(),
        };
        drop(inner);

        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<(), String>>();
        handle
            .input_tx
            .send(AcpRuntimeCommand::RefreshSessionConfig {
                command,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to queue {} model refresh: {err}",
                    agent.name()
                ))
            })?;

        match response_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(self.snapshot()),
            Ok(Err(detail)) => Err(ApiError::internal(format!(
                "failed to refresh {} model options: {detail}",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out refreshing {} model options",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "{} model refresh did not return a result",
                agent.name()
            ))),
        }
    }

}
