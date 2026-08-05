// Owns OpenCode dynamic ACP configuration reconciliation, application, notification waits,
// dependent-option recovery, and bounded ACP configuration-option parsing.
// Does not own generic ACP transport, message handling, process lifecycle, or prompt dispatch.
// Split from acp.rs to keep the OpenCode configuration protocol boundary cohesive.

const MAX_OPENCODE_RECONCILE_FINGERPRINTS: usize = 8;
const MAX_OPENCODE_CONFIG_NOTICE_DETAIL_CHARS: usize = 2_000;
const OPENCODE_POST_MODEL_CONFIG_TIMEOUT: Duration = Duration::from_secs(4);
const OPENCODE_CONFIG_REFRESH_HINT: &str =
    " Use `Refresh models` to reconnect OpenCode and reload model-specific choices.";
/// Reconciles OpenCode's dynamic model/effort/mode choices before prompt dispatch.
///
/// `auto` is agent-authoritative and therefore never emits a set request.
/// Explicit TermAl choices are re-applied in deterministic
/// model-then-effort-then-mode order. If a previously selected value
/// disappeared, TermAl visibly resets that one selection to `auto` and adopts
/// the agent's current value.
fn reconcile_opencode_config(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    termal_session_id: &str,
    agent: AcpAgent,
    external_session_id: &str,
    command: &AcpPromptCommand,
    config_result: &Value,
) -> Result<()> {
    let requested_model = normalize_opencode_model(&command.model)?;
    let requested_effort = normalize_opencode_effort(
        command
            .opencode_effort
            .as_deref()
            .unwrap_or(OPENCODE_CONFIG_AUTO),
    )?;
    let requested_mode = normalize_opencode_mode(
        command
            .opencode_mode
            .as_deref()
            .unwrap_or(OPENCODE_CONFIG_AUTO),
    )?;
    let mut notices = Vec::new();

    let model_update = has_acp_config_option_list(config_result, "model").then(|| {
        let model_options = acp_model_options(config_result, agent);
        reconcile_opencode_config_option(
            writer,
            pending_requests,
            agent,
            external_session_id,
            "model",
            &requested_model,
            current_opencode_config_option_value(config_result, "model", &mut notices),
            &model_options,
            &mut notices,
        )
        .map(|(selection, current)| OpenCodeConfigOptionUpdate {
            selection,
            current,
            options: model_options,
        })
    });
    let model_update = model_update.transpose()?;
    let effort_update = has_acp_config_option_list(config_result, "effort").then(|| {
        let effort_options = acp_opencode_effort_options(config_result);
        reconcile_opencode_config_option(
            writer,
            pending_requests,
            agent,
            external_session_id,
            "effort",
            &requested_effort,
            current_opencode_config_option_value(config_result, "effort", &mut notices),
            &effort_options,
            &mut notices,
        )
        .map(|(selection, current)| OpenCodeConfigOptionUpdate {
            selection,
            current,
            options: effort_options,
        })
    });
    let effort_update = effort_update.transpose()?;
    let mode_update = has_acp_config_option_list(config_result, "mode").then(|| {
        let mode_options = acp_opencode_mode_options(config_result);
        reconcile_opencode_config_option(
            writer,
            pending_requests,
            agent,
            external_session_id,
            "mode",
            &requested_mode,
            current_opencode_config_option_value(config_result, "mode", &mut notices),
            &mode_options,
            &mut notices,
        )
        .map(|(selection, current)| OpenCodeConfigOptionUpdate {
            selection,
            current,
            options: mode_options,
        })
    });
    let mode_update = mode_update.transpose()?;

    if model_update.is_none()
        && effort_update.is_none()
        && mode_update.is_none()
        && notices.is_empty()
    {
        return Ok(());
    }
    state.sync_session_opencode_config(
        termal_session_id,
        OpenCodeConfigUpdate {
            model: model_update,
            effort: effort_update,
            mode: mode_update,
            notices,
        },
    )
}

