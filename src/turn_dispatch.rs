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
// from `turn_lifecycle.rs`. `dispatch_orphaned_queued_prompts` is the
// recovery path: after an abnormal exit or startup where a session
// landed back at `Idle` while still carrying queued prompts, we scan
// for that mismatch and re-kick the queue. Without it a crashed turn
// could leave queued prompts stranded forever.
//
// Runtime reset. `record.runtime_reset_required` signals that the
// session's long-lived runtime handle must be torn down and respawned
// before the next turn. `start_turn_on_record` honors it for Claude
// and ACP — kills the existing handle, clears pending approvals, and
// spawns a fresh one. Codex cannot be reset this way since it is
// shared across all Codex sessions (see `shared_codex_mgr.rs`).

struct StartedTurn {
    dispatch: TurnDispatch,
    message_delta: StartedTurnMessageDelta,
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
}

impl AppState {
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

        spawn_acp_runtime(self.clone(), session_id, workdir, agent, gemini_approval_mode)
    }

    fn publish_started_turn_message_delta(&self, revision: u64, delta: StartedTurnMessageDelta) {
        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: delta.session_id,
            message_id: delta.message_id,
            message_index: delta.message_index,
            message_count: delta.message_count,
            message: delta.message,
            preview: delta.preview,
            status: delta.status,
            session_mutation_stamp: Some(delta.session_mutation_stamp),
        });
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
    /// - Honor `record.runtime_reset_required` for Claude/ACP: kill
    ///   the existing handle, clear pending approvals, and force a
    ///   fresh spawn on this turn.
    /// - Route to the right spawn helper based on `record.session.agent`:
    ///   `spawn_claude_runtime`, the shared Codex runtime (see
    ///   [`Self::shared_codex_runtime`]), or `spawn_acp_runtime`.
    /// - Stamp `active_turn_start_message_count` so SSE deltas can later
    ///   attribute subsequent messages to this turn.
    /// - Route expanded prompts (slash-command expansions) separately
    ///   from the user-visible `prompt` so the UI still shows the raw
    ///   command but the runtime sees the expanded text.
    ///
    /// Returns a [`TurnDispatch`] bundling the runtime handle + a
    /// RuntimeToken the caller stores on the record for the `_if_matches`
    /// guard path later.
    fn start_turn_on_record(
        &self,
        record: &mut SessionRecord,
        message_id: String,
        prompt: String,
        attachments: Vec<PromptImageAttachment>,
        expanded_prompt: Option<String>,
    ) -> std::result::Result<StartedTurn, ApiError> {
        if record.is_remote_proxy() {
            return Err(ApiError::internal(
                "remote proxy sessions must dispatch through the remote backend",
            ));
        }

        let message_attachments = attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();
        record.active_turn_start_message_count = Some(record.session.messages.len());
        let expanded_prompt = expanded_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let runtime_prompt = expanded_prompt.clone().unwrap_or_else(|| prompt.clone());

        let dispatch = match record.session.agent {
            Agent::Claude => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Claude session runtime: {err:#}"
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                    record.runtime_reset_required = false;
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
                            None,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Claude session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Claude(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentClaude {
                    command: ClaudePromptCommand {
                        attachments: attachments.clone(),
                        text: runtime_prompt.clone(),
                    },
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
                    record.runtime = SessionRuntime::None;
                    record.pending_codex_approvals.clear();
                    record.pending_codex_user_inputs.clear();
                    record.pending_codex_mcp_elicitations.clear();
                    record.pending_codex_app_requests.clear();
                    record.runtime_reset_required = false;
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

                TurnDispatch::PersistentCodex {
                    command: CodexPromptCommand {
                        approval_policy: record.codex_approval_policy,
                        attachments,
                        cwd: record.session.workdir.clone(),
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        reasoning_effort: record.codex_reasoning_effort,
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            agent @ (Agent::Cursor | Agent::Gemini) => {
                if !attachments.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support image attachments yet",
                        agent.name()
                    )));
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
                    record.runtime = SessionRuntime::None;
                    record.pending_acp_approvals.clear();
                    record.runtime_reset_required = false;
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

                TurnDispatch::PersistentAcp {
                    command: AcpPromptCommand {
                        cwd: record.session.workdir.clone(),
                        cursor_mode: record.session.cursor_mode,
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        resume_session_id: record.external_session_id.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
        };

        record.orchestrator_auto_dispatch_blocked = false;
        record.active_turn_file_changes.clear();
        record.active_turn_file_change_grace_deadline = None;
        let message = Message::Text {
            attachments: message_attachments.clone(),
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::You,
            text: prompt.clone(),
            expanded_text: expanded_prompt,
        };
        let message_index = push_message_on_record(record, message.clone());
        record.session.status = SessionStatus::Active;
        record.session.preview = prompt_preview_text(&prompt, &message_attachments);
        let message_delta = StartedTurnMessageDelta {
            session_id: record.session.id.clone(),
            message_id,
            message_index,
            message_count: session_message_count(record),
            message,
            preview: record.session.preview.clone(),
            status: record.session.status,
            session_mutation_stamp: record.mutation_stamp,
        };

        Ok(StartedTurn {
            dispatch,
            message_delta,
        })
    }

    /// Re-kicks queued prompts whose owning session landed back at
    /// `SessionStatus::Idle` without draining the queue.
    ///
    /// This is the recovery path for the "stranded queue" failure mode:
    /// the normal flow is that `finish_turn_ok` / `mark_turn_error`
    /// transitions the session to `Idle` and immediately calls
    /// [`Self::dispatch_next_queued_turn`] to start the next prompt. If
    /// that flow is interrupted — a runtime exit handler fires mid-
    /// transition, a `stop_session` lands between them, or state is
    /// restored from disk mid-queue — the session can wind up idle with
    /// prompts still queued and no pending dispatch. This scans for
    /// that mismatch and re-invokes the dispatcher for every matching
    /// session. Safe to call speculatively: the dispatcher bails if
    /// the session is no longer idle or the queue has been drained by
    /// a concurrent call.
    ///
    /// Called at startup (after `recover_interrupted_sessions`) and
    /// defensively from a handful of error paths.
    fn dispatch_orphaned_queued_prompts(&self) {
        let session_ids: Vec<String> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.is_remote_proxy()
                        && record.session.status == SessionStatus::Idle
                        && !record.queued_prompts.is_empty()
                        && !record.orchestrator_auto_dispatch_blocked
                        && matches!(record.runtime, SessionRuntime::None)
                })
                .map(|record| record.session.id.clone())
                .collect()
        };

        for session_id in session_ids {
            match self.dispatch_next_queued_turn(&session_id, false) {
                Ok(Some(dispatch)) => {
                    if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                        eprintln!(
                            "startup> failed dispatching orphaned queued prompt for `{session_id}`: {}",
                            err.message
                        );
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!(
                        "startup> failed dispatching orphaned queued prompt for `{session_id}`: {err:#}"
                    );
                }
            }
        }
    }

    /// Pops the front of the session's queued-prompt FIFO and
    /// dispatches it when the session is idle.
    ///
    /// Called from every path that transitions a session to `Idle`:
    /// `finish_turn_ok`, `mark_turn_error`, `handle_runtime_exit`, the
    /// approval-submission paths that unblock a turn, and the orphan
    /// recovery [`Self::dispatch_orphaned_queued_prompts`]. Guards:
    ///
    /// - Does nothing if the session is not `Idle` (an in-flight turn
    ///   will call us again on completion).
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
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        if inner.sessions[index].orchestrator_auto_dispatch_blocked && !allow_blocked_dispatch {
            return Ok(None);
        }

        let queued = inner.sessions[index].queued_prompts.front().cloned();

        let Some(queued) = queued else {
            return Ok(None);
        };

        let started = self
            .start_turn_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
                queued.attachments.clone(),
                queued.pending_prompt.expanded_text.clone(),
            )
            .map_err(|err| anyhow!("failed to dispatch queued prompt: {}", err.message))?;
        let mut message_delta = started.message_delta;
        {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.queued_prompts.pop_front();
            sync_pending_prompts(record);
            message_delta.session_mutation_stamp = record.mutation_stamp;
        }
        let revision = self.commit_persisted_delta_locked(&mut inner)?;
        drop(inner);
        self.publish_started_turn_message_delta(revision, message_delta);
        Ok(Some(started.dispatch))
    }

    /// The inner dispatch that actually hands a ready turn to the agent
    /// driver.
    ///
    /// Locates the session in `AppState.inner`, builds the prompt
    /// payload, calls [`Self::start_turn_on_record`] to get a
    /// [`TurnDispatch`], installs the `RuntimeToken` on the record so
    /// later `_if_runtime_matches` guards can tell live from stale
    /// events, and finally forwards the prompt to the runtime's writer
    /// channel. If any step fails we mark the turn as errored via the
    /// lifecycle path rather than surfacing a bare `ApiError` to the
    /// caller — the turn is already underway by the time we're in here.
    fn dispatch_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            self.proxy_remote_turn_dispatch(session_id, request)?;
            return Ok(DispatchTurnResult::Queued);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if delegated_child_dispatch_is_blocked_locked(&inner, index) {
            return Err(ApiError::conflict(
                DELEGATION_NO_LONGER_STARTABLE_MESSAGE,
            ));
        }

        let mut prompt = request.text.trim().to_owned();
        let mut expanded_prompt = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if record_has_archived_codex_thread(&inner.sessions[index]) {
            return Err(ApiError::conflict(
                "the current Codex thread is archived; unarchive it before sending another prompt",
            ));
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
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }

        let session_is_busy = matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval
        );
        let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();
        let blocked_queue_contains_user_prompt = inner.sessions[index]
            .queued_prompts
            .iter()
            .any(|queued| queued.source == QueuedPromptSource::User);
        let recover_blocked_queue_with_existing_user_prompt = !session_is_busy
            && has_queued_prompts
            && inner.sessions[index].orchestrator_auto_dispatch_blocked
            && blocked_queue_contains_user_prompt;
        let prioritize_manual_dispatch_over_blocked_queue = !session_is_busy
            && has_queued_prompts
            && inner.sessions[index].orchestrator_auto_dispatch_blocked
            && !blocked_queue_contains_user_prompt;

        if recover_blocked_queue_with_existing_user_prompt {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                },
                attachments,
            );
            prioritize_user_queued_prompts(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;

            drop(inner);
            let dispatch = self
                .dispatch_next_queued_turn(session_id, true)
                .map_err(|err| {
                    ApiError::internal(format!("failed to dispatch queued turn: {err:#}"))
                })?
                .ok_or_else(|| ApiError::internal("queued prompt disappeared before dispatch"))?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        if prioritize_manual_dispatch_over_blocked_queue {
            let message_id = inner.next_message_id();
            let started = self.start_turn_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                message_id,
                prompt,
                attachments,
                expanded_prompt,
            )?;

            let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            drop(inner);
            self.publish_started_turn_message_delta(revision, started.message_delta);
            return Ok(DispatchTurnResult::Dispatched(started.dispatch));
        }

        if session_is_busy || has_queued_prompts {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                },
                attachments,
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            if session_is_busy {
                return Ok(DispatchTurnResult::Queued);
            }

            drop(inner);
            let dispatch = self
                .dispatch_next_queued_turn(session_id, true)
                .map_err(|err| {
                    ApiError::internal(format!("failed to dispatch queued turn: {err:#}"))
                })?
                .ok_or_else(|| ApiError::internal("queued prompt disappeared before dispatch"))?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        let message_id = inner.next_message_id();
        let started = self.start_turn_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            message_id,
            prompt,
            attachments,
            expanded_prompt,
        )?;

        let revision = self.commit_persisted_delta_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        drop(inner);
        self.publish_started_turn_message_delta(revision, started.message_delta);

        Ok(DispatchTurnResult::Dispatched(started.dispatch))
    }
}
