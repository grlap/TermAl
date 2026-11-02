// Turn kickoff + queued-turn dispatcher for `AppState`.
//
// This file owns the entry path that takes a prompt for a session and
// hands it to the correct agent driver (Claude, Codex, or ACP). The
// sibling `turn_lifecycle.rs` owns what happens *after* dispatch — the
// `Idle -> Active -> Approval -> Idle` state transitions + the
// `RuntimeToken` staleness guards. The agent-specific turn runners
// themselves live in `turns.rs` (Claude via `claude.rs`, ACP via
// `acp.rs`) and `codex.rs` / `codex_rpc.rs` for Codex. This file is the
// switchboard that picks the right runner and performs the spawn-if-
// needed bookkeeping.
//
// Queue semantics. Each session has a FIFO of `QueuedPromptRecord`
// entries (see `session_interaction.rs`). When the session is idle
// and a prompt comes in, we dispatch immediately; otherwise we append
// to the queue and drain it one turn at a time as the session
// transitions back to `Idle`. `dispatch_next_queued_turn` is the drain
// callback invoked by `finish_turn_ok` / `mark_turn_error` / similar
// Failed first-follow-up starts carry typed attempt identity to the
// delegation_followup_admission settlement hook. Both the off-lock wrapper
// and direct locked callers must settle that error after releasing state.
// from `turn_lifecycle.rs`. Persisted mailbox and user queues deliberately
// remain dormant after process restart: the next user prompt, explicit resume,
// or new inbound wake is the activation event that drains them. Workflow-owned
// queue heads are different: delegation waits and orchestrator transitions
// already consumed their durable trigger before committing the queued prompt,
// so startup re-kicks those queue heads to preserve crash recovery. A user
// prompt ahead of a workflow prompt remains a strict FIFO barrier.
//
// Runtime reset. `record.runtime_reset_required` signals that the
// session's long-lived runtime handle must be torn down and respawned
// before the next turn. `start_turn_on_record` honors it for every
// local runtime family: Claude and ACP kill their dedicated handle;
// Codex detaches the logical session from its shared app-server handle.
// Each path clears protocol-specific pending interactions before the
// next turn attaches or spawns a fresh session runtime.

struct StartedTurn {
    dispatch: TurnDispatch,
    message_delta: StartedTurnMessageDelta,
}

enum CodexServiceTierPreparation {
    Ready(Option<String>),
    PendingStop,
    Superseded,
}

struct StartedTurnMessageDelta {
    session_id: String,
    message_id: String,
    message_index: usize,
    message_count: u32,
    message: Message,
    preview: String,
    status: SessionStatus,
    session_mutation_stamp: u64,
    session_seq: u64,
}

struct OrphanedWorkflowDispatch {
    dispatch: TurnDispatch,
    queued: QueuedPromptRecord,
}

const ENGRAM_CONTEXT_PROMPT_ADMISSION_RETRIES: usize = 2;

fn engram_context_runtime_prompt(context: &str, runtime_prompt: &str) -> String {
    format!("{}\n\n{runtime_prompt}", engram_context_fence(context))
}

struct StartedQueuedTurn {
    dispatch: TurnDispatch,
    queued: QueuedPromptRecord,
}