/// Applies one OpenCode config selection and returns persisted/effective state.
fn reconcile_opencode_config_option(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    agent: AcpAgent,
    external_session_id: &str,
    option_id: &str,
    requested_selection: &str,
    current_value: Option<String>,
    options: &[SessionModelOption],
    notices: &mut Vec<String>,
) -> Result<(String, Option<String>)> {
    if requested_selection == OPENCODE_CONFIG_AUTO {
        return Ok((OPENCODE_CONFIG_AUTO.to_owned(), current_value));
    }

    let requested_normalized = requested_selection.to_ascii_lowercase();
    let matching_value = options.iter().find_map(|option| {
        let value_matches = option.value.to_ascii_lowercase() == requested_normalized;
        let label_matches = option.label.to_ascii_lowercase() == requested_normalized;
        (value_matches || label_matches).then(|| option.value.clone())
    });
    let Some(matching_value) = matching_value else {
        let adopted = current_value
            .as_deref()
            .map(|value| format!(" and adopted OpenCode's current value `{value}`"))
            .unwrap_or_default();
        notices.push(format!(
            "OpenCode no longer offers {option_id} `{requested_selection}`. TermAl switched this session's {option_id} selection to `auto`{adopted}."
        ));
        return Ok((OPENCODE_CONFIG_AUTO.to_owned(), current_value));
    };

    if current_value.as_deref() != Some(matching_value.as_str()) {
        let set_result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/set_config_option",
            json!({
                "sessionId": external_session_id,
                // `configId`, not `optionId` — see the note on the model option.
                "configId": option_id,
                "value": matching_value,
            }),
            Duration::from_secs(15),
            agent,
        );
        if let Err(err) = set_result {
            if acp_json_rpc_response_error(&err).is_none() {
                return Err(err);
            }
            let fallback_selection = current_value
                .clone()
                .unwrap_or_else(|| OPENCODE_CONFIG_AUTO.to_owned());
            let fallback_display = current_value.as_deref().unwrap_or(OPENCODE_CONFIG_AUTO);
            notices.push(format!(
                "OpenCode rejected {option_id} `{requested_selection}`: {err}. \
                 The session continues on `{fallback_display}`."
            ));
            return Ok((fallback_selection, current_value));
        }
    }
    Ok((matching_value.clone(), Some(matching_value)))
}

/// Applies a live OpenCode config-options update on the ACP writer thread.
fn reconcile_opencode_session_config(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    command: &AcpPromptCommand,
    config_result: &Value,
) -> Result<()> {
    if agent != AcpAgent::OpenCode {
        bail!("only OpenCode supports dynamic ACP config reconciliation");
    }
    let external_session_id = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .clone()
        .ok_or_else(|| anyhow!("OpenCode ACP session is not ready for config reconciliation"))?;
    reconcile_opencode_config(
        writer,
        pending_requests,
        state,
        session_id,
        agent,
        &external_session_id,
        command,
        config_result,
    )
}

/// Applies an unsolicited OpenCode config update without making the auxiliary
/// reconciliation path runtime-fatal. A late update, rejected saved selection,
/// or transient config request must remain visible and recoverable while the
/// active ACP prompt and process continue independently.
fn handle_opencode_config_reconcile_command(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    config_result: &Value,
) -> Result<()> {
    let command = match state.opencode_config_command(session_id) {
        Ok(command) => command,
        Err(err) => {
            // Config notifications are auxiliary to the active prompt. A
            // deletion/teardown race must not make the ACP writer fatal.
            eprintln!(
                "runtime state warning> ignored late OpenCode config update for \
                 session `{session_id}`: {err:#}"
            );
            return Ok(());
        }
    };
    let reconcile_fingerprint = json!({
        "requestedModel": command.model.clone(),
        "requestedEffort": command.opencode_effort.clone(),
        "requestedMode": command.opencode_mode.clone(),
        "config": config_result.clone(),
    });
    if runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .opencode_reconcile_fingerprints
        .contains(&reconcile_fingerprint)
    {
        return Ok(());
    }
    let reconcile_result = reconcile_opencode_session_config(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        &command,
        config_result,
    );
    if reconcile_result.is_ok() {
        let mut runtime = runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned");
        if runtime.current_session_id.is_some() {
            runtime
                .opencode_reconcile_fingerprints
                .push_back(reconcile_fingerprint);
            while runtime.opencode_reconcile_fingerprints.len()
                > MAX_OPENCODE_RECONCILE_FINGERPRINTS
            {
                runtime.opencode_reconcile_fingerprints.pop_front();
            }
        }
        return Ok(());
    }
    let err = reconcile_result.expect_err("failed OpenCode reconciliation should carry an error");

    if acp_error_is_transport_failure(&err) {
        return Err(err);
    }
    let detail = format!("{err:#}");
    eprintln!(
        "runtime state warning> failed to reconcile OpenCode config for session \
         `{session_id}` without stopping the runtime: {detail}"
    );
    if let Err(notice_err) =
        push_opencode_config_reconciliation_failure_notice(state, session_id, &detail)
    {
        eprintln!(
            "runtime state warning> failed to surface OpenCode config reconciliation \
             warning for session `{session_id}`: {notice_err:#}"
        );
    }
    Ok(())
}

