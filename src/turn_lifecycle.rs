// Turn state machine + pending-approval registers for `AppState`. A
// session's `SessionStatus` moves Idle -> Active when a prompt is
// dispatched, Active -> Approval when the agent asks the user to confirm
// a tool call (or fills any other interaction request), Approval -> Idle
// once the approval resolves and the agent finishes, and Active -> Idle
// on success, error, or abnormal runtime exit. `mark_turn_error` is the
// only transition that lands on `SessionStatus::Error` while keeping the
// turn live so the user can choose retry vs. abort.
//
// Every state transition is gated by a `RuntimeToken` match (see
// `src/session_runtime.rs` for `RuntimeToken` + `KillableRuntime` +
// `DeferredStopCallback`). Each runtime spawn stamps a fresh token on the
// `SessionRuntime` handle. If a runtime crashes or the user stops and
// restarts a session, stray in-flight events from the torn-down runtime
// would otherwise land on the new runtime and corrupt its state — the
// `_if_runtime_matches` wrapper drops them silently. Callers (event
// handlers on the runtime side, typically in `codex_events.rs` or the
// Claude/ACP equivalents) don't have to check whether their runtime is
// still current; they always call the guarded variant and it no-ops when
// the token is stale. When `runtime_stop_in_progress` is set, the
// transition is buffered onto `deferred_stop_callbacks` instead of being
// applied, so the stop machinery finalizes session state before the
// callback is replayed (see `src/tests/session_stop.rs`).
//
// The pending-approval registers each keep a per-session map so
// `submit_approval` / `submit_codex_user_input` etc. can later look up
// the in-flight interaction request. Each agent protocol identifies
// interactions differently: Claude uses a `request_id` string the CLI
// assigns, Codex uses a JSON-RPC `message_id`, ACP uses its own
// `message_id`. That's why the store is split per-protocol rather than
// shared. `clear_claude_pending_approval_by_request` is the inverse
// lookup Claude needs because its cancellation path arrives with a
// `request_id` rather than the message_id key the register is stored
// under. The pending store is consumed from `src/state.rs` by
// `update_approval`, `submit_codex_user_input`,
// `submit_codex_mcp_elicitation`, and `submit_codex_app_request`;
// `src/session_interaction.rs` owns the record-level interaction-state
// transitions and preview-text projections that fire once a pending
// entry resolves. Cross-refs: `src/session_runtime.rs` (RuntimeToken),
// `src/session_interaction.rs` (sync_session_interaction_state),
// `src/tests/session_stop.rs` + `src/tests/session_stop_runtime.rs`
// (invariant pins for deferred replay + stop lifecycle).