fn ordinary_queue_dispatch_is_held(
    inner: &StateInner, record: &SessionRecord, allow_blocked_dispatch: bool,
) -> bool {
    session_resource_reset_in_progress(inner, record)
        || record.engram.observation_delete_requested
        || record.runtime_stop_in_progress
        || matches!(record.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
        || (record.orchestrator_auto_dispatch_blocked && !allow_blocked_dispatch)
}

/// Adds transport context that a receiving agent cannot infer from its own
/// repository instructions. Peer sessions may live in entirely different
/// projects, so the prompt must explain both the context boundary and the
/// explicit MCP reply path. This is runtime-only: the transcript keeps the
/// original message plus its structured [`MessageSource`] attribution.
fn peer_message_runtime_prompt(prompt: &str, source: &MessageSource) -> String {
    let session_id = source.session_id.as_deref().unwrap_or("unknown");
    format!(
        "[TermAl cross-session message]\n\
From session: {:?} (`{}`)\n\
The sender may be working in a different repository. Do not assume it shares this session's repository instructions, tracker state, or skills.\n\
To reply to the sender, use the TermAl MCP tool `termal_send_to_session` with `sessionId` set to `{}`. A normal assistant reply stays only in this session and will not reach the sender.\n\n\
--- Message from {:?} ---\n\
{}",
        source.name, session_id, session_id, source.name, prompt
    )
}

impl AppState {
    /// Resolves an admitted Fast turn before any thread/turn request is queued.
    /// Discovery runs on the Fast delivery worker, never the shared stdout reader,
    /// and waits outside StateInner; its result belongs only to the same
    /// runtime and turn generation. Publishing capabilities never changes Fast
    /// authority or re-normalizes the admitted command's model/settings.
    fn prepare_codex_service_tier_off_lock(
        &self,
        session_id: &str,
        runtime_token: &RuntimeToken,
        active_turn_generation: u64,
        model: &str,
        service_tier: &Result<Option<String>>,
        sender: &mpsc::Sender<CodexRuntimeCommand>,
    ) -> Result<CodexServiceTierPreparation> {
        if let Ok(tier) = service_tier {
            return Ok(CodexServiceTierPreparation::Ready(tier.clone()));
        }
        let (response_tx, response_rx) = mpsc::channel();
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(record) = inner.sessions.iter().find(|record| {
                record.session.id == session_id
                    && record.runtime.matches_runtime_token(runtime_token)
                    && record.active_turn_generation == active_turn_generation
            }) else {
                return Ok(CodexServiceTierPreparation::Superseded);
            };
            if record.runtime_stop_in_progress {
                return Ok(CodexServiceTierPreparation::PendingStop);
            }
            if record.session.status != SessionStatus::Active {
                return Ok(CodexServiceTierPreparation::Superseded);
            }
            sender
                .send(CodexRuntimeCommand::RefreshModelList { response_tx })
                .context("failed to queue Codex model discovery; Fast remains enabled")?;
        }
        let refreshed = response_rx
            .recv_timeout(CODEX_MODEL_DISCOVERY_TIMEOUT)
            .context("Codex model discovery did not return a result; Fast remains enabled")
            .and_then(|result| {
                result.map_err(|detail| {
                    anyhow!("Codex model discovery failed; Fast remains enabled: {detail}")
                })
            });

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(CodexServiceTierPreparation::Superseded);
        };
        let record = &inner.sessions[index];
        if !record.runtime.matches_runtime_token(runtime_token)
            || record.active_turn_generation != active_turn_generation
        {
            return Ok(CodexServiceTierPreparation::Superseded);
        }
        if record.runtime_stop_in_progress {
            return Ok(CodexServiceTierPreparation::PendingStop);
        }
        if record.session.status != SessionStatus::Active {
            return Ok(CodexServiceTierPreparation::Superseded);
        }
        let options = refreshed?;
        let resolved = codex_fast_service_tier_value(model, &options, true);
        if record.session.model_options != options {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .session
                .model_options = options;
            self.commit_locked(&mut inner)?;
        }
        resolved.map(CodexServiceTierPreparation::Ready)
    }

    #[cfg(test)]
    fn install_test_acp_runtime_override(&self, agent: AcpAgent, runtime: AcpRuntimeHandle) {
        self.test_acp_runtime_overrides
            .lock()
            .expect("test ACP runtime overrides mutex poisoned")
            .push(TestAcpRuntimeOverride { agent, runtime });
    }

    #[cfg(test)]
    fn has_test_acp_runtime_override(&self, agent: AcpAgent) -> bool {
        self.test_acp_runtime_overrides
            .lock()
            .expect("test ACP runtime overrides mutex poisoned")
            .iter()
            .any(|override_runtime| override_runtime.agent == agent)
    }

    fn start_acp_runtime_for_turn(
        &self,
        session_id: String,
        workdir: String,
        agent: AcpAgent,
        gemini_approval_mode: Option<GeminiApprovalMode>,
        engram_mcp: Option<&TermalDelegationMcpStdioConfig>,
    ) -> Result<AcpRuntimeHandle> {
        #[cfg(test)]
        {
            let mut overrides = self
                .test_acp_runtime_overrides
                .lock()
                .expect("test ACP runtime overrides mutex poisoned");
            if let Some(index) = overrides
                .iter()
                .position(|override_runtime| override_runtime.agent == agent)
            {
                return Ok(overrides.remove(index).runtime);
            }
        }

        spawn_acp_runtime(
            self.clone(),
            session_id,
            workdir,
            agent,
            gemini_approval_mode,
            engram_mcp,
        )
    }

    fn publish_started_turn_message_delta(
        &self,
        inner: &StateInner,
        revision: u64,
        delta: StartedTurnMessageDelta,
    ) {
        self.publish_delta_locked(
            &inner,
            DeltaEvent::MessageCreated {
                revision,
                session_id: delta.session_id,
                message_id: delta.message_id,
                message_index: delta.message_index,
                message_count: delta.message_count,
                message: delta.message,
                preview: delta.preview,
                status: delta.status,
                session_queue: None,
                session_mutation_stamp: Some(delta.session_mutation_stamp),
                session_seq: Some(delta.session_seq),
                body_seq_epoch: Some(self.server_instance_id.clone()),
            },
        );
    }

    /// Promotes the current queue head while the caller still owns the state
    /// lock. Keeping selection, turn start, and removal in one critical
    /// section prevents a concurrent sender or lifecycle drain from stealing
    /// the prompt between a queue commit and its dispatch.
    fn start_next_queued_turn_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        allow_blocked_dispatch: bool,
    ) -> Result<Option<StartedTurn>> {
        let record = &inner.sessions[index];
        if session_resource_reset_in_progress(inner, record)
            || record.engram.observation_delete_requested
            || record.runtime_stop_in_progress
            || matches!(record.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
            || (record.orchestrator_auto_dispatch_blocked && !allow_blocked_dispatch)
        {
            return Ok(None);
        }
        let Some(position) = ordinary_queue_position(record) else { return Ok(None); };
        let queued = record.queued_prompts[position].clone();
        // Human sends retain their off-lock reset and exact selected owner.
        if queued.source == QueuedPromptSource::Human
            || queued_prompt_held_for_durability(record, &queued)
            || !self.prompt_delivery_gate_locked(inner, index, &queued.pending_prompt.id, true)
        {
            return Ok(None);
        }
        let session_id = inner.sessions[index].session.id.clone();
        let engram_mcp = engram_mcp_runtime_config_for_session_locked(inner, &session_id);
        let host_mcp_servers_only = session_is_acceptance_evaluator_child_locked(inner, &session_id);
        let host_shutdown = self.shutdown_signal_tx.borrow();
        if *host_shutdown { return Ok(None); }
        let mut started = self.start_turn_on_record(
            &mut inner.sessions[index],
            queued.pending_prompt.id.clone(), queued.pending_prompt.text.clone(),
            queued.attachments.clone(), queued.pending_prompt.expanded_text.clone(),
            queued.pending_prompt.source.clone(), engram_mcp, host_mcp_servers_only,
        ).map_err(|error| annotate_queued_followup_start_failure(
            inner, index, anyhow!("failed to dispatch queued prompt: {}", error.message),
        ))?;
        drop(host_shutdown);
        EngramHost::turn_started(inner, index);
        let record = &mut inner.sessions[index];
        mark_prompt_delivery_promoted(record, &queued, &mut started.dispatch,
            &started.message_delta.message_id, &self.server_instance_id);
        if queued.engram_bind.is_some() || queued.engram_evaluate.is_some()
            || queued.engram_interrupted || queued.engram_waiting
        {
            // Keep the original stored record as history after its own newly
            // proven-unsent content runs once. It cannot be selected again.
            let entry = &mut record.queued_prompts[position];
            entry.engram_ordinary_handoff = true;
            entry.promoted_message_index = Some(started.message_delta.message_index);
        } else {
            record.queued_prompts.remove(position);
        }
        sync_pending_prompts(record);
        started.message_delta.session_mutation_stamp = record.mutation_stamp;
        Ok(Some(started))
    }

    /// Kicks off a turn against the given `SessionRecord`, spawning or
    /// reusing the agent runtime as needed and returning the
    /// [`TurnDispatch`] that the caller wires into the runtime.
    ///
    /// This is the inner routine used once a caller has already located
    /// the record and decided that *this specific session* should run
    /// a new turn right now. Responsibilities:
    ///
    /// - Reject remote-proxy records (they have to go through the
    ///   remote backend — see `remote_routes.rs`).
    /// - Honor `record.runtime_reset_required` for Claude, Codex, and ACP:
    ///   kill or detach the existing handle, clear pending interactions,
    ///   and force a fresh session runtime on this turn.
    /// - Route to the right spawn helper based on `record.session.agent`:
    ///   `spawn_claude_runtime`, the shared Codex runtime (see
    ///   [`Self::shared_codex_runtime_for`]), or `spawn_acp_runtime`.
    /// - Stamp `active_turn_start_message_count` so SSE deltas can later
    ///   attribute subsequent messages to this turn.
    /// - Route expanded prompts (slash-command expansions) separately
    ///   from the user-visible `prompt` so the UI still shows the raw
    ///   command but the runtime sees the expanded text.
    ///
    /// Returns a [`TurnDispatch`] bundling the runtime handle + a
    /// RuntimeToken the caller stores on the record for the `_if_matches`
    /// guard path later.
    ///
    /// Every caller must call `EngramHost::turn_started` once this succeeds
    /// (it needs the whole state, which this record-level helper does not
    /// hold): it forgets the commands of the session's earlier turn and marks
    /// another session's open Engram checks in its worktree as overlapped.
    fn start_turn_on_record(
        &self,
        record: &mut SessionRecord,
        message_id: String,
        prompt: String,
        attachments: Vec<PromptImageAttachment>,
        expanded_prompt: Option<String>,
        source: Option<MessageSource>,
        engram_mcp: Option<EngramMcpRuntimeConfig>,
        host_mcp_servers_only: bool,
    ) -> std::result::Result<StartedTurn, ApiError> {
        if record.runtime_stop_in_progress {
            return Err(ApiError::conflict("session is stopping"));
        }
        if record.dedicated_predecessor_pending() {
            return Err(ApiError::conflict(
                "previous runtime cleanup remains pending",
            ));
        }
        if let Some(detail) = record.runtime.dedicated_cleanup_failure() {
            return Err(ApiError::conflict(format!("{detail}; Stop or Delete must finish cleanup before a new turn")));
        }
        match record.remote_proxy_identity() {
            Ok(None) => {}
            Ok(Some(_)) => {
                return Err(ApiError::internal(
                    "remote proxy sessions must dispatch through the remote backend",
                ));
            }
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "session has invalid remote proxy identity: {err:#}"
                )));
            }
        }

        let message_attachments = attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();
        record.active_turn_generation = record.active_turn_generation.wrapping_add(1).max(1);
        let active_turn_generation = record.active_turn_generation;
        // A successor never inherits the previous opening's presentation.
        record.active_turn_start_message_count = Some(record.session.messages.len());
        let expanded_prompt = expanded_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let runtime_prompt = expanded_prompt.clone().unwrap_or_else(|| prompt.clone());
        let runtime_prompt = if source
            .as_ref()
            .is_some_and(|source| source.is_peer_batch() || source.is_mailbox())
        {
            runtime_prompt
        } else {
            source
                .as_ref()
                .map(|source| peer_message_runtime_prompt(&runtime_prompt, source))
                .unwrap_or(runtime_prompt)
        };
        // A check an earlier turn left live with no end reported is closed
        // with outcome Unknown here, before the host line below is composed,
        // so this very prompt tells the holder (`EngramHost`).
        EngramHost::turn_prompt_composing(record);
        // A one-time host line about where the agent's turns on claimed work
        // are measured (`engram_source_roots.rs`): after the context fence
        // when one is delivered, before the prompt. It is cleared only once the
        // runtime accepts this prompt, and only if it is still the line this
        // prompt carried (`acknowledge_engram_source_root_line_delivery`), so a
        // dispatch refused or deferred after this point keeps it for the next,
        // and a line set meanwhile is not lost.
        let runtime_prompt = match record.engram.pending_source_root_line.as_deref() {
            Some(line) => {
                let runtime_prompt = format!("{line}\n\n{runtime_prompt}");
                record.engram.source_root_line_delivery =
                    Some((line.to_owned(), active_turn_generation));
                runtime_prompt
            }
            None => {
                record.engram.source_root_line_delivery = None;
                runtime_prompt
            }
        };
        let runtime_prompt = match record.engram.pending_context_nudge.as_deref() {
            Some(context) => {
                record.engram.context_nudge_delivery_generation =
                    Some(record.engram.context_nudge_generation);
                record.engram.context_nudge_delivery_turn_generation = Some(active_turn_generation);
                engram_context_runtime_prompt(context, &runtime_prompt)
            }
            None => {
                record.engram.context_nudge_delivery_generation = None;
                record.engram.context_nudge_delivery_turn_generation = None;
                runtime_prompt
            }
        };
        let mailbox_notification = source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .map(|mailbox| MailboxNotificationDelivery {
                mailbox_id: mailbox.mailbox_id.clone(),
                session_id: record.session.id.clone(),
                through_sequence: mailbox.sequence,
            });
        record.active_turn_mailbox_notification = mailbox_notification.clone();
        let dispatch = match record.session.agent {
            Agent::Claude => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        handle.kill_for_locked_reset().map_err(|err| {
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

                let handle = match &record.runtime {
                    SessionRuntime::Claude(handle) => handle.clone(),
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::None => {
                        let delegation_mcp_config = self
                            .termal_delegation_mcp_claude_config_json_with_engram(
                                &record.session.id,
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
                            engram_mcp.as_ref().map(|config| &config.stdio),
                            host_mcp_servers_only,
                            None,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Claude session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Claude(handle.clone());
                        record.register_dedicated_runtime();
                        record.engram_mcp_installed = engram_mcp.map(|config| config.installed);
                        handle
                    }
                };

                TurnDispatch::PersistentClaude {
                    human_prompt_id: None,
                    federation_handoff: None,
                    active_turn_generation,
                    opening_prompt: None,
                    command: ClaudePromptCommand {
                        attachments: attachments.clone(),
                        replay_generation: Uuid::new_v4().to_string(),
                        text: runtime_prompt.clone(),
                        turn_generation: active_turn_generation,
                    },
                    mailbox_notification: mailbox_notification.clone(),
                    runtime_token: record
                        .runtime
                        .runtime_token()
                        .expect("dispatched Claude turn must retain its runtime token"),
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            Agent::Codex => {
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
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::None => {
                        let profile = record.shared_codex_profile();
                        let handle = spawn_codex_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            profile,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Codex session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Codex(handle.clone());
                        record.attach_to_shared_codex_profile(profile);
                        handle
                    }
                };

                TurnDispatch::PersistentCodex {
                    human_prompt_id: None,
                    federation_handoff: None,
                    active_turn_generation,
                    opening_prompt: None,
                    service_tier: codex_fast_service_tier_value(
                        &record.session.model,
                        &record.session.model_options,
                        record.session.codex_fast_mode,
                    ),
                    command: CodexPromptCommand {
                        active_turn_generation,
                        approval_policy: record.codex_approval_policy,
                        attachments,
                        cwd: record.session.workdir.clone(),
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        reasoning_effort: record.codex_reasoning_effort,
                        // Resolved off the state lock before runtime delivery.
                        service_tier: None,
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                        // A receipted promotion sets it (prompt_delivery.rs).
                        delivery_receipt: None,
                    },
                    mailbox_notification: mailbox_notification.clone(),
                    runtime_token: record
                        .runtime
                        .runtime_token()
                        .expect("dispatched Codex turn must retain its runtime token"),
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            agent @ (Agent::Cursor | Agent::Gemini | Agent::OpenCode | Agent::Kimi) => {
                if !attachments.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support image attachments yet",
                        agent.name()
                    )));
                }

                if record.runtime_reset_required {
                    if let SessionRuntime::Acp(handle) = &record.runtime {
                        handle.kill_for_locked_reset().map_err(|err| {
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

                let expected_acp_agent = agent
                    .acp_runtime()
                    .ok_or_else(|| ApiError::internal("missing ACP runtime config"))?;
                let handle = match &record.runtime {
                    SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => {
                        handle.clone()
                    }
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
                        let handle = self
                            .start_acp_runtime_for_turn(
                                record.session.id.clone(),
                                record.session.workdir.clone(),
                                expected_acp_agent,
                                record.session.gemini_approval_mode,
                                engram_mcp.as_ref().map(|config| &config.stdio),
                            )
                            .map_err(|err| {
                                ApiError::internal(format!(
                                    "failed to start persistent {} session: {err:#}",
                                    agent.name()
                                ))
                            })?;
                        record.runtime = SessionRuntime::Acp(handle.clone());
                        record.register_dedicated_runtime();
                        record.engram_mcp_installed =
                            engram_mcp.as_ref().map(|config| config.installed.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentAcp {
                    human_prompt_id: None,
                    federation_handoff: None,
                    active_turn_generation,
                    opening_prompt: None,
                    command: AcpPromptCommand {
                        cwd: record.session.workdir.clone(),
                        cursor_mode: record.session.cursor_mode,
                        model: record
                            .session
                            .opencode_model
                            .clone()
                            .unwrap_or_else(|| record.session.model.clone()),
                        opencode_effort: record.session.opencode_effort.clone(),
                        opencode_mode: record.session.opencode_mode.clone(),
                        prompt: runtime_prompt.to_owned(),
                        resume_session_id: record.external_session_id.clone(),
                    },
                    mailbox_notification,
                    runtime_token: record
                        .runtime
                        .runtime_token()
                        .expect("dispatched ACP turn must retain its runtime token"),
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                    turn_lifecycle: handle.turn_lifecycle,
                }
            }
        };

        record.set_auto_dispatch_blocked(false);
        record.active_turn_file_changes.clear();
        record.active_turn_file_change_grace_deadline = None;
        let observation_source = record.queued_prompts.iter()
            .find(|head| head.pending_prompt.id == message_id)
            .map(|head| head.source)
            .unwrap_or(if source.as_ref().is_some_and(MessageSource::is_mailbox) {
                QueuedPromptSource::Mailbox
            } else {
                QueuedPromptSource::User
            });
        let message = Message::Text {
            attachments: message_attachments.clone(),
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::You,
            text: prompt.clone(),
            expanded_text: expanded_prompt,
            source,
        };
        let previous_promotion = record
            .queued_prompts
            .iter()
            .find(|queued| queued.pending_prompt.id == message_id)
            .and_then(|queued| queued.promoted_message_index);
        let message_index = previous_promotion.unwrap_or_else(|| {
            let local_index = cached_message_index_on_record(record, &message_id)
                .unwrap_or_else(|| push_message_on_record(record, message.clone()));
            global_message_index(record, local_index)
        });
        record.file_change_tracking = Some(Arc::new(FileChangeTrackingLifetime::new(Some((
            message_id.clone(),
            message_index,
        )))));
        if let Some(queued) = record
            .queued_prompts
            .front_mut()
            .filter(|queued| queued.pending_prompt.id == message_id)
        {
            queued.promoted_message_index = Some(message_index);
            queued.promotion_disposition_known = true;
        }
        record.session.live_activity = Some(SessionLiveActivity {
            prompt: prompt.clone(),
            command: None,
            command_status: None,
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = prompt_preview_text(&prompt, &message_attachments);
        record.provider_handoff_observations.prepared = Some(ProviderHandoffPrompt {
            prompt_id: message_id.clone(),
            source: observation_source,
            turn_generation: active_turn_generation,
        });
        let session_seq = record.next_body_delta_seq(&message_id);
        let message_delta = StartedTurnMessageDelta {
            session_id: record.session.id.clone(),
            message_id,
            message_index,
            message_count: session_message_count(record),
            message,
            preview: record.session.preview.clone(),
            status: record.session.status,
            session_mutation_stamp: record.mutation_stamp,
            session_seq,
        };

        Ok(StartedTurn {
            dispatch,
            message_delta,
        })
    }

    /// Re-kicks committed workflow prompts whose owning session landed back at
    /// [`SessionStatus::Idle`] before delivery, or retained
    /// [`SessionStatus::Error`] after a previous boot handoff was rejected.
    ///
    /// Delegation-wait resumes and orchestrator transitions both queue prompts
    /// with [`QueuedPromptSource::Orchestrator`] after consuming their durable
    /// trigger. A crash between that commit and runtime delivery would otherwise
    /// strand the workflow permanently. Ordinary user prompts remain a strict
    /// barrier because restart is not their activation event. Boot-recovered
    /// mailbox wakes may precede the workflow prompt because recovery inserts
    /// them at the queue front; in that mixed case the already-committed workflow
    /// activation drains the mailbox wake first without overtaking it.
    /// A user prompt before the workflow entry remains a strict FIFO barrier,
    /// so restart leaves the entire queue dormant until a genuine activation.
    fn dispatch_orphaned_workflow_prompts(&self) {
        let session_ids: Vec<String> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    record.is_local_session()
                        && matches!(
                            record.session.status,
                            SessionStatus::Idle | SessionStatus::Error
                        )
                        && queue_has_orphaned_workflow(record)
                        && !record.orchestrator_auto_dispatch_blocked
                        && matches!(record.runtime, SessionRuntime::None)
                })
                .map(|record| record.session.id.clone())
                .collect()
        };

        for session_id in session_ids {
            match self.start_orphaned_workflow_turn(&session_id) {
                Ok(Some(started)) => match deliver_turn_dispatch(self, started.dispatch) {
                    TurnDispatchDeliveryOutcome::Rejected(err) => {
                        eprintln!(
                            "startup> failed dispatching orphaned workflow prompt for `{session_id}`: {}",
                            err.message
                        );
                        match started.queued.source {
                            QueuedPromptSource::Mailbox => {
                                self.retry_orphaned_workflow_after_rejected_mailbox_wake(
                                    &session_id,
                                );
                            }
                            QueuedPromptSource::Orchestrator => {
                                if let Err(requeue_err) = self
                                    .requeue_rejected_orphaned_workflow_prompt(
                                        &session_id,
                                        started.queued,
                                    )
                                {
                                    eprintln!(
                                        "startup> failed restoring rejected orphaned workflow prompt for `{session_id}`: {requeue_err:#}"
                                    );
                                }
                            }
                            QueuedPromptSource::User | QueuedPromptSource::Human => {}
                        }
                    }
                    TurnDispatchDeliveryOutcome::Held { error: Some(error) } => eprintln!(
                        "startup> retained orphaned workflow prompt for `{session_id}` after uncertain persistence: {}",
                        error.message
                    ),
                    TurnDispatchDeliveryOutcome::Delivered
                    | TurnDispatchDeliveryOutcome::Scheduled
                    | TurnDispatchDeliveryOutcome::Held { error: None }
                    | TurnDispatchDeliveryOutcome::Superseded => {}
                },
                Ok(None) => {}
                Err(err) => {
                    eprintln!(
                        "startup> failed dispatching orphaned workflow prompt for `{session_id}`: {err:#}"
                    );
                }
            }
        }
    }

    /// Atomically revalidates and promotes one queue head that is owned by a
    /// committed workflow activation.
    ///
    /// The preliminary scan in [`Self::dispatch_orphaned_workflow_prompts`] is
    /// only an optimization. Queue shape, status, runtime ownership, and prompt
    /// promotion are checked together under the state lock here so a concurrent
    /// activation cannot turn the startup pass into a second live dispatch.
    fn start_orphaned_workflow_turn(
        &self,
        session_id: &str,
    ) -> Result<Option<OrphanedWorkflowDispatch>> {
        if let Err(err) = self.reconcile_never_woken_mailbox_notifications_for_session(session_id) {
            eprintln!(
                "mailbox> failed reconciling notifications before workflow recovery for `{session_id}`: {err:#}"
            );
        }
        if let Err(error) = self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id) {
            return consume_mailbox_relay_hold(Err(error));
        }

        let Some(started) = self.start_next_queued_turn_off_lock(session_id, false, true)? else {
            return Ok(None);
        };
        Ok(Some(OrphanedWorkflowDispatch {
            dispatch: started.dispatch,
            queued: started.queued,
        }))
    }

    /// Restores a workflow prompt whose runtime channel rejected the boot
    /// handoff. The new message id prevents the eventual retry from duplicating
    /// the failed turn's transcript id, while `push_front` preserves the
    /// workflow's original ordering ahead of prompts queued concurrently.
    fn requeue_rejected_orphaned_workflow_prompt(
        &self,
        session_id: &str,
        mut queued: QueuedPromptRecord,
    ) -> Result<()> {
        if queued.source != QueuedPromptSource::Orchestrator {
            return Ok(());
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        // A receipted prompt comes back only under a new prompt id held for a
        // new durability acknowledgement (prompt_delivery.rs).
        if inner.sessions[index]
            .prompt_deliveries
            .get(&queued.pending_prompt.id)
            .is_some_and(|delivery| delivery.disposition == PromptDeliveryDisposition::Rejected)
        {
            drop(inner);
            self.requeue_rejected_prompt_delivery_at(session_id, &queued.pending_prompt.id, true)?;
            return Ok(());
        }
        queued.pending_prompt.id = inner.next_message_id();
        queued.pending_prompt.timestamp = stamp_now();
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        record.queued_prompts.push_front(queued);
        sync_pending_prompts(record);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Makes one bounded retry when a stale runtime rejects a boot-recovered
    /// mailbox wake that is gating an already-committed workflow activation.
    ///
    /// Rejection clears the dead runtime, records the failed turn, and restores
    /// the exact mailbox wake at the queue front. Without a retry, the session
    /// remains `Error` and the durable workflow prompt behind that wake has no
    /// remaining activation event. This helper deliberately retries only this
    /// mixed Mailbox -> Orchestrator shape and only once; another rejection
    /// remains visible for a user or later restart instead of recursively
    /// respawning a broken runtime.
    fn retry_orphaned_workflow_after_rejected_mailbox_wake(&self, session_id: &str) {
        match self.start_orphaned_workflow_turn(session_id) {
            Ok(Some(started)) => match deliver_turn_dispatch(self, started.dispatch) {
                TurnDispatchDeliveryOutcome::Rejected(err) => {
                    eprintln!(
                        "startup> failed retrying recovered mailbox wake before orphaned workflow for `{session_id}`: {}",
                        err.message
                    );
                    if started.queued.source == QueuedPromptSource::Orchestrator {
                        if let Err(requeue_err) = self
                            .requeue_rejected_orphaned_workflow_prompt(session_id, started.queued)
                        {
                            eprintln!(
                                "startup> failed restoring workflow prompt rejected during mailbox recovery retry for `{session_id}`: {requeue_err:#}"
                            );
                        }
                    }
                }
                TurnDispatchDeliveryOutcome::Held { error: Some(error) } => eprintln!(
                    "startup> retained recovered mailbox/workflow prompt for `{session_id}` after uncertain persistence: {}",
                    error.message
                ),
                TurnDispatchDeliveryOutcome::Delivered
                | TurnDispatchDeliveryOutcome::Scheduled
                | TurnDispatchDeliveryOutcome::Held { error: None }
                | TurnDispatchDeliveryOutcome::Superseded => {}
            },
            Ok(None) => {}
            Err(err) => {
                eprintln!(
                    "startup> failed preparing recovered mailbox wake retry before orphaned workflow for `{session_id}`: {err:#}"
                );
            }
        }
    }

    /// Pops the front of the session's queued-prompt FIFO and
    /// dispatches it when the session is idle.
    ///
    /// Called from every path that transitions a session to `Idle`:
    /// `finish_turn_ok`, `mark_turn_error`, `handle_runtime_exit`, and
    /// the approval-submission paths that unblock a turn. It is also
    /// used by explicit post-restart activation paths and selective
    /// workflow crash recovery. Guards:
    ///
    /// - Does nothing if the queue is empty.
    /// - Respects `orchestrator_auto_dispatch_blocked` so orchestrator
    ///   parents can manually gate when queued turns run.
    ///
    /// On success, promotes the queue head to an active turn and calls
    /// [`Self::dispatch_turn`] to actually hand it to the agent runner.
    fn dispatch_next_queued_turn(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
    ) -> Result<Option<TurnDispatch>> {
        Ok(self
            .dispatch_next_queued_turn_with_prompt(session_id, allow_blocked_dispatch)?
            .map(|started| started.dispatch))
    }

    /// Keeps the prompt actually admitted after mailbox revalidation, so a
    /// Resume can report a delivery hold against that prompt's identity.
    fn dispatch_next_queued_turn_with_prompt(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
    ) -> Result<Option<StartedQueuedTurn>> {
        if let Err(err) = self.reconcile_never_woken_mailbox_notifications_for_session(session_id) {
            eprintln!(
                "mailbox> failed reconciling notifications before queue drain for `{session_id}`: {err:#}"
            );
        }
        if let Err(error) = self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id) {
            return consume_mailbox_relay_hold(Err(error));
        }
        self.start_next_queued_turn_off_lock(session_id, allow_blocked_dispatch, false)
    }

    fn dispatch_next_queued_turn_for_mailbox_relay(
        &self, session_id: &str, owner: MailboxRelayDrainOwner,
    ) -> Result<Option<TurnDispatch>> {
        self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id)?;
        Ok(self.start_next_queued_turn_off_lock_for_owner(
            session_id, false, false, Some(QueuedDrainOwner::MailboxRelay(owner)),
        )?.map(|started| started.dispatch))
    }

    fn dispatch_next_queued_turn_for_activation(&self, session_id: &str,
        owner: QueuedTurnOwner) -> Result<Option<TurnDispatch>> {
        self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id)?;
        Ok(self.start_next_queued_turn_off_lock_for_owner(
            session_id, false, false, Some(QueuedDrainOwner::Activation(owner)),
        )?.map(|started| started.dispatch))
    }

    /// An explicit Resume bound to the queue head its caller checked (a held
    /// delegation's retained prompt): it bypasses the paused queue only for
    /// `owner`, so a successor exposed by a concurrent cancellation is never
    /// admitted in its place.
    fn dispatch_next_queued_turn_for_resume(
        &self,
        session_id: &str,
        owner: QueuedTurnOwner,
    ) -> Result<Option<TurnDispatch>> {
        Ok(self
            .dispatch_next_queued_turn_for_resume_with_prompt(session_id, owner)?
            .map(|started| started.dispatch))
    }

    fn dispatch_next_queued_turn_for_resume_with_prompt(
        &self,
        session_id: &str,
        owner: QueuedTurnOwner,
    ) -> Result<Option<StartedQueuedTurn>> {
        if let Err(err) = self.reconcile_never_woken_mailbox_notifications_for_session(session_id) {
            eprintln!(
                "mailbox> failed reconciling notifications before the bound resume for                  `{session_id}`: {err:#}"
            );
        }
        self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id)?;
        self.start_next_queued_turn_off_lock_for_owner(
            session_id,
            true,
            false,
            Some(QueuedDrainOwner::Resume(owner)),
        )
    }

    /// Evaluates and promotes one stable queue head. The prompt snapshot and
    /// generation are captured under the state mutex; all Engram work happens
    /// after releasing it; the same queue head/generation is then revalidated
    /// before `start_turn_on_record` mutates the session.
    fn start_next_queued_turn_off_lock(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
        orphaned_workflow_only: bool,
    ) -> Result<Option<StartedQueuedTurn>> {
        self.start_next_queued_turn_off_lock_for_owner(
            session_id,
            allow_blocked_dispatch,
            orphaned_workflow_only,
            None,
        )
    }

    /// `start_next_queued_turn_off_lock`; with an `owner`, the paused-queue
    /// bypass is that owner's alone.
    fn start_next_queued_turn_off_lock_for_owner(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
        orphaned_workflow_only: bool,
        owner: Option<QueuedDrainOwner>,
    ) -> Result<Option<StartedQueuedTurn>> {
        if let Some(QueuedDrainOwner::Resume(expected) | QueuedDrainOwner::Activation(expected)) = &owner {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id)
                .is_none_or(|index| !expected.matches(&inner.sessions[index])) {
                return Ok(None);
            }
        }
        let mut selected_human = None;
        let human_owner = match owner.as_ref() {
            None => true,
            Some(QueuedDrainOwner::Resume(owner) | QueuedDrainOwner::Activation(owner)) => {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.find_session_index(session_id).is_some_and(|index| {
                    let record = &inner.sessions[index];
                    let eligible = owner.matches(record) && dispatch_queue_position(&inner, index)
                        .and_then(|position| record.queued_prompts.get(position)).is_some_and(|head|
                        head.source == QueuedPromptSource::Human);
                    if eligible { selected_human = HumanResumeOwner::capture(record, owner.clone()); }
                    selected_human.is_some()
                })
            }
            _ => false,
        };
        if human_owner && !orphaned_workflow_only {
            #[cfg(test)]
            let _drain_completion = selected_human.as_ref().and_then(|_| run_test_human_drain_capture_hook(self, session_id));
            // Ordinary/peer Send also uses allow_blocked_dispatch for its own
            // legacy queue policy. It is not an explicit Human Resume. Only
            // the exact receipted Resume owner may carry that pause bypass;
            // explicit Human UI actions call their own promotion path.
            let human_resume = allow_blocked_dispatch
                && matches!(owner.as_ref(), Some(QueuedDrainOwner::Resume(_)));
            let drained = self.start_human_queued_turn_with_selection(session_id, human_resume, selected_human.as_mut());
            let drained = match drained {
                Err(error) if selected_human.is_some() && error.downcast_ref::<HumanPromptOwnerChanged>().is_some() => return Ok(None),
                other => other?,
            };
            match drained {
                HumanPromptDrain::Started(started) => return Ok(Some(started)),
                HumanPromptDrain::Held => return Ok(None),
                HumanPromptDrain::Absent => {}
            }
        }
        if let Some(QueuedDrainOwner::Activation(owner)) = &owner {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(session_id).is_none_or(|index| {
                let record = &inner.sessions[index];
                !owner.matches(record) || record.orchestrator_auto_dispatch_blocked
                    || matches!(record.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
                    || record.runtime_stop_in_progress
                    || session_resource_reset_in_progress(&inner, record)
            }) { return Ok(None) }
        }
        // Where the session writes, for the overlap marks its turn start
        // makes under the lock.
        self.engram_host().turn_starting(session_id);
        let result = self.start_next_queued_turn_inner_off_lock(
            session_id, allow_blocked_dispatch, orphaned_workflow_only, owner,
        );
        let result = self.settle_unstarted_queued_turn(consume_mailbox_relay_hold(result));
        // A promotion that started nothing may have left a delegation child
        // held (a failed promotion, a parked queued admission); a started turn
        // reaches the delivery path, which refreshes it there.
        if !matches!(result, Ok(Some(_))) {
            self.sync_delegation_attempt_for_child_session(session_id);
        }
        result
    }

    fn settle_unstarted_queued_turn(
        &self,
        result: Result<Option<StartedQueuedTurn>>,
    ) -> Result<Option<StartedQueuedTurn>> {
        if let Err(error) = &result {
            if let Some(failure) = error.downcast_ref::<QueuedFollowupStartFailure>() {
                if let Err(cleanup) =
                    self.settle_queued_followup_start_failure(failure, &format!("{error:#}"))
                {
                    return Err(anyhow!(
                        "{error:#}; failed to settle queued follow-up: {cleanup:#}"
                    ));
                }
            }
        }
        result
    }

    // Direct dispatch already committed the enqueue before promoting its head.
    // A failed second commit must restore that durable queue even when the
    // original follow-up request (and its reservation) has long since returned.
    fn start_and_commit_queued_dispatch_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        followup: Option<&DelegationRecord>,
        prompt_id: &str,
    ) -> Result<Option<(u64, StartedTurn)>> {
        // A head held for its durability acknowledgement keeps the queue,
        // and the prompt just committed behind it, queued; the
        // acknowledgement dispatches them (queued_prompt_durability.rs).
        if queued_head_held_for_durability(&inner.sessions[index]) {
            return Ok(None);
        }
        let Some(position) = ordinary_queue_position(&inner.sessions[index]) else { return Ok(None); };
        let queued = inner.sessions[index].queued_prompts[position].clone();
        if queued.source == QueuedPromptSource::Human { return Ok(None); }
        let snapshot = inner.sessions[index].capture_queue_promotion_snapshot();
        let receipted_head = inner.sessions[index]
            .prompt_deliveries
            .contains_key(&queued.pending_prompt.id);
        let Some(started) = self
            .start_next_queued_turn_locked(inner, index, true)
            .context("failed to start queued turn")?
        else {
            // A receipted head waits for its write-ahead hand-off record, and
            // the queue with it: the caller wakes the ordinary drain once the
            // lock is released, which begins that record, and its
            // acknowledgement dispatches them (prompt_delivery.rs).
            if receipted_head {
                return Ok(None);
            }
            return Err(anyhow!("queued prompt disappeared before dispatch"));
        };
        let session_id = inner.sessions[index].session.id.clone();
        match self.commit_followup_prompt_locked_for_session(inner, followup, prompt_id, false, Some(&session_id)) {
            Ok(revision) => Ok(Some((revision, started))),
            Err(error) => {
                // A live follow-up admission has its own failure owner. For
                // ordinary dispatch restore first, so the typed settlement
                // still recognizes the exact not-yet-delivered attempt.
                if followup.is_none() {
                    inner
                        .session_mut_by_index(index)
                        .expect("validated session")
                        .restore_queue_promotion(
                            snapshot,
                            queued,
                            &started.message_delta.message_id,
                        );
                    // commit_persisted_delta_locked consumed this revision
                    // before its synchronous fallback failed. Persist the
                    // restored record without consuming another revision, then
                    // publish that projection so clients do not remain on the
                    // provisional Active/transcript state.
                    let restore_error = self.persist_internal_locked(&inner).err();
                    self.publish_state_locked(&inner);
                    let error = match restore_error {
                        Some(restore_error) => anyhow!(
                            "{error:#}; failed to persist restored queued promotion: {restore_error:#}"
                        ),
                        None => error,
                    };
                    return Err(annotate_queued_followup_start_failure(
                        inner,
                        index,
                        error.context("failed to persist the promoted queue head"),
                    ));
                }
                Err(error)
            }
        }
    }

    // Implementation detail: all callers use the wrapper above so tagged
    // delegation failures are settled before returning to queue drainers.
    fn start_next_queued_turn_inner_off_lock(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
        orphaned_workflow_only: bool,
        mut owner: Option<QueuedDrainOwner>,
    ) -> Result<Option<StartedQueuedTurn>> {
        // Only an automatic, non-bypassing drain treats an existing hold as
        // a successful no-op. Explicit owners keep their original errors.
        let automatic_queue = (owner.is_none() && !allow_blocked_dispatch).then_some(false);
        let mut automatic_owner = automatic_queue.and_then(|_| {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.find_session_index(session_id)
                .and_then(|index| QueuedTurnOwner::capture(&inner.sessions[index]))
        });
        let ordinary_owner = match owner.as_mut() {
            Some(QueuedDrainOwner::Resume(owner) | QueuedDrainOwner::Activation(owner)) => Some(owner),
            _ => automatic_owner.as_mut(),
        };
        if matches!(self.prepare_dedicated_runtime_reset_for_ordinary_queue_off_lock(
            session_id, ordinary_owner, automatic_queue,
        )?, RuntimeResetPreparation::Held) {
            return Ok(None);
        }
        let activation_owner = match &owner {
            Some(QueuedDrainOwner::Activation(owner)) => Some(owner.clone()),
            _ => None,
        };
        let live_owner = match &owner {
            Some(QueuedDrainOwner::FederationWake(owner)) => Some(owner.clone()),
            _ => None,
        };
        let relay_owner = match &owner {
            Some(QueuedDrainOwner::MailboxRelay(owner)) => Some(owner.clone()),
            _ => None,
        };
        let bypass_owner = match owner {
            Some(QueuedDrainOwner::Resume(owner)) => Some(owner),
            Some(_) => None,
            None => allow_blocked_dispatch.then(|| {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.find_session_index(session_id)
                    .and_then(|index| QueuedTurnOwner::capture(&inner.sessions[index]))
            }).flatten(),
        };
        let mut context_preparation = self.prepare_engram_context_nudge_off_lock(session_id);
        // Base-tier context refresh is the sole external operation before this
        // queue-drain path. With base integration absent or disabled, the
        // pre-Engram zero-transport behavior remains intact.
        {
            let relay_authority = self.prepare_front_mailbox_relay_authority(session_id).map_err(|error|mailbox_relay_admission_held(error.message))?;
            let mut context_admission_retries = 0;
            let (_relay_link, mut inner) = loop {
                let relay_link = relay_authority.as_ref().map(|(_, authority)| authority.lock_link()).transpose()
                    .map_err(mailbox_relay_admission_held)?;
                let inner = self.inner.lock().expect("state mutex poisoned");
                let context_refresh_raced_prompt_admission =
                    matches!(context_preparation, EngramContextNudgePreparation::Ready)
                        && inner.find_session_index(session_id).is_some_and(|index| {
                            inner.sessions[index].engram.context_needs_preparation()
                        });
                if !context_refresh_raced_prompt_admission {
                    break (relay_link, inner);
                }
                context_admission_retries += 1;
                if context_admission_retries >= ENGRAM_CONTEXT_PROMPT_ADMISSION_RETRIES {
                    eprintln!(
                        "engram> session={session_id} context refresh kept racing queued prompt admission; dispatching without further delay"
                    );
                    break (relay_link, inner);
                }
                drop(inner);
                drop(relay_link);
                context_preparation = self.prepare_engram_context_nudge_off_lock(session_id);
            };
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(None);
            };
            self.validate_front_mailbox_relay_authority_locked(&inner, session_id, relay_authority.as_ref())
                .map_err(|error|mailbox_relay_admission_held(error.message))?;
            if live_owner.as_ref().is_some_and(|owner| !owner.matches(&inner.sessions[index])) {
                return Ok(None);
            }
            if relay_owner.as_ref().is_some_and(|owner| !owner.matches(&inner.sessions[index])) {
                return Ok(None);
            }
            if activation_owner.as_ref().is_some_and(|owner| !owner.matches(&inner.sessions[index])) {
                return Ok(None);
            }
            if automatic_owner.as_ref().is_some_and(|owner| !owner.matches(&inner.sessions[index])) {
                return Ok(None);
            }
            if allow_blocked_dispatch
                && !bypass_owner
                    .as_ref()
                    .is_some_and(|owner| owner.matches(&inner.sessions[index]))
            {
                return Ok(None);
            }
            let record = &inner.sessions[index];
            if ordinary_queue_dispatch_is_held(&inner, record, allow_blocked_dispatch) {
                return Ok(None);
            }
            if orphaned_workflow_only
                && !(record.is_local_session()
                    && matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
                    && matches!(record.runtime, SessionRuntime::None)
                    && queue_has_orphaned_workflow(record))
            {
                return Ok(None);
            }
        let Some(queued) = dispatch_queue_position(&inner, index)
            .and_then(|position| record.queued_prompts.get(position)).cloned() else {
            return Ok(None);
        };
        // Write-ahead acknowledgement belongs to this exact prompt;
        // a held entry never writes or consumes a handoff.
        if !queued_prompt_held_for_durability(record, &queued)
            && !self.prompt_delivery_gate_locked(
                &mut inner,
                index,
                &queued.pending_prompt.id,
                false,
            )
        {
            return Ok(None);
        }
        let promotion_snapshot = inner.sessions[index].capture_queue_promotion_snapshot();
        let Some(started) = self.start_next_queued_turn_locked(
            &mut inner,
            index,
            allow_blocked_dispatch,
        )?
        else {
            return Ok(None);
        };
        let revision = match self.commit_persisted_session_delta_locked(&mut inner, session_id) {
            Ok(revision) => revision,
            Err(err) => {
                // The promotion is only real once it is durable. Put
                // the record back so the head, the latch, and the
                // transcript match what the runtime actually saw:
                // nothing.
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .restore_queue_promotion(
                        promotion_snapshot,
                        queued,
                        &started.message_delta.message_id,
                    );
                let restore_error = self.persist_internal_locked(&inner).err();
                self.publish_state_locked(&inner);
                let err = match restore_error {
                    Some(restore_error) => anyhow!(
                        "{err:#}; failed to persist restored queued promotion: {restore_error:#}"
                    ),
                    None => err,
                };
                return Err(annotate_queued_followup_start_failure(
                    &inner,
                    index,
                    err.context("failed to persist the promoted queue head"),
                ));
            }
        };
        let StartedTurn {
            dispatch,
            message_delta,
        } = started;
        self.publish_started_turn_message_delta(&inner, revision, message_delta);
        drop(inner);
        return Ok(Some(StartedQueuedTurn { dispatch, queued }));
        }
    }

    fn dispatch_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        let _acceptance_guard = self.reserve_direct_requester_acceptance_prompt(session_id, &request)?;
        self.dispatch_turn_with_followup(session_id, request, None)
    }

    fn dispatch_turn_with_followup(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        followup: Option<&DelegationRecord>,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        self.dispatch_or_queue_turn_with_followup(session_id, request, followup)
    }

    fn dispatch_or_queue_turn_with_followup(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        followup: Option<&DelegationRecord>,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        // Where the session writes, for the overlap marks its turn start
        // makes under the lock.
        self.engram_host().turn_starting(session_id);
        if self.remote_session_target(session_id)?.is_some() {
            if followup.is_some() {
                return Err(ApiError::conflict(
                    "remote delegation follow-up is not supported",
                ));
            }
            self.proxy_remote_turn_dispatch(session_id, request)?;
            return Ok(DispatchTurnResult::Queued);
        }
        let mut prompt = request.text.trim().to_owned();
        let mut expanded_prompt = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }
        // Background/direct dispatch must not restore a terminal child before
        // rejecting it. Only the explicit follow-up reservation can restore a
        // terminal child here. Recheck its ownership at prompt admission too.
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner
                .find_visible_session_index(session_id)
                .is_some_and(|index| delegated_child_dispatch_is_blocked_locked(&inner, index))
                && !reserved_followup_is_admissible_locked(&inner, session_id, followup)
            {
                return Err(ApiError::conflict(DELEGATION_NO_LONGER_STARTABLE_MESSAGE));
            }
        }
        let review_recut = self.capture_followup_review_recut(followup)?;
        self.prepare_codex_child_followup(session_id)?;
        self.refresh_followup_review_custody(review_recut.as_ref())?;
        self.prepare_dedicated_runtime_reset_off_lock(session_id, false)
            .map_err(|error| {
                ApiError::conflict(format!(
                    "failed preparing dedicated runtime reset: {error:#}"
                ))
            })?;
        if request.source_mailbox.is_none() {
            if let Err(err) =
                self.reconcile_never_woken_mailbox_notifications_for_session(session_id)
            {
                eprintln!(
                    "mailbox> failed reconciling notifications before turn start for `{session_id}`: {err:#}"
                );
            }
        }
        let relay_before_revalidation = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.find_session_index(session_id).and_then(|index| {
                let record = &inner.sessions[index];
                record.queued_prompts.front().and_then(|head| head.pending_prompt.source.as_ref())
                    .and_then(|source| source.mailbox.as_ref())
                    .filter(|source| source.mailbox_id.starts_with("relay-mailbox-"))
                    .and_then(|_| QueuedTurnOwner::capture(record))
            })
        };
        let mut refused_relay_head = match self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id) {
            Ok(()) => None,
            Err(error) if request.source_mailbox.is_none() => {
                eprintln!("native relay> preserving fenced carrier during ordinary prompt admission: {error:#}");
                relay_before_revalidation
            }
            Err(error) => return Err(mailbox_api_error(error)),
        };
        #[cfg(test)]
        if let Some(hook) = TEST_MAILBOX_RELAY_AFTER_SEND_REVALIDATION.with(|hook| hook.borrow_mut().take()) { hook(); }
        let mut context_preparation = self.prepare_engram_context_nudge_off_lock(session_id);
        let mut context_admission_retries = 0;
        // Capture outside StateInner, then hold link -> state through all
        // direct promotions. A pre-context proof alone cannot authorize them.
        let relay_authority = self.prepare_front_mailbox_relay_authority(session_id);
        let (relay_link, mut inner) = loop {
            let relay_link = relay_authority.as_ref().ok().and_then(|authority|authority.as_ref())
                .map(|(_,authority)| authority.lock_link()).transpose();
            let inner = self.inner.lock().expect("state mutex poisoned");
            let context_refresh_raced_prompt_admission =
                matches!(context_preparation, EngramContextNudgePreparation::Ready)
                    && inner
                        .find_visible_session_index(session_id)
                        .is_some_and(|index| {
                            inner.sessions[index].engram.context_needs_preparation()
                        });
            if !context_refresh_raced_prompt_admission {
                break (relay_link, inner);
            }
            context_admission_retries += 1;
            if context_admission_retries >= ENGRAM_CONTEXT_PROMPT_ADMISSION_RETRIES {
                eprintln!(
                    "engram> session={session_id} context refresh kept racing prompt admission; dispatching without further delay"
                );
                break (relay_link, inner);
            }
            drop(inner);
            drop(relay_link);
            context_preparation = self.prepare_engram_context_nudge_off_lock(session_id);
        };
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let late_relay_refused = relay_authority.is_err() || relay_link.is_err()
            || self.validate_front_mailbox_relay_authority_locked(
                &inner, session_id, relay_authority.as_ref().ok().and_then(|authority|authority.as_ref()),
            ).is_err();
        if late_relay_refused {
            // Qualify the refusal against the exact current metadata carrier;
            // the existing ordinary-user path keeps it and preserves FIFO.
            if inner.sessions[index].queued_prompts.front()
                .and_then(|queued|queued.pending_prompt.source.as_ref())
                .and_then(|source|source.mailbox.as_ref())
                .is_some_and(|source|source.mailbox_id.starts_with("relay-mailbox-")) {
                refused_relay_head = QueuedTurnOwner::capture(&inner.sessions[index]);
            }
        }
        if delegated_child_dispatch_is_blocked_locked(&inner, index)
            && !reserved_followup_is_admissible_locked(&inner, session_id, followup)
        {
            return Err(ApiError::conflict(DELEGATION_NO_LONGER_STARTABLE_MESSAGE));
        }

        if inner.sessions[index]
            .codex_delegation_release
            .as_ref()
            .is_some_and(|release| {
                release
                    .outcome
                    .lock()
                    .expect("Codex release mutex poisoned")
                    .is_none()
            })
        {
            return Err(ApiError::conflict(
                "Codex thread cleanup started during prompt admission; retry the prompt",
            ));
        }

        // Resolve peer-sender attribution (`termal_send_to_session`) to the
        // sender's current display name while we hold the state lock. The
        // backend owns this lookup so a caller cannot spoof another session's
        // name. An unknown id (e.g. a sender deleted before a queued prompt
        // drains) drops to `None`, so the message simply stays "You".
        let source = request.source_session_id.as_deref().and_then(|source_id| {
            inner.find_session_index(source_id).map(|source_index| {
                let source_name = inner.sessions[source_index].session.name.clone();
                match request.source_mailbox.clone() {
                    Some(mailbox) => {
                        MessageSource::mailbox(source_id.to_owned(), source_name, mailbox)
                    }
                    None => MessageSource::peer(source_id.to_owned(), source_name),
                }
            }).or_else(|| {
                let mailbox=request.source_mailbox.clone()?;
                let participant=inner.federation.mailbox_directory.get(source_id)?;
                Some(MessageSource::mailbox(source_id.to_owned(),participant.name.clone(),mailbox))
            })
        });
        // Only a genuine user turn replaces automatic capacity recovery.
        if inner.sessions[index].codex_terminal_identity_unresolved {
            return Err(ApiError::conflict(
                "Codex terminal identity is unresolved; Stop before sending another prompt",
            ));
        }
        // Peer/mailbox provenance is resolved above; their wakes stay queued.
        if source.is_none() && request.source_mailbox.is_none()
            && clear_provider_capacity_retry(&mut inner.sessions[index])
        {
            inner.stamp_session_at_index(index);
        }
        if record_has_archived_codex_thread(&inner.sessions[index]) {
            return Err(ApiError::conflict(
                "the current Codex thread is archived; unarchive it before sending another prompt",
            ));
        }
        if followup.is_some() {
            validate_delegation_followup_child_locked(&inner, session_id, !attachments.is_empty())?;
            if !reserved_followup_is_admissible_locked(&inner, session_id, followup) {
                return Err(ApiError::conflict(
                    "delegation changed during follow-up admission",
                ));
            }
            // This lock is held until the delegation is rearmed below, so an
            // evaluator request cannot create a second active evaluator of the
            // same task in between, nor can this rearm follow one.
            if let Some(previous) = followup {
                self.validate_followup_review_custody_locked(&inner, previous)?;
                acceptance_evaluation_followup_admission_locked(&inner, previous)?;
            }
            // Keep the previous result until a prompt is started or queued.
            // Runtime creation below is fallible, even with this lock held.
        }
        if let Some(template_session) =
            orchestrator_template_session_for_runtime_session(&inner, session_id)
        {
            let rendered_prompt = build_orchestrator_destination_prompt(
                &inner.sessions[index],
                &template_session.instructions,
                expanded_prompt.as_deref().unwrap_or(&prompt),
            );
            if expanded_prompt.is_some() {
                prompt = rendered_prompt.clone();
                expanded_prompt = Some(rendered_prompt);
            } else {
                prompt = rendered_prompt;
            }
        }
        let session_is_busy = matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        ) || inner.sessions[index].runtime_stop_in_progress
            || inner.sessions[index].engram.observation_delete_requested
            || session_resource_reset_in_progress(&inner, &inner.sessions[index]);
        let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();
        let blocked_queue_contains_user_prompt = inner.sessions[index]
            .queued_prompts
            .iter()
            .any(|queued| ordinary_queue_entry_is_runnable(&inner.sessions[index], queued)
                && matches!(queued.source, QueuedPromptSource::User | QueuedPromptSource::Human));
        let orchestrator_auto_dispatch_blocked =
            inner.sessions[index].orchestrator_auto_dispatch_blocked;
        let paused_human_queue = orchestrator_auto_dispatch_blocked
            && inner.sessions[index].queued_prompts.iter()
                .any(|queued| ordinary_queue_entry_is_runnable(&inner.sessions[index], queued)
                    && queued.source == QueuedPromptSource::Human);
        let queued_prompt_source = if source
            .as_ref()
            .is_some_and(|message_source| message_source.is_mailbox())
        {
            QueuedPromptSource::Mailbox
        } else {
            QueuedPromptSource::User
        };
        let capacity_automatic_prompt = inner.sessions[index].provider_capacity_retry.is_some()
            && (source.is_some() || request.source_mailbox.is_some());
        // Stop leaves a persisted explicit-resume latch. Routine mailbox
        // notifications must join the durable queue even when it was empty at
        // the moment of Stop; only a later user prompt may lift the latch.
        let blocked_automatic_prompt = orchestrator_auto_dispatch_blocked
            && (queued_prompt_source == QueuedPromptSource::Mailbox
                || capacity_automatic_prompt || paused_human_queue);

        let mut message_id = inner.next_message_id();
        let still_refused = refused_relay_head.as_ref().is_some_and(|owner| owner.matches(&inner.sessions[index]));
        let record = &inner.sessions[index];
        let bypass_refused_carrier = still_refused && !session_is_busy
            && !capacity_automatic_prompt && record.provider_capacity_retry.is_none()
            && queued_prompt_source == QueuedPromptSource::User
            && !queued_head_held_for_durability(record)
            && record.queued_prompts.front().is_some_and(|head|
                ordinary_queue_entry_is_runnable(record, head));
        // Scheduling isolation only: use the existing explicit-user priority
        // paths without changing the persisted Stop/pause latch. The exact
        // refused carrier remains queued; ordinary-user FIFO is preserved.
        let orchestrator_auto_dispatch_blocked = orchestrator_auto_dispatch_blocked || bypass_refused_carrier;
        let manual_can_overtake = bypass_refused_carrier || (!session_is_busy && orchestrator_auto_dispatch_blocked
            && !blocked_queue_contains_user_prompt && !capacity_automatic_prompt
            && queued_prompt_source == QueuedPromptSource::User
            && !queued_head_held_for_durability(&inner.sessions[index])
            && !inner.sessions[index].queued_prompts.front().is_some_and(|head|
                !ordinary_queue_entry_is_runnable(&inner.sessions[index], head)));
        if still_refused && !manual_can_overtake {
            // Admission of ordinary work is separate from authority to promote
            // the retained relay carrier. Keep FIFO/durability and resume gates;
            // the relay worker will re-drive the carrier when authority returns.
            queue_prompt_on_record_with_source(
                inner.session_mut_by_index(index).expect("session index should be valid"),
                PendingPrompt {
                engram_interrupted:false, is_engram_retained:false,
                attachments:attachments.iter().map(|attachment|attachment.metadata.clone()).collect(),
                id:message_id.clone(), timestamp:stamp_now(), text:prompt,
                expanded_text:expanded_prompt, source,
            }, attachments, queued_prompt_source);
            self.commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, true, Some(session_id))
                .map_err(|error|ApiError::internal(format!("failed to persist ordinary prompt behind retained relay: {error:#}")))?;
            return Ok(DispatchTurnResult::Queued);
        }
        if session_is_busy || has_queued_prompts || blocked_automatic_prompt {
            if let Some(mailbox_id) = source
                .as_ref()
                .and_then(|source| source.mailbox.as_ref())
                .map(|mailbox| mailbox.mailbox_id.clone())
            {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                // An entry enqueued with a durability prerequisite keeps
                // its proven content; a wake never rewrites it.
                let durability_holds = &record.queued_prompt_durability_holds;
                if let Some(existing) = record.queued_prompts.iter_mut().find(|queued| {
                    !durability_holds.contains_key(&queued.pending_prompt.id)
                        && !queued.has_stored_history()
                        && queued
                            .pending_prompt
                            .source
                            .as_ref()
                            .and_then(|source| source.mailbox.as_ref())
                            .is_some_and(|mailbox| mailbox.mailbox_id == mailbox_id)
                }) {
                    let existing_sequence = existing
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .map(|mailbox| mailbox.sequence)
                        .unwrap_or_default();
                    let incoming_sequence = source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .map(|mailbox| mailbox.sequence)
                        .unwrap_or_default();
                    if existing_sequence <= incoming_sequence {
                        existing.pending_prompt.timestamp = stamp_now();
                        existing.pending_prompt.text = prompt;
                        existing.pending_prompt.expanded_text = expanded_prompt;
                        existing.pending_prompt.source = source;
                        existing.source = QueuedPromptSource::Mailbox;
                    }
                    message_id = existing.pending_prompt.id.clone();
                    note_mailbox_continuation_boundary(record, &message_id);
                    sync_pending_prompts(record);
                    self.commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, true, Some(session_id))
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to persist mailbox notification: {err:#}"
                            ))
                        })?;
                    if session_is_busy || orchestrator_auto_dispatch_blocked {
                        return Ok(DispatchTurnResult::Queued);
                    }
                    if blocked_queue_contains_user_prompt {
                        prioritize_user_queued_prompts(
                            inner
                                .session_mut_by_index(index)
                                .expect("session index should be valid"),
                        );
                    }
                    let (revision, started) = match self
                        .start_and_commit_queued_dispatch_locked(
                            &mut inner,
                            index,
                            followup,
                            &message_id,
                        ) {
                        Ok(Some(started)) => started,
                        Ok(None) => {
                            return Ok(
                                self.queued_after_direct_enqueue(inner, index, session_id)
                            );
                        }
                        Err(error) => {
                            drop(inner);
                            return Err(self.finish_queued_followup_start_error(
                                error.context("failed to dispatch coalesced mailbox queue"),
                            ));
                        }
                    };
                    let started_current_mailbox = matches!(
                        &started.message_delta.message,
                        Message::Text {
                            source: Some(source),
                            ..
                        } if source
                            .mailbox
                            .as_ref()
                            .is_some_and(|mailbox| mailbox.mailbox_id == mailbox_id)
                    );
                    self.publish_started_turn_message_delta(
                        &inner,
                        revision,
                        started.message_delta,
                    );
                    drop(inner);
                    return Ok(if started_current_mailbox {
                        DispatchTurnResult::Dispatched(started.dispatch)
                    } else {
                        DispatchTurnResult::DispatchedAfterQueue(started.dispatch)
                    });
                }
            }
        }
        let recover_blocked_queue_with_existing_user_prompt = !session_is_busy
            && !paused_human_queue
            && has_queued_prompts
            && orchestrator_auto_dispatch_blocked
            && blocked_queue_contains_user_prompt
            && !capacity_automatic_prompt
            && queued_prompt_source == QueuedPromptSource::User;
        // A head held for its durability acknowledgement is never
        // overtaken: the manual send queues behind it instead.
        let prioritize_manual_dispatch_over_blocked_queue = !session_is_busy
            && has_queued_prompts
            && orchestrator_auto_dispatch_blocked
            && !blocked_queue_contains_user_prompt
            && !capacity_automatic_prompt
            && queued_prompt_source == QueuedPromptSource::User
            && !queued_head_held_for_durability(&inner.sessions[index])
            && !inner.sessions[index].queued_prompts.front().is_some_and(|head|
                !ordinary_queue_entry_is_runnable(&inner.sessions[index], head));

        if recover_blocked_queue_with_existing_user_prompt {
            queue_prompt_on_record_with_source(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
                PendingPrompt {
                    engram_interrupted: false,
                    is_engram_retained: false,
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id.clone(),
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                    source: source.clone(),
                },
                attachments,
                queued_prompt_source,
            );
            prioritize_user_queued_prompts(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
            );
            self.commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, true, Some(session_id))
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist session state: {err:#}"))
                })?;
            let (revision, started) = match self.start_and_commit_queued_dispatch_locked(
                &mut inner,
                index,
                followup,
                &message_id,
            ) {
                Ok(Some(started)) => started,
                Ok(None) => {
                    return Ok(self.queued_after_direct_enqueue(inner, index, session_id));
                }
                Err(error) => {
                    drop(inner);
                    return Err(self.finish_queued_followup_start_error(error));
                }
            };
            self.publish_started_turn_message_delta(&inner, revision, started.message_delta);
            drop(inner);
            return Ok(DispatchTurnResult::DispatchedAfterQueue(started.dispatch));
        }

        if prioritize_manual_dispatch_over_blocked_queue {
            let engram_mcp = engram_mcp_runtime_config_for_session_locked(&inner, session_id);
            let host_mcp_servers_only =
                session_is_acceptance_evaluator_child_locked(&inner, session_id);
            let started = self.start_turn_on_record(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
                message_id.clone(),
                prompt,
                attachments,
                expanded_prompt,
                source,
                engram_mcp,
                host_mcp_servers_only,
            )?;
            EngramHost::turn_started(&mut inner, index);
            let revision = self
                .commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, false, Some(session_id))
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist session state: {err:#}"))
                })?;
            self.publish_started_turn_message_delta(&inner, revision, started.message_delta);
            drop(inner);
            return Ok(DispatchTurnResult::Dispatched(started.dispatch));
        }

        if session_is_busy || has_queued_prompts || blocked_automatic_prompt {
            queue_prompt_on_record_with_source(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
                PendingPrompt {
                    engram_interrupted: false,
                    is_engram_retained: false,
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id.clone(),
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt,
                    source,
                },
                attachments,
                queued_prompt_source,
            );
            self.commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, true, Some(session_id))
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist session state: {err:#}"))
                })?;
            // Only the explicit priority branches above may release a pause.
            // Appending new input cannot resume an unknown provider handoff.
            if session_is_busy || blocked_automatic_prompt || orchestrator_auto_dispatch_blocked {
                return Ok(DispatchTurnResult::Queued);
            }
            let (revision, started) = match self.start_and_commit_queued_dispatch_locked(
                &mut inner,
                index,
                followup,
                &message_id,
            ) {
                Ok(Some(started)) => started,
                Ok(None) => {
                    return Ok(self.queued_after_direct_enqueue(inner, index, session_id));
                }
                Err(error) => {
                    drop(inner);
                    return Err(self.finish_queued_followup_start_error(error));
                }
            };
            self.publish_started_turn_message_delta(&inner, revision, started.message_delta);
            drop(inner);
            return Ok(DispatchTurnResult::DispatchedAfterQueue(started.dispatch));
        }

        let engram_mcp = engram_mcp_runtime_config_for_session_locked(&inner, session_id);
        let host_mcp_servers_only =
            session_is_acceptance_evaluator_child_locked(&inner, session_id);
        let started = self.start_turn_on_record(
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid"),
            message_id.clone(),
            prompt,
            attachments,
            expanded_prompt,
            source,
            engram_mcp,
            host_mcp_servers_only,
        )?;
        EngramHost::turn_started(&mut inner, index);
        let revision = self
            .commit_followup_prompt_locked_for_session(&mut inner, followup, &message_id, false, Some(session_id))
            .map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
        self.publish_started_turn_message_delta(&inner, revision, started.message_delta);
        drop(inner);
        return Ok(DispatchTurnResult::Dispatched(started.dispatch));
    }
}

/// Whose queue head a paused-queue drain may admit, when its caller fixed it
/// in advance.
#[derive(Clone)]
enum QueuedDrainOwner {
    /// Ordinary service of an exact head; never permission to bypass a pause.
    Activation(QueuedTurnOwner),
    /// A metadata wake can re-drive only its captured boundary, never a successor.
    MailboxRelay(MailboxRelayDrainOwner),
    /// Same-process activation of one durable remote wake; never a pause bypass.
    FederationWake(FederationExecutorLiveOwner),
    /// An explicit Resume of the head its caller checked.
    Resume(QueuedTurnOwner),
}