/// Applies a user-requested OpenCode config change on the serialized ACP
/// writer and reports protocol rejection without tearing down a healthy
/// runtime. Transport failures still terminate the runtime so the next prompt
/// must re-establish and reconcile authority before dispatch.
#[allow(clippy::too_many_arguments)]
fn handle_opencode_config_apply_command(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    selections: OpenCodeConfigSelections,
    execution_deadline: std::time::Instant,
    started_tx: Sender<()>,
    proceed_rx: mpsc::Receiver<()>,
    response_tx: Sender<std::result::Result<(), String>>,
) -> Result<()> {
    // The API owns the scheduling deadline. The return acknowledgement closes
    // the edge race where `started_tx.send` succeeds just as `recv_timeout`
    // expires: the writer applies only after the API has observed the start
    // signal and explicitly authorized execution.
    if started_tx.send(()).is_err() || proceed_rx.recv().is_err() {
        return Ok(());
    }
    let result = apply_opencode_config_update(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        selections,
        execution_deadline,
    );
    match result {
        Ok(()) => {
            // Report success before the nonessential fingerprint cleanup. A
            // contended runtime-state mutex must not make the API time out
            // after all acknowledged session-state commits already landed.
            let _ = response_tx.send(Ok(()));
            // A user-authority change starts a new deduplication generation.
            // Old A/B notification cycles remain suppressed within their
            // generation, while a legitimate later A selection can recur.
            runtime_state
                .lock()
                .expect("ACP runtime state mutex poisoned")
                .opencode_reconcile_fingerprints
                .clear();
            Ok(())
        }
        Err(err) => {
            let detail = format!("{err:#}");
            let _ = response_tx.send(Err(detail));
            if acp_error_is_transport_failure(&err) {
                Err(err)
            } else {
                Ok(())
            }
        }
    }
}