impl AppState {
    /// Active/Approval -> Idle with `SessionStatus::Error`. Runtime-token
    /// guarded: stale tokens silently no-op. If `runtime_stop_in_progress`,
    /// buffers a `DeferredStopCallback::TurnFailed` for replay instead of
    /// applying. Pushes a "Turn failed" assistant message, finalizes any
    /// active file-change tracking, and dispatches the next queued prompt
    /// if one is waiting.
    fn fail_turn_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnFailed(cleaned.to_owned()));
                return Ok(());
            }

            if let Some(message_id) = message_id {
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                });
            }
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }

            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            finish_active_turn_file_change_tracking(record);
            let has_queued_prompts = !record.queued_prompts.is_empty();
            match self.commit_locked(&mut inner) {
                Ok(_) => {}
                Err(err) => {
                    // Persistence failed but the in-memory state is already
                    // updated. Publish anyway so the frontend sees the error
                    // state instead of being stuck on an active turn.
                    eprintln!(
                        "state warning> failed to persist turn failure for session `{session_id}`, \
                         publishing in-memory state: {err:#}"
                    );
                    self.publish_state_locked(&inner);
                }
            }
            has_queued_prompts
        };

        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!("state warning> failed to refresh delegation after turn failure: {err:#}");
        }

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }
    /// Records a retry attempt on the transcript without ending the turn.
    /// Runtime-token guarded: stale tokens silently no-op. Keeps the turn
    /// in Active (or leaves Approval alone) and refreshes the preview
    /// while the runtime retries under the covers. De-duplicates
    /// consecutive identical retry messages so repeated retries don't
    /// spam the transcript.
    fn note_turn_retry_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        detail: &str,
    ) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let duplicate_last_message = {
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                return Ok(());
            }

            matches!(
                record.session.messages.last(),
                Some(Message::Text {
                    author: Author::Assistant,
                    text,
                    ..
                }) if text.trim() == cleaned
            )
        };

        let message_id = (!duplicate_last_message).then(|| inner.next_message_id());
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

        if let Some(message_id) = message_id {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: cleaned.to_owned(),
                expanded_text: None,
            });
        }

        if record.session.status != SessionStatus::Approval {
            record.session.status = SessionStatus::Active;
        }
        record.session.preview = make_preview(cleaned);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Active -> Error while the turn stays live. Runtime-token guarded:
    /// stale tokens silently no-op; when `runtime_stop_in_progress`,
    /// buffers a `DeferredStopCallback::TurnError` for replay. Unlike
    /// `fail_turn_if_runtime_matches`, this keeps the turn in a retryable
    /// state — the user can submit again without starting over.
    fn mark_turn_error_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnError(cleaned.to_owned()));
                return Ok(());
            }

            record.session.status = SessionStatus::Error;
            if !cleaned.is_empty() {
                record.session.preview = make_preview(cleaned);
            }
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }
            finish_active_turn_file_change_tracking(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
            );
            let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };
        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!("state warning> failed to refresh delegation after turn error: {err:#}");
        }

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }
    /// Active/Approval -> Idle on successful turn completion. Runtime-token
    /// guarded: stale tokens silently no-op; when `runtime_stop_in_progress`,
    /// buffers a `DeferredStopCallback::TurnCompleted` for replay. Also
    /// schedules any orchestrator transitions keyed off this session's
    /// completion and dispatches the next queued prompt.
    fn finish_turn_ok_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Result<()> {
        let stopping_orchestrator_session_ids = self.stopping_orchestrator_session_ids_snapshot();
        let (should_dispatch_next, orchestrator_delta) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnCompleted);
                return Ok(());
            }

            if record.session.status == SessionStatus::Active {
                record.session.status = SessionStatus::Idle;
            }
            if record.session.preview.trim().is_empty() {
                record.session.preview = "Turn completed.".to_owned();
            }
            let completion_revision = inner.revision.saturating_add(1);
            let orchestrator_changed = schedule_orchestrator_transitions_for_completed_session(
                &mut inner,
                &stopping_orchestrator_session_ids,
                session_id,
                completion_revision,
            );
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid"),
                    message_id,
                );
            }
            finish_active_turn_file_change_tracking(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
            );
            self.commit_locked(&mut inner)?;
            let orchestrator_delta = orchestrator_changed
                .then(|| (inner.revision, inner.orchestrator_instances.clone()));
            (true, orchestrator_delta)
        };

        if let Some((revision, orchestrators)) = orchestrator_delta {
            self.publish_orchestrators_updated(revision, orchestrators);
        }

        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!("state warning> failed to refresh delegation after turn completion: {err:#}");
        }

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }
    /// Handles abnormal runtime exit (process died, stdout closed, etc.).
    /// Runtime-token guarded: stale tokens silently no-op; when
    /// `runtime_stop_in_progress`, buffers a
    /// `DeferredStopCallback::RuntimeExited` for replay. Clears the
    /// `SessionRuntime` handle, drops every pending-interaction register,
    /// cancels outstanding interaction messages, and if the session was
    /// Active/Approval pushes a "Turn failed" message so the user sees a
    /// reason rather than a silent stall. Dispatches any queued prompt
    /// into the fresh slot once the runtime spins back up.
    fn handle_runtime_exit_if_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: Option<&str>,
    ) -> Result<()> {
        let cleaned = error_message.map(str::trim).unwrap_or("");
        let (should_dispatch_next, pending_interaction_updates, revision) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let matches_runtime = inner.sessions[index].runtime.matches_runtime_token(token);
            if !matches_runtime {
                return Ok(());
            }
            if inner.sessions[index].runtime_stop_in_progress {
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::RuntimeExited(
                        error_message.map(str::to_owned),
                    ));
                return Ok(());
            }
            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let message_id = (was_busy || !cleaned.is_empty()).then(|| inner.next_message_id());
            let detail = if !cleaned.is_empty() || was_busy {
                Some(if !cleaned.is_empty() {
                    cleaned.to_owned()
                } else {
                    match token {
                        RuntimeToken::Claude(_) => {
                            "Claude session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Codex(_) => {
                            "Codex session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Acp(_) => {
                            "Agent session exited before the active turn completed".to_owned()
                        }
                    }
                })
            } else {
                None
            };
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let (has_queued_prompts, pending_interaction_updates) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                record.runtime = SessionRuntime::None;
                record.runtime_reset_required = false;
                record.orchestrator_auto_dispatch_blocked = false;
                record.runtime_stop_in_progress = false;
                record.deferred_stop_callbacks.clear();
                let pending_interaction_indices =
                    cancel_pending_interaction_messages(&mut record.session.messages);
                clear_all_pending_requests(record);
                if let Some(detail) = detail.as_ref() {
                    if let Some(message_id) = message_id {
                        record.session.messages.push(Message::Text {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            text: format!("Turn failed: {detail}"),
                            expanded_text: None,
                        });
                    }
                    record.session.status = SessionStatus::Error;
                    record.session.preview = make_preview(detail);
                }
                if let Some(message_id) = file_change_message_id {
                    push_active_turn_file_changes_on_record(record, message_id);
                }
                (
                    !record.queued_prompts.is_empty(),
                    message_updated_delta_parts_for_indices(record, pending_interaction_indices),
                )
            };
            finish_active_turn_file_change_tracking(
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid"),
            );
            let revision = self.commit_locked(&mut inner)?;
            (has_queued_prompts, pending_interaction_updates, revision)
        };
        self.publish_message_updated_delta_parts(revision, pending_interaction_updates);

        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!("state warning> failed to refresh delegation after runtime exit: {err:#}");
        }

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }

    /// Stores a Claude pending approval keyed by `message_id`.
    /// `update_approval` in `src/state.rs` looks up the entry by
    /// `message_id` when the user clicks accept/reject and routes the
    /// decision back to the Claude runtime using the stored handle.
    fn register_claude_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Stores a Codex pending approval keyed by `message_id`.
    /// `update_approval` in `src/state.rs` looks it up on user action
    /// and sends the decision back over JSON-RPC.
    fn register_codex_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Stores a Codex pending user-input request keyed by `message_id`.
    /// `submit_codex_user_input` in `src/state.rs` looks it up when the
    /// user answers the form and returns the answers to Codex.
    fn register_codex_pending_user_input(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_user_inputs
            .insert(message_id, request);
        Ok(())
    }

    /// Stores a Codex pending MCP elicitation keyed by `message_id`.
    /// `submit_codex_mcp_elicitation` in `src/state.rs` looks it up when
    /// the user chooses accept/decline/cancel and returns the result to
    /// the MCP server via Codex.
    fn register_codex_pending_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingMcpElicitation,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_mcp_elicitations
            .insert(message_id, request);
        Ok(())
    }

    /// Stores a Codex pending app request keyed by `message_id`.
    /// `submit_codex_app_request` in `src/state.rs` looks it up when the
    /// user responds and returns the result to the Codex app-server.
    fn register_codex_pending_app_request(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingAppRequest,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_app_requests
            .insert(message_id, request);
        Ok(())
    }

    /// Stores an ACP pending approval keyed by `message_id`.
    /// `update_approval` in `src/state.rs` looks it up when the user
    /// responds and dispatches the decision over the ACP protocol.
    fn register_acp_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_acp_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Drops any Claude pending approval entries matching `request_id` and
    /// marks the backing transcript messages as `Canceled`. Claude's
    /// cancellation events carry a `request_id` (the Claude CLI's internal
    /// identifier) rather than the `message_id` that keys the register —
    /// so this is the only clear path that walks the map to find matching
    /// entries instead of looking up by the store's key directly.
    fn clear_claude_pending_approval_by_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let message_ids: Vec<String> = record
            .pending_claude_approvals
            .iter()
            .filter(|(_, approval)| approval.request_id == request_id)
            .map(|(message_id, _)| message_id.clone())
            .collect();

        if message_ids.is_empty() {
            return Ok(());
        }

        for message_id in &message_ids {
            set_approval_decision_on_record(record, message_id, ApprovalDecision::Canceled)?;
            record.pending_claude_approvals.remove(message_id);
        }

        sync_session_interaction_state(
            record,
            approval_preview_text(record.session.agent.name(), ApprovalDecision::Canceled),
        );
        self.commit_locked(&mut inner)?;
        Ok(())
    }
}