/// Sends acknowledged OpenCode model/effort/mode changes in deterministic order and
/// commits each selection only after the agent accepts it.
fn apply_opencode_config_update(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    selections: OpenCodeConfigSelections,
    execution_deadline: std::time::Instant,
) -> Result<()> {
    apply_opencode_config_update_with_timeout(
        writer,
        pending_requests,
        state,
        session_id,
        runtime_state,
        agent,
        selections,
        execution_deadline,
        OPENCODE_POST_MODEL_CONFIG_TIMEOUT,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_opencode_config_update_with_timeout(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    agent: AcpAgent,
    selections: OpenCodeConfigSelections,
    execution_deadline: std::time::Instant,
    post_model_options_timeout: Duration,
) -> Result<()> {
    ensure_opencode_config_execution_active(execution_deadline, "starting the update")?;
    if agent != AcpAgent::OpenCode {
        bail!("only OpenCode supports acknowledged dynamic config updates");
    }
    let external_session_id = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .current_session_id
        .clone()
        .ok_or_else(|| anyhow!("OpenCode ACP session is not ready for config updates"))?;

    let needs_effort = selections
        .effort
        .as_deref()
        .is_some_and(|selection| selection != OPENCODE_CONFIG_AUTO);
    let needs_mode = selections
        .mode
        .as_deref()
        .is_some_and(|selection| selection != OPENCODE_CONFIG_AUTO);
    let notification_subscription = (selections.model.is_some() && (needs_effort || needs_mode))
        .then(|| subscribe_to_opencode_config_notifications(runtime_state));

    let snapshot = state.opencode_config_snapshot(session_id)?;
    let model_request = selections
        .model
        .as_deref()
        .map(|requested| {
            apply_opencode_config_selection(
                writer,
                pending_requests,
                state,
                session_id,
                agent,
                &external_session_id,
                "model",
                requested,
                snapshot.model_selection,
                Some(snapshot.effective_model),
                snapshot.model_options,
                false,
                execution_deadline,
            )
        })
        .transpose()?
        .flatten();

    let mut skip_effort = false;
    let mut skip_mode = false;
    let refreshed = if let Some(expected_model) = model_request.as_deref() {
        let notification_timeout = remaining_opencode_config_execution_time(
            execution_deadline,
            "waiting for post-model config options",
        )?
        .min(post_model_options_timeout);
        let wait_result = notification_subscription
            .as_ref()
            .map(|subscription| {
                wait_for_opencode_post_model_options(
                    &subscription.receiver,
                    expected_model,
                    needs_effort,
                    needs_mode,
                    notification_timeout,
                )
            })
            .transpose();
        match wait_result {
            Ok(options) => options,
            Err(incomplete) => {
                ensure_opencode_config_execution_active(
                    execution_deadline,
                    "recovering incomplete post-model config options",
                )?;
                skip_effort = needs_effort && incomplete.options.effort.is_none();
                skip_mode = needs_mode && incomplete.options.mode.is_none();
                reset_unreported_opencode_dependents_after_model_change(
                    state,
                    session_id,
                    expected_model,
                    &selections,
                    &incomplete.options,
                    &incomplete.reason,
                    execution_deadline,
                )?;
                Some(incomplete.options)
            }
        }
    } else {
        None
    };

    let snapshot = state.opencode_config_snapshot(session_id)?;
    if let Some(requested) = selections.effort.as_deref().filter(|_| !skip_effort) {
        let refreshed_effort = refreshed
            .as_ref()
            .and_then(|options| options.effort.clone());
        let options_are_authoritative = refreshed_effort.is_some();
        let (current, options) =
            refreshed_effort.unwrap_or((snapshot.current_effort, snapshot.effort_options));
        apply_opencode_dependent_selection(
            writer,
            pending_requests,
            state,
            session_id,
            agent,
            &external_session_id,
            "effort",
            requested,
            snapshot.effort_selection,
            current,
            options,
            selections.model.as_deref(),
            options_are_authoritative,
            execution_deadline,
        )?;
    }

    let snapshot = state.opencode_config_snapshot(session_id)?;
    if let Some(requested) = selections.mode.as_deref().filter(|_| !skip_mode) {
        let refreshed_mode = refreshed.as_ref().and_then(|options| options.mode.clone());
        let options_are_authoritative = refreshed_mode.is_some();
        let (current, options) =
            refreshed_mode.unwrap_or((snapshot.current_mode, snapshot.mode_options));
        apply_opencode_dependent_selection(
            writer,
            pending_requests,
            state,
            session_id,
            agent,
            &external_session_id,
            "mode",
            requested,
            snapshot.mode_selection,
            current,
            options,
            selections.model.as_deref(),
            options_are_authoritative,
            execution_deadline,
        )?;
    }
    // Every requested stage has either completed its deadline-guarded commit or
    // returned an error before reaching this point. Do not check the deadline
    // again after the final commit: doing so can report wholesale failure even
    // though the acknowledged state change is already visible and persisted.
    Ok(())
}

fn ensure_opencode_config_execution_active(
    execution_deadline: std::time::Instant,
    stage: &str,
) -> Result<()> {
    if std::time::Instant::now() >= execution_deadline {
        bail!("OpenCode config request deadline expired before {stage}");
    }
    Ok(())
}

fn remaining_opencode_config_execution_time(
    execution_deadline: std::time::Instant,
    stage: &str,
) -> Result<Duration> {
    let remaining = execution_deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        bail!("OpenCode config request deadline expired before {stage}");
    }
    Ok(remaining)
}

struct OpenCodeConfigNotificationSubscription {
    runtime_state: Arc<Mutex<AcpRuntimeState>>,
    receiver: mpsc::Receiver<OpenCodeConfigNotification>,
}

impl Drop for OpenCodeConfigNotificationSubscription {
    fn drop(&mut self) {
        self.runtime_state
            .lock()
            .expect("ACP runtime state mutex poisoned")
            .opencode_config_notification_tx = None;
    }
}

/// Installs the one notification subscriber owned by the serialized ACP
/// writer. The returned guard unregisters it on success and every error path.
fn subscribe_to_opencode_config_notifications(
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
) -> OpenCodeConfigNotificationSubscription {
    let (sender, receiver) = mpsc::channel();
    runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned")
        .opencode_config_notification_tx = Some(sender);
    OpenCodeConfigNotificationSubscription {
        runtime_state: Arc::clone(runtime_state),
        receiver,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_opencode_dependent_selection(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    agent: AcpAgent,
    external_session_id: &str,
    option_id: &str,
    requested_selection: &str,
    current_selection: String,
    current_value: Option<String>,
    options: Vec<SessionModelOption>,
    changed_model: Option<&str>,
    options_are_authoritative: bool,
    execution_deadline: std::time::Instant,
) -> Result<()> {
    if requested_selection != OPENCODE_CONFIG_AUTO
        && matching_session_model_option_value(requested_selection, &options).is_none()
    {
        if let Some(model) = changed_model {
            return reset_opencode_dependent_selection_after_model_change(
                state,
                session_id,
                model,
                option_id,
                requested_selection,
                current_value,
                options,
                "no longer offers that selection",
                None,
                execution_deadline,
            );
        }
    }

    let result = apply_opencode_config_selection(
        writer,
        pending_requests,
        state,
        session_id,
        agent,
        external_session_id,
        option_id,
        requested_selection,
        current_selection,
        current_value.clone(),
        options.clone(),
        options_are_authoritative,
        execution_deadline,
    );
    match result {
        Ok(_) => Ok(()),
        Err(err) if changed_model.is_some() && acp_json_rpc_response_error(&err).is_some() => {
            reset_opencode_dependent_selection_after_model_change(
                state,
                session_id,
                changed_model.expect("changed model checked above"),
                option_id,
                requested_selection,
                current_value,
                options,
                &format!("rejected the selection: {err}"),
                None,
                execution_deadline,
            )
        }
        Err(err) => Err(err),
    }
}

#[allow(clippy::too_many_arguments)]
fn reset_opencode_dependent_selection_after_model_change(
    state: &AppState,
    session_id: &str,
    model: &str,
    option_id: &str,
    requested_selection: &str,
    current: Option<String>,
    options: Vec<SessionModelOption>,
    reason: &str,
    recovery_hint: Option<&str>,
    execution_deadline: std::time::Instant,
) -> Result<()> {
    let reason = bounded_opencode_config_notice_detail(reason);
    let adopted = current
        .as_deref()
        .map(|value| format!(" OpenCode currently reports `{value}`."))
        .unwrap_or_else(|| " OpenCode has not reported a current value yet.".to_owned());
    let option_update = OpenCodeConfigOptionUpdate {
        selection: OPENCODE_CONFIG_AUTO.to_owned(),
        current,
        options,
    };
    let recovery_hint = recovery_hint.unwrap_or_default();
    let mut update = OpenCodeConfigUpdate {
        notices: vec![format!(
            "After changing model to `{model}`, OpenCode {reason} for {option_id} `{requested_selection}`. \
             TermAl switched this session's {option_id} selection to `auto`.{adopted}{recovery_hint}"
        )],
        ..OpenCodeConfigUpdate::default()
    };
    match option_id {
        "effort" => update.effort = Some(option_update),
        "mode" => update.mode = Some(option_update),
        _ => bail!("unsupported model-dependent OpenCode config option `{option_id}`"),
    }
    state.sync_session_opencode_config_before_deadline(session_id, update, execution_deadline)
}

fn reset_unreported_opencode_dependents_after_model_change(
    state: &AppState,
    session_id: &str,
    model: &str,
    selections: &OpenCodeConfigSelections,
    refreshed: &OpenCodePostModelOptions,
    reason: &str,
    execution_deadline: std::time::Instant,
) -> Result<()> {
    for (option_id, requested, reported) in [
        (
            "effort",
            selections.effort.as_deref(),
            refreshed.effort.as_ref(),
        ),
        ("mode", selections.mode.as_deref(), refreshed.mode.as_ref()),
    ] {
        let Some(requested) = requested else {
            continue;
        };
        if requested == OPENCODE_CONFIG_AUTO {
            state.sync_session_opencode_selection_before_deadline(
                session_id,
                option_id,
                OPENCODE_CONFIG_AUTO.to_owned(),
                execution_deadline,
            )?;
            continue;
        }
        if reported.is_some() {
            continue;
        }
        reset_opencode_dependent_selection_after_model_change(
            state,
            session_id,
            model,
            option_id,
            requested,
            None,
            Vec::new(),
            reason,
            Some(OPENCODE_CONFIG_REFRESH_HINT),
            execution_deadline,
        )?;
    }
    Ok(())
}

/// Applies one acknowledged OpenCode option and returns the value for which a
/// protocol request was emitted. Equal persisted authority does not suppress a
/// dependent re-apply when the newly selected model reports another effective
/// value.
#[allow(clippy::too_many_arguments)]
fn apply_opencode_config_selection(
    writer: &mut impl Write,
    pending_requests: &AcpPendingRequestMap,
    state: &AppState,
    session_id: &str,
    agent: AcpAgent,
    external_session_id: &str,
    option_id: &str,
    requested_selection: &str,
    current_selection: String,
    current_value: Option<String>,
    options: Vec<SessionModelOption>,
    options_are_authoritative: bool,
    execution_deadline: std::time::Instant,
) -> Result<Option<String>> {
    ensure_opencode_config_execution_active(execution_deadline, "applying a selection")?;
    if requested_selection == OPENCODE_CONFIG_AUTO {
        if current_selection != OPENCODE_CONFIG_AUTO || options_are_authoritative {
            sync_applied_opencode_selection(
                state,
                session_id,
                option_id,
                OPENCODE_CONFIG_AUTO.to_owned(),
                current_value,
                options,
                execution_deadline,
            )?;
        }
        return Ok(None);
    }

    let matching_value = matching_session_model_option_value(requested_selection, &options)
        .ok_or_else(|| {
            anyhow!(
                "OpenCode no longer offers {option_id} `{requested_selection}`; refresh the options and choose again"
            )
        })?;
    let request_sent = current_value.as_deref() != Some(matching_value.as_str());
    if request_sent {
        let timeout = remaining_opencode_config_execution_time(
            execution_deadline,
            "waiting for an OpenCode config acknowledgement",
        )?
        .min(Duration::from_secs(15));
        let request_result = send_acp_json_rpc_request(
            writer,
            pending_requests,
            "session/set_config_option",
            json!({
                "sessionId": external_session_id,
                // `configId`, not `optionId` — see the note on the model option.
                "configId": option_id,
                "value": matching_value,
            }),
            timeout,
            agent,
        );
        if request_result.is_err() && std::time::Instant::now() >= execution_deadline {
            bail!(
                "OpenCode config request deadline expired while waiting for {option_id} acknowledgement"
            );
        }
        request_result?;
    }
    if current_selection != matching_value || request_sent || options_are_authoritative {
        sync_applied_opencode_selection(
            state,
            session_id,
            option_id,
            matching_value.clone(),
            Some(matching_value.clone()),
            options,
            execution_deadline,
        )?;
    }
    Ok(request_sent.then_some(matching_value))
}

/// Atomically commits one acknowledged selection together with the option list
/// used to validate it. Post-model lists are authoritative immediately after
/// the ACK; waiting for the separately queued reconciliation command would
/// leave a brief UI window containing the prior model's choices.
fn sync_applied_opencode_selection(
    state: &AppState,
    session_id: &str,
    option_id: &str,
    selection: String,
    current: Option<String>,
    options: Vec<SessionModelOption>,
    execution_deadline: std::time::Instant,
) -> Result<()> {
    let option_update = OpenCodeConfigOptionUpdate {
        selection,
        current,
        options,
    };
    let mut update = OpenCodeConfigUpdate::default();
    match option_id {
        "model" => update.model = Some(option_update),
        "effort" => update.effort = Some(option_update),
        "mode" => update.mode = Some(option_update),
        _ => bail!("unsupported OpenCode config option `{option_id}`"),
    }
    state.sync_session_opencode_config_before_deadline(session_id, update, execution_deadline)
}

#[derive(Debug, Default)]
struct OpenCodePostModelOptions {
    effort: Option<(Option<String>, Vec<SessionModelOption>)>,
    mode: Option<(Option<String>, Vec<SessionModelOption>)>,
}

#[derive(Debug)]
struct IncompleteOpenCodePostModelOptions {
    options: OpenCodePostModelOptions,
    reason: String,
}

/// Waits for config options emitted after a model request.
///
/// A matching reported model establishes explicit authority. OpenCode may also
/// emit a dependent-only update after acknowledging the model request; because
/// the subscription is installed immediately before that request, the first
/// such update is authoritative unless an intervening notification explicitly
/// reported a different model. The reader records notifications directly, so
/// this bounded wait does not deadlock behind its queued reconciliation command.
fn wait_for_opencode_post_model_options(
    notifications: &mpsc::Receiver<OpenCodeConfigNotification>,
    expected_model: &str,
    needs_effort: bool,
    needs_mode: bool,
    timeout: Duration,
) -> std::result::Result<OpenCodePostModelOptions, IncompleteOpenCodePostModelOptions> {
    let deadline = std::time::Instant::now() + timeout;
    let mut model_is_authoritative = false;
    let mut saw_mismatched_model = false;
    let mut options = OpenCodePostModelOptions::default();
    loop {
        let now = std::time::Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            return Err(IncompleteOpenCodePostModelOptions {
                options,
                reason: "did not publish model-specific config options in time".to_owned(),
            });
        }
        let notification = match notifications.recv_timeout(remaining) {
            Ok(notification) => notification,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(IncompleteOpenCodePostModelOptions {
                    options,
                    reason: "did not publish model-specific config options in time".to_owned(),
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(IncompleteOpenCodePostModelOptions {
                    options,
                    reason: "closed the config notification stream before publishing model-specific options"
                        .to_owned(),
                });
            }
        };
        if let Some(current_model) = notification.model {
            model_is_authoritative = normalize_opencode_model(&current_model)
                .is_ok_and(|current| current == expected_model);
            saw_mismatched_model = !model_is_authoritative;
            if !model_is_authoritative {
                options = OpenCodePostModelOptions::default();
            }
        } else if !model_is_authoritative
            && !saw_mismatched_model
            && (notification.effort.is_some() || notification.mode.is_some())
        {
            model_is_authoritative = true;
        }
        if !model_is_authoritative {
            continue;
        }
        if notification.effort.is_some() {
            options.effort = notification.effort;
        }
        if notification.mode.is_some() {
            options.mode = notification.mode;
        }
        if model_is_authoritative
            && (!needs_effort || options.effort.is_some())
            && (!needs_mode || options.mode.is_some())
        {
            return Ok(options);
        }
    }
}

/// Appends a bounded, actionable transcript notice for a non-fatal OpenCode
/// config reconciliation failure.
fn push_opencode_config_reconciliation_failure_notice(
    state: &AppState,
    session_id: &str,
    detail: &str,
) -> Result<()> {
    let bounded = bounded_opencode_config_notice_detail(detail);
    let suffix = if bounded.is_empty() {
        String::new()
    } else {
        format!("\n\nDetails: {bounded}")
    };
    state.push_message(
        session_id,
        Message::Text {
            attachments: Vec::new(),
            id: state.allocate_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: format!(
                "OpenCode config update warning: TermAl could not reconcile the latest \
                 model, reasoning variant, and mode options. The current session remains available; refresh \
                 the options or choose the setting again.{suffix}"
            ),
            expanded_text: None,
            source: None,
        },
    )
}

/// Bounds agent-controlled diagnostic text before it enters a persisted
/// OpenCode config notice. Callers add their own actionable context around the
/// returned detail, so this helper only trims and truncates the untrusted part.
fn bounded_opencode_config_notice_detail(detail: &str) -> String {
    let trimmed = detail.trim();
    let mut chars = trimmed.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_OPENCODE_CONFIG_NOTICE_DETAIL_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// Returns whether this config payload contains a usable list for one option.
///
/// Some handshakes and notifications carry only the options that changed, or
/// include a current value without the selectable list. Absence is not proof
/// that a previously saved explicit selection disappeared; only a present list
/// can support that conclusion.
fn has_acp_config_option_list(config_result: &Value, option_id: &str) -> bool {
    acp_config_options(config_result).is_some_and(|options| {
        options.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some(option_id)
                && entry.get("options").and_then(Value::as_array).is_some()
        })
    })
}

/// Returns the current ACP config option value.
fn current_acp_config_option_value(config_result: &Value, option_id: &str) -> Option<String> {
    acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))
        .and_then(|entry| entry.get("currentValue").and_then(Value::as_str))
        .map(str::to_owned)
}

/// Returns a bounded, single-line OpenCode effective config value.
///
/// OpenCode owns `currentValue`, but the value still crosses persistence, API,
/// SSE, and UI boundaries. Treat malformed agent output like an absent current
/// value and surface a bounded notice instead of persisting it.
fn current_opencode_config_option_value(
    config_result: &Value,
    option_id: &str,
    notices: &mut Vec<String>,
) -> Option<String> {
    let value = current_acp_config_option_value(config_result, option_id)?;
    let normalized = match option_id {
        "model" => normalize_opencode_model(&value),
        "effort" => normalize_opencode_effort(&value),
        "mode" => normalize_opencode_mode(&value),
        _ => {
            notices.push(format!(
                "OpenCode reported an unsupported current config option `{option_id}`. \
                 TermAl ignored it."
            ));
            return None;
        }
    };
    match normalized {
        Ok(value) => Some(value),
        Err(_) => {
            notices.push(format!(
                "OpenCode reported an invalid current {option_id}. TermAl ignored it \
                 to keep persisted session state bounded and single-line."
            ));
            None
        }
    }
}

/// Returns the matching ACP config option value.
fn matching_acp_config_option_value(
    config_result: &Value,
    option_id: &str,
    requested_value: &str,
) -> Option<String> {
    let requested = requested_value.trim();
    if requested.is_empty() {
        return None;
    }
    let requested_normalized = requested.to_ascii_lowercase();
    let option = acp_config_options(config_result)?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))?;
    let options = option.get("options").and_then(Value::as_array)?;
    options.iter().find_map(|entry| {
        let value = entry.get("value").and_then(Value::as_str)?;
        let name = entry
            .get("name")
            .or_else(|| entry.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let value_normalized = value.to_ascii_lowercase();
        let name_normalized = name.to_ascii_lowercase();
        if value_normalized == requested_normalized || name_normalized == requested_normalized {
            Some(value.to_owned())
        } else {
            None
        }
    })
}

/// Handles ACP model options without imposing one agent's ingress contract on
/// the others.
fn acp_model_options(config_result: &Value, agent: AcpAgent) -> Vec<SessionModelOption> {
    let max_value_chars = (agent == AcpAgent::OpenCode).then_some(MAX_OPENCODE_MODEL_CHARS);
    acp_session_config_options(config_result, "model", max_value_chars)
}

/// Handles bounded, model-specific OpenCode reasoning variants. OpenCode's
/// TUI calls these variants while ACP exposes the selector as `effort`.
fn acp_opencode_effort_options(config_result: &Value) -> Vec<SessionModelOption> {
    acp_session_config_options(config_result, "effort", Some(MAX_OPENCODE_EFFORT_CHARS))
}

/// Handles bounded OpenCode primary-agent mode options.
fn acp_opencode_mode_options(config_result: &Value) -> Vec<SessionModelOption> {
    acp_session_config_options(config_result, "mode", Some(MAX_OPENCODE_MODE_CHARS))
}

fn acp_session_config_options(
    config_result: &Value,
    option_id: &str,
    max_value_chars: Option<usize>,
) -> Vec<SessionModelOption> {
    let Some(option) = acp_config_options(config_result).and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(option_id))
    }) else {
        return Vec::new();
    };
    option
        .get("options")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let value = entry.get("value").and_then(Value::as_str)?.trim();
                    if value.is_empty()
                        || max_value_chars.is_some_and(|max| {
                            value.chars().count() > max || value.chars().any(char::is_control)
                        })
                    {
                        return None;
                    }
                    let label = entry
                        .get("name")
                        .or_else(|| entry.get("label"))
                        .and_then(Value::as_str)
                        .and_then(|label| {
                            bounded_acp_option_text(label, MAX_ACP_OPTION_LABEL_CHARS)
                        })
                        .unwrap_or(value);
                    let description =
                        entry
                            .get("description")
                            .and_then(Value::as_str)
                            .and_then(|description| {
                                bounded_acp_option_text(
                                    description,
                                    MAX_ACP_OPTION_DESCRIPTION_CHARS,
                                )
                            });
                    Some(SessionModelOption {
                        label: label.to_owned(),
                        value: value.to_owned(),
                        description: description.map(str::to_owned),
                        badges: Vec::new(),
                        supported_claude_effort_levels: Vec::new(),
                        default_reasoning_effort: None,
                        supported_reasoning_efforts: Vec::new(),
                        service_tiers: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn bounded_acp_option_text(value: &str, max_chars: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control))
    .then_some(value)
}

/// Handles ACP config options.
fn acp_config_options(config_result: &Value) -> Option<&Vec<Value>> {
    config_result
        .get("configOptions")
        .or_else(|| config_result.get("config_options"))
        .and_then(Value::as_array)
}

/// Signals bounded OpenCode config facts before the reader queues writer-side
/// reconciliation. A live model+effort/mode update can therefore wait for
/// model-authoritative lists without deadlocking or retaining raw agent JSON.
fn record_opencode_config_notification(
    runtime_state: &Arc<Mutex<AcpRuntimeState>>,
    config_result: &Value,
) {
    // This early parse only wakes the serialized writer. The same raw update
    // is queued immediately afterward for writer-side reconciliation, which
    // re-parses and publishes normalization notices; do not emit them twice.
    let mut notices = Vec::new();
    let notification = OpenCodeConfigNotification {
        model: current_opencode_config_option_value(config_result, "model", &mut notices),
        effort: has_acp_config_option_list(config_result, "effort").then(|| {
            (
                current_opencode_config_option_value(config_result, "effort", &mut notices),
                acp_opencode_effort_options(config_result),
            )
        }),
        mode: has_acp_config_option_list(config_result, "mode").then(|| {
            (
                current_opencode_config_option_value(config_result, "mode", &mut notices),
                acp_opencode_mode_options(config_result),
            )
        }),
    };
    let mut runtime = runtime_state
        .lock()
        .expect("ACP runtime state mutex poisoned");
    if runtime
        .opencode_config_notification_tx
        .as_ref()
        .is_some_and(|sender| sender.send(notification).is_err())
    {
        runtime.opencode_config_notification_tx = None;
    }
}
