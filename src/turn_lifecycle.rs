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
// applied. Buffered callbacks also retain `active_turn_generation`, because
// persistent Claude/ACP handles and shared Codex processes may reuse a token
// for a successor turn. The stop machinery finalizes session state before the
// callback is replayed (see `src/tests/session_stop.rs`).
//
// The pending-approval registers each keep a per-session map so
// `submit_approval` / `submit_codex_user_input` etc. can later look up
// the in-flight interaction request. Each agent protocol identifies
// interactions differently: Claude uses a `request_id` string the CLI
// assigns, Codex uses a JSON-RPC `message_id`, ACP uses its own
// `message_id`. That's why the store is split per-protocol rather than
// shared. `clear_claude_pending_interaction_by_request` is the inverse
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

struct EngramMcpRuntimeRevocationTarget {
    session_id: String,
    token: RuntimeToken,
    runtime: KillableRuntime,
    owner_generation: u64,
    stop_options: Option<StopSessionOptions>,
}

#[derive(Default)]
struct EngramMcpRuntimeRevocationBatch {
    targets: Vec<EngramMcpRuntimeRevocationTarget>,
    pending_session_ids: Vec<String>,
    newly_pending_session_ids: Vec<String>,
}

#[derive(Default)]
struct EngramMcpRuntimeRevocationRelease {
    deferred_callbacks: Vec<(String, RuntimeToken, Vec<DeferredStopCallback>)>,
    project_fence_release_failed: bool,
}

#[derive(Debug, Default)]
struct EngramMcpRuntimeRevocationCompletion {
    session_id: String,
    should_dispatch_next: bool,
    should_refresh_delegation: bool,
    should_resume_orchestrator_transitions: bool,
    orchestrator_stop_instance_id: Option<String>,
}

#[derive(Debug, Default)]
struct EngramMcpRuntimeRevocationFinalization {
    completion: Option<EngramMcpRuntimeRevocationCompletion>,
    failures: Vec<String>,
}

struct EngramMcpRuntimeRevocationShutdown {
    target: EngramMcpRuntimeRevocationTarget,
    shutdown_error: Option<String>,
    retain_runtime_for_retry: bool,
    suppress_codex_thread_resume: bool,
}

#[derive(Default)]
struct EngramMcpRuntimeRevocationShutdownBatch {
    shutdowns: Vec<EngramMcpRuntimeRevocationShutdown>,
    pending_session_ids: Vec<String>,
}

#[derive(Default)]
struct EngramMcpRuntimeRevocationOutcome {
    completions: Vec<EngramMcpRuntimeRevocationCompletion>,
    pending_session_ids: Vec<String>,
    failures: Vec<String>,
}

#[derive(Clone, Copy)]
enum AtomicTurnFailureMode<'a> {
    RejectedDelivery {
        token: &'a RuntimeToken,
        active_turn_generation: u64,
        owner_generation: u64,
    },
    MissingRuntime {
        active_turn_generation: u64,
        owner_generation: u64,
    },
    MatchingRuntime {
        token: &'a RuntimeToken,
        active_turn_generation: u64,
        owner_generation: u64,
        retain_runtime: bool,
    },
}

impl<'a> AtomicTurnFailureMode<'a> {
    fn preserves_accepted_turn_state(self) -> bool {
        !matches!(self, Self::RejectedDelivery { .. })
    }

    fn retained_runtime_token(self) -> Option<&'a RuntimeToken> {
        match self {
            Self::MatchingRuntime {
                token,
                retain_runtime: true,
                ..
            } => Some(token),
            _ => None,
        }
    }

    fn runtime_token(self) -> Option<&'a RuntimeToken> {
        match self {
            Self::RejectedDelivery { token, .. } | Self::MatchingRuntime { token, .. } => {
                Some(token)
            }
            Self::MissingRuntime { .. } => None,
        }
    }
}

/// Captures every stale runtime while the settings mutation still owns the
/// state lock. The settings update and these fences are committed together, so
/// a new descriptor can never be mistaken for part of the stale batch.
fn claim_engram_mcp_runtime_revocations_locked(
    inner: &mut StateInner,
    session_ids: &[String],
) -> EngramMcpRuntimeRevocationBatch {
    let mut batch = EngramMcpRuntimeRevocationBatch::default();
    for session_id in session_ids {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        let record = &inner.sessions[index];
        if !record.is_local_session() {
            continue;
        }
        let Some(token) = record.runtime.runtime_token() else {
            continue;
        };
        if record.runtime_stop_in_progress {
            let was_already_pending = record.engram_mcp_revocation_pending;
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram_mcp_revocation_pending = true;
            batch.pending_session_ids.push(session_id.clone());
            if !was_already_pending {
                batch.newly_pending_session_ids.push(session_id.clone());
            }
            continue;
        }
        let runtime = match &record.runtime {
            SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
            SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
            SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
            SessionRuntime::None => continue,
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        debug_assert!(record.deferred_stop_callbacks.is_empty());
        let owner_generation = record.claim_runtime_stop(
            RuntimeStopOwnerKind::EngramMcpRevocation,
            token.clone(),
        );
        record.engram_mcp_revocation_pending = false;
        batch.targets.push(EngramMcpRuntimeRevocationTarget {
            session_id: session_id.clone(),
            token,
            runtime,
            owner_generation,
            stop_options: None,
        });
    }
    batch
}

/// Restores claims that were never made visible because their enclosing
/// settings/delete commit failed under the same state lock.
fn rollback_engram_mcp_runtime_revocations_locked(
    inner: &mut StateInner,
    batch: &EngramMcpRuntimeRevocationBatch,
) {
    for target in &batch.targets {
        let Some(index) = inner.find_session_index(&target.session_id) else {
            continue;
        };
        if inner.sessions[index].runtime.matches_runtime_token(&target.token)
            && inner.sessions[index].runtime_stop_is_owned_by(
                RuntimeStopOwnerKind::EngramMcpRevocation,
                &target.token,
                target.owner_generation,
            )
        {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .clear_runtime_stop();
        }
    }
    for session_id in &batch.newly_pending_session_ids {
        if let Some(index) = inner.find_session_index(session_id) {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .engram_mcp_revocation_pending = false;
        }
    }
}

/// Transfers a failed ordinary Stop's existing callback fence to the pending
/// revocation without a lock gap in which a queued prompt could run.
fn take_pending_engram_mcp_revocation_after_stop_failure_locked(
    inner: &mut StateInner,
    index: usize,
    stop_owner_generation: u64,
    stop_options: StopSessionOptions,
) -> Option<EngramMcpRuntimeRevocationTarget> {
    let record = &inner.sessions[index];
    if !record.engram_mcp_revocation_pending || !record.runtime_stop_in_progress {
        return None;
    }
    let token = record.runtime.runtime_token()?;
    if !record.runtime_stop_owner.as_ref().is_some_and(|owner| {
        owner.kind == RuntimeStopOwnerKind::UserStop
            && owner.token.as_ref() == Some(&token)
            && owner.generation == stop_owner_generation
    }) {
        return None;
    }
    let runtime = match &record.runtime {
        SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
        SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
        SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
        SessionRuntime::None => return None,
    };
    let session_id = record.session.id.clone();
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    record.engram_mcp_revocation_pending = false;
    let owner_generation = record.claim_runtime_stop(
        RuntimeStopOwnerKind::EngramMcpRevocation,
        token.clone(),
    );
    Some(EngramMcpRuntimeRevocationTarget {
        session_id,
        token,
        runtime,
        owner_generation,
        stop_options: Some(stop_options),
    })
}

/// Clears exact generation-owned runtime revocation fences under an existing
/// StateInner lock. Project settings transactions use this primitive while
/// releasing their project-generation fence in the same critical section, so
/// a newer mutation cannot install pending teardown state between the two.
fn release_engram_mcp_runtime_revocations_without_teardown_locked(
    inner: &mut StateInner,
    batch: EngramMcpRuntimeRevocationBatch,
) -> (EngramMcpRuntimeRevocationRelease, bool) {
    let mut release = EngramMcpRuntimeRevocationRelease::default();
    let mut changed = false;
    for target in batch.targets {
        let Some(index) = inner.find_session_index(&target.session_id) else {
            continue;
        };
        if !inner.sessions[index].runtime_stop_is_owned_by(
            RuntimeStopOwnerKind::EngramMcpRevocation,
            &target.token,
            target.owner_generation,
        ) {
            continue;
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let runtime_still_matches = record.runtime.matches_runtime_token(&target.token);
        record.clear_runtime_stop();
        let deferred_callbacks = std::mem::take(&mut record.deferred_stop_callbacks);
        if runtime_still_matches && !deferred_callbacks.is_empty() {
            release
                .deferred_callbacks
                .push((target.session_id, target.token, deferred_callbacks));
        }
        changed = true;
    }
    for session_id in batch.newly_pending_session_ids {
        if let Some(index) = inner.find_session_index(&session_id) {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if record.engram_mcp_revocation_pending {
                record.engram_mcp_revocation_pending = false;
                changed = true;
            }
        }
    }
    (release, changed)
}

impl AppState {
    /// Releases a visible revocation fence after Engram confirmed that the old
    /// authority is unusable. The runtime stays alive until its normal reset
    /// boundary, while terminal callbacks captured during the bounded revoke
    /// window are returned for replay after the project fence is released.
    fn release_engram_mcp_runtime_revocations_without_teardown(
        &self,
        batch: EngramMcpRuntimeRevocationBatch,
    ) -> EngramMcpRuntimeRevocationRelease {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let (release, changed) =
            release_engram_mcp_runtime_revocations_without_teardown_locked(&mut inner, batch);
        if changed {
            self.publish_state_locked(&inner);
        }
        release
    }

    fn replay_deferred_runtime_stop_callbacks(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        mut deferred_callbacks: Vec<DeferredStopCallback>,
    ) {
        // Runtime exit must remain last: it removes the runtime handle that the
        // preceding turn terminal callbacks still need to match. Every replay
        // also validates the callback's original active-turn generation, so a
        // same-token successor cannot absorb a stale terminal transition after
        // the stop fence has been released.
        deferred_callbacks.sort_by_key(|deferred| {
            matches!(deferred, DeferredStopCallback::RuntimeExited { .. })
        });
        for deferred in deferred_callbacks {
            let active_turn_generation = deferred.active_turn_generation();
            let replay_result = match deferred {
                DeferredStopCallback::TurnFailed { message, .. } => {
                    self.fail_turn_if_runtime_and_generation_match(
                        session_id,
                        token,
                        active_turn_generation,
                        &message,
                    )
                }
                DeferredStopCallback::TurnError { message, .. } => {
                    self.mark_turn_error_if_runtime_and_generation_match(
                        session_id,
                        token,
                        active_turn_generation,
                        &message,
                    )
                }
                DeferredStopCallback::TurnCompleted { .. } => {
                    self.finish_turn_ok_if_runtime_and_generation_match(
                        session_id,
                        token,
                        active_turn_generation,
                    )
                }
                DeferredStopCallback::RuntimeExited { message, .. } => {
                    self.handle_runtime_exit_if_runtime_and_generation_match(
                        session_id,
                        token,
                        active_turn_generation,
                        message.as_deref(),
                    )
                }
            };
            if let Err(error) = replay_result {
                eprintln!(
                    "session cleanup warning> failed to replay deferred stop callback for session `{session_id}`: {error:#}"
                );
            }
        }
    }

    /// Releases the fence owned by a revocation target when its token was
    /// replaced by a lower-level teardown path. Dispatch never crosses this
    /// fence, so any callbacks buffered for the stale token are discarded.
    fn release_engram_mcp_revocation_fence_after_token_mismatch(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        owner_generation: u64,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        if !inner.sessions[index].runtime_stop_is_owned_by(
            RuntimeStopOwnerKind::EngramMcpRevocation,
            token,
            owner_generation,
        ) {
            return Ok(());
        }
        if inner.sessions[index].runtime.matches_runtime_token(token) {
            return Err(anyhow!(
                "Engram MCP revocation token unexpectedly became current again"
            ));
        }
        {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.engram_mcp_runtime_quarantined = false;
            record.clear_runtime_stop();
            record.deferred_stop_callbacks.clear();
        }
        self.commit_locked(&mut inner).map_err(|error| {
            self.publish_state_locked(&inner);
            anyhow!("failed to persist released revocation fence: {error:#}")
        })?;
        Ok(())
    }

    /// Completes an Engram MCP revocation while owning the stop fence claimed
    /// above. Busy state is sampled in the same critical section as the
    /// terminal mutation. Successful revocation resumes queued work against a
    /// fresh descriptor; degraded process cleanup leaves the session in Error
    /// with automatic dispatch blocked and reports the failure to the caller.
    fn finish_revoked_engram_mcp_runtime_if_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        owner_generation: u64,
        stop_options: Option<&StopSessionOptions>,
        suppress_codex_thread_resume: bool,
        retain_runtime_for_retry: bool,
        shutdown_error: Option<&str>,
    ) -> EngramMcpRuntimeRevocationFinalization {
        let (
            revision,
            pending_interaction_updates,
            created_messages,
            should_dispatch_next,
            may_dispatch_requeued_mailbox,
            failed_mailbox_notification,
            should_refresh_delegation,
            stopped_wait_refresh,
            persist_error,
            effective_shutdown_error,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramMcpRuntimeRevocationFinalization::default();
            };
            if !inner.sessions[index].runtime.matches_runtime_token(token) {
                drop(inner);
                let failure = self
                    .release_engram_mcp_revocation_fence_after_token_mismatch(
                    session_id,
                    token,
                    owner_generation,
                )
                    .err()
                    .map(|error| format!("failed to release stale revocation fence: {error:#}"));
                return EngramMcpRuntimeRevocationFinalization {
                    completion: None,
                    failures: failure.into_iter().collect(),
                };
            }
            if !inner.sessions[index].runtime_stop_is_owned_by(
                RuntimeStopOwnerKind::EngramMcpRevocation,
                token,
                owner_generation,
            ) {
                if !inner.sessions[index].runtime_stop_in_progress
                    && inner.sessions[index].runtime_stop_owner.is_none()
                {
                    // A lower-level cleanup path can clear this owner without
                    // replacing the runtime. Reclaim the fence under the same
                    // lock so a live stale descriptor is never left pending
                    // with nobody scheduled to finish its revocation.
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid")
                        .claim_runtime_stop(
                            RuntimeStopOwnerKind::EngramMcpRevocation,
                            token.clone(),
                        );
                } else {
                    return EngramMcpRuntimeRevocationFinalization {
                        completion: None,
                        failures: vec![
                            "Engram MCP revocation lost its runtime stop fence to another owner"
                                .to_owned(),
                        ],
                    };
                }
            }

            // A dedicated runtime can exit in the narrow interval after the
            // off-lock exit probe reported it live and before finalization
            // retakes the state lock. Its waiter buffers RuntimeExited behind
            // this fence. Treat that callback as confirmed cleanup instead of
            // discarding it and quarantining an already-dead handle. Shared
            // Codex exit callbacks can also be synthesized by the deliberate
            // whole-app-server escalation, so only dedicated handles provide
            // this confirmation.
            let is_shared_codex_runtime = matches!(
                &inner.sessions[index].runtime,
                SessionRuntime::Codex(handle) if handle.shared_session.is_some()
            );
            let runtime_exit_was_buffered = inner.sessions[index]
                .deferred_stop_callbacks
                .iter()
                .any(|callback| matches!(callback, DeferredStopCallback::RuntimeExited { .. }));
            let buffered_exit_confirms_cleanup = retain_runtime_for_retry
                && !is_shared_codex_runtime
                && runtime_exit_was_buffered;
            let retain_runtime_for_retry =
                retain_runtime_for_retry && !buffered_exit_confirms_cleanup;
            let shutdown_error = if buffered_exit_confirms_cleanup {
                None
            } else {
                shutdown_error
            };

            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let preserve_delegation_for_rebind = inner.sessions[index].engram.rebind_required
                && inner.sessions[index].engram.active_grant_id.is_some()
                && engram_project_for_session_locked(&inner, session_id).is_some();
            let should_write_message = was_busy || shutdown_error.is_some();
            let message_id = should_write_message.then(|| inner.next_message_id());
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let suppress_automatic_resume = stop_options
                .is_some_and(|options| options.pause_automatic_resumes_on_success);
            let dispatch_queued_prompts = stop_options
                .map_or(true, |options| options.dispatch_queued_prompts_on_success);
            let mut thread_id_to_suppress = None;
            let (
                has_queued_prompts,
                pending_interaction_updates,
                created_messages,
                failed_mailbox_notification,
            ) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let failed_mailbox_notification =
                    record.active_turn_mailbox_notification.take();
                take_and_abandon_engram_pending_dispatch(record);
                if retain_runtime_for_retry {
                    // Termination failed and process exit was not confirmed.
                    // Keep the only application-owned handle quarantined so a
                    // later explicit user action can retry teardown. The reset
                    // flag makes every such action kill this stale descriptor
                    // before a fresh runtime can be constructed.
                    record.runtime_reset_required = true;
                    record.engram_mcp_runtime_quarantined = true;
                } else {
                    record.clear_runtime();
                    record.clear_runtime_reset();
                }
                record.clear_runtime_stop();
                record.deferred_stop_callbacks.clear();
                let auto_dispatch_blocked = record.orchestrator_auto_dispatch_blocked
                    || shutdown_error.is_some()
                    || suppress_automatic_resume;
                record.set_auto_dispatch_blocked(auto_dispatch_blocked);
                if suppress_automatic_resume {
                    clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
                }
                if suppress_codex_thread_resume {
                    thread_id_to_suppress = record.external_session_id.clone();
                    set_record_external_session_id(record, None);
                }
                let pending_interaction_indices =
                    cancel_pending_interaction_messages(&mut record.session.messages);
                clear_all_pending_requests(record);
                let mut created_message_indices = Vec::new();
                if let Some(message_id) = message_id {
                    let detail = match shutdown_error {
                        Some(error) => format!(
                            "Engram MCP configuration was revoked, but the old agent runtime could not be stopped cleanly: {error}"
                        ),
                        None => "Turn stopped: Engram MCP configuration was revoked.".to_owned(),
                    };
                    let message_index = record.session.messages.len();
                    record.session.messages.push(Message::Text {
                        attachments: Vec::new(),
                        id: message_id,
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: detail.clone(),
                        expanded_text: None,
                        source: None,
                    });
                    created_message_indices.push(message_index);
                    record.session.status = if shutdown_error.is_some() {
                        SessionStatus::Error
                    } else {
                        SessionStatus::Idle
                    };
                    record.session.preview = make_preview(&detail);
                }
                if let Some(message_id) = file_change_message_id {
                    let file_change_message_index = record.session.messages.len();
                    if push_active_turn_file_changes_on_record(record, message_id) {
                        created_message_indices.push(file_change_message_index);
                    }
                }
                finish_active_turn_file_change_tracking(record);
                (
                    !record.queued_prompts.is_empty(),
                    message_updated_delta_parts_for_indices(
                        record,
                        pending_interaction_indices,
                    ),
                    message_created_delta_parts_for_indices(record, created_message_indices),
                    failed_mailbox_notification,
                )
            };

            if let Some(ref thread_id) = thread_id_to_suppress {
                inner.ignore_discovered_codex_thread(Some(thread_id));
            }

            let delegation_waits_before_stop =
                suppress_automatic_resume.then(|| inner.delegation_waits.clone());
            let stopped_wait_refresh = if suppress_automatic_resume {
                consume_delegation_waits_for_stopped_parent_locked(&mut inner, session_id)
            } else {
                DelegationWaitRefresh::default()
            };

            let mut stopped_orchestrator_instance_index = None;
            let mut added_stopped_session_id = false;
            if let Some(orchestrator_instance_id) = stop_options
                .and_then(|options| options.orchestrator_stop_instance_id.as_deref())
            {
                if let Some(instance_index) = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == orchestrator_instance_id)
                {
                    stopped_orchestrator_instance_index = Some(instance_index);
                    let stopped_session_ids = &mut inner.orchestrator_instances[instance_index]
                        .stopped_session_ids_during_stop;
                    if !stopped_session_ids.iter().any(|candidate| candidate == session_id) {
                        stopped_session_ids.push(session_id.to_owned());
                        stopped_session_ids.sort();
                        added_stopped_session_id = true;
                    }
                }
            }

            match self.commit_locked(&mut inner) {
                Ok(revision) => (
                    Some(revision),
                    pending_interaction_updates,
                    created_messages,
                    shutdown_error.is_none()
                        && !suppress_automatic_resume
                        && dispatch_queued_prompts
                        && has_queued_prompts,
                    shutdown_error.is_none()
                        && !suppress_automatic_resume
                        && dispatch_queued_prompts,
                    failed_mailbox_notification,
                    should_write_message && !preserve_delegation_for_rebind,
                    stopped_wait_refresh,
                    None,
                    shutdown_error.map(str::to_owned),
                ),
                Err(error) => {
                    if let Some(delegation_waits_before_stop) = delegation_waits_before_stop {
                        inner.delegation_waits = delegation_waits_before_stop;
                    }
                    if added_stopped_session_id {
                        if let Some(instance_index) = stopped_orchestrator_instance_index {
                            inner.orchestrator_instances[instance_index]
                                .stopped_session_ids_during_stop
                                .retain(|candidate| candidate != session_id);
                        }
                    }
                    let detail = format!("{error:#}");
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid")
                        .set_auto_dispatch_blocked(true);
                    self.publish_state_locked(&inner);
                    (
                        None,
                        Vec::new(),
                        Vec::new(),
                    false,
                    false,
                    failed_mailbox_notification,
                    should_write_message && !preserve_delegation_for_rebind,
                        DelegationWaitRefresh::default(),
                        Some(detail),
                        shutdown_error.map(str::to_owned),
                    )
                }
            }
        };

        let mailbox_requeued = failed_mailbox_notification
            .as_ref()
            .is_some_and(|notification| {
                match self.requeue_rejected_mailbox_notification(notification) {
                    Ok(changed) => changed,
                    Err(error) => {
                        eprintln!(
                            "mailbox> failed restoring the Engram-revoked wake for `{}` / `{}`: {error:#}",
                            notification.session_id, notification.mailbox_id
                        );
                        false
                    }
                }
            });

        if let Some(revision) = revision {
            self.publish_message_created_delta_parts(revision, created_messages);
            self.publish_message_updated_delta_parts(revision, pending_interaction_updates);
            self.publish_delegation_wait_consumed_deltas(
                revision,
                &stopped_wait_refresh.consumed_waits,
            );
        }

        let mut failures = Vec::new();
        if let Some(error) = effective_shutdown_error {
            failures.push(format!("runtime shutdown failed: {error}"));
        }
        let persist_succeeded = persist_error.is_none();
        if let Some(error) = persist_error {
            failures.push(format!("failed to persist revocation state: {error}"));
        }
        EngramMcpRuntimeRevocationFinalization {
            completion: Some(EngramMcpRuntimeRevocationCompletion {
                session_id: session_id.to_owned(),
                should_dispatch_next: should_dispatch_next
                    || (mailbox_requeued && may_dispatch_requeued_mailbox),
                should_refresh_delegation,
                should_resume_orchestrator_transitions: persist_succeeded
                    && stop_options.is_none_or(|options| {
                        !options.pause_automatic_resumes_on_success
                    }),
                orchestrator_stop_instance_id: stop_options
                    .and_then(|options| options.orchestrator_stop_instance_id.clone()),
            }),
            failures,
        }
    }

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
        self.fail_turn_if_runtime_matches_and_report(
            session_id,
            token,
            None,
            error_message,
        )
            .map(|_| ())
    }

    fn fail_turn_if_runtime_and_generation_match(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        error_message: &str,
    ) -> Result<()> {
        self.fail_turn_if_runtime_matches_and_report(
            session_id,
            token,
            Some(active_turn_generation),
            error_message,
        )
        .map(|_| ())
    }

    fn fail_turn_if_runtime_matches_and_report(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        expected_active_turn_generation: Option<u64>,
        error_message: &str,
    ) -> Result<bool> {
        self.checkpoint_engram_turn_off_lock(
            session_id,
            Some(token),
            expected_active_turn_generation,
            EngramNextIntent::Wait,
            None,
        );
        let cleaned = error_message.trim();
        let (should_dispatch_next, failed_mailbox_notification) = {
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
                return Ok(false);
            }
            if expected_active_turn_generation.is_some_and(|generation| {
                record.active_turn_generation != generation
            }) {
                return Ok(false);
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnFailed {
                        active_turn_generation: record.active_turn_generation,
                        message: cleaned.to_owned(),
                    });
                return Ok(false);
            }
            take_and_abandon_engram_pending_dispatch(record);
            let failed_mailbox_notification =
                record.active_turn_mailbox_notification.take();
            if failed_mailbox_notification.is_some() {
                // Restore the durable wake below, but do not immediately run
                // the same poisoned queue head again. An explicit resume can
                // retry it after the operator has inspected the failure.
                record.set_auto_dispatch_blocked(true);
            }

            if let Some(message_id) = message_id {
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                    source: None,
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
            (has_queued_prompts, failed_mailbox_notification)
        };

        if let Some(notification) = failed_mailbox_notification.as_ref() {
            if let Err(error) = self.requeue_rejected_mailbox_notification(notification) {
                eprintln!(
                    "mailbox> failed restoring the failed wake for `{}` / `{}`: {error:#}",
                    notification.session_id, notification.mailbox_id
                );
            }
        }

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

        Ok(true)
    }

    /// Records a rejected runtime-command delivery as one durable terminal
    /// transition. The runtime handle, pending runtime state, failure message,
    /// and public Error status must share one revision so restart recovery can
    /// never observe an Active session whose dead runtime was already cleared.
    fn fail_rejected_turn_delivery(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        let Some(owner_generation) = self.claim_turn_terminalization_if_runtime_matches(
            session_id,
            token,
            active_turn_generation,
        )? else {
            return Ok(false);
        };
        self.fail_turn_and_clear_runtime_atomically(
            session_id,
            error_message,
            AtomicTurnFailureMode::RejectedDelivery {
                token,
                active_turn_generation,
                owner_generation,
            },
        )
    }

    /// Terminalizes an Active/Approval turn only when its runtime has already
    /// disappeared and no stop owner is coordinating callbacks. This is the
    /// narrow recovery path for Stop and late shared-Codex turn/start errors;
    /// a stale error cannot affect a live successor runtime.
    fn fail_active_turn_if_runtime_missing(
        &self,
        session_id: &str,
        active_turn_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        let owner_generation = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            if !matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) || !matches!(inner.sessions[index].runtime, SessionRuntime::None)
                || inner.sessions[index].active_turn_generation != active_turn_generation
                || inner.sessions[index].runtime_stop_in_progress
            {
                return Ok(false);
            }
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid")
                .claim_missing_runtime_stop(RuntimeStopOwnerKind::LostRuntimeTerminalization)
        };
        self.fail_turn_and_clear_runtime_atomically(
            session_id,
            error_message,
            AtomicTurnFailureMode::MissingRuntime {
                active_turn_generation,
                owner_generation,
            },
        )
    }

    /// Claims the exact runtime before an accepted shared-Codex turn is
    /// interrupted off-lock. The owner prevents a successor dispatch or a
    /// terminal callback from entering between interruption and finalization.
    fn claim_turn_terminalization_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
    ) -> Result<Option<u64>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        if !inner.sessions[index].runtime.matches_runtime_token(token)
            || inner.sessions[index].active_turn_generation != active_turn_generation
            || inner.sessions[index].runtime_stop_in_progress
            || !matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            )
        {
            return Ok(None);
        }
        let generation = inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .claim_runtime_stop(
                RuntimeStopOwnerKind::LostRuntimeTerminalization,
                token.clone(),
            );
        Ok(Some(generation))
    }

    /// Releases an exact lost-runtime terminalization claim when a later
    /// shared-Codex recheck proves that `turn/started` or a successor won the
    /// race. Deferred terminal callbacks are replayed only after the owner is
    /// gone so none remain stranded behind a cancelled watchdog.
    fn release_turn_terminalization_if_owned(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        owner_generation: u64,
    ) -> bool {
        let callbacks = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return false;
            };
            if !inner.sessions[index].runtime_stop_is_owned_by(
                RuntimeStopOwnerKind::LostRuntimeTerminalization,
                token,
                owner_generation,
            ) {
                return false;
            }
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.clear_runtime_stop();
            std::mem::take(&mut record.deferred_stop_callbacks)
        };
        self.replay_deferred_runtime_stop_callbacks(session_id, token, callbacks);
        true
    }

    fn fail_turn_and_clear_runtime_if_owned(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        owner_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        self.fail_turn_and_clear_runtime_atomically(
            session_id,
            error_message,
            AtomicTurnFailureMode::MatchingRuntime {
                token,
                active_turn_generation,
                owner_generation,
                retain_runtime: false,
            },
        )
    }

    fn fail_turn_and_retain_runtime_if_owned(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        owner_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        self.fail_turn_and_clear_runtime_atomically(
            session_id,
            error_message,
            AtomicTurnFailureMode::MatchingRuntime {
                token,
                active_turn_generation,
                owner_generation,
                retain_runtime: true,
            },
        )
    }

    fn fail_turn_and_clear_runtime_atomically(
        &self,
        session_id: &str,
        error_message: &str,
        mode: AtomicTurnFailureMode<'_>,
    ) -> Result<bool> {
        // Deliberately omit the runtime token: MatchingRuntime already owns
        // the exact stop fence, while rejected delivery and missing-runtime
        // recovery have no live handle to match. Passing a token would make
        // the checkpoint helper refuse the owned stop-in-progress case.
        let checkpoint_token = None;
        self.checkpoint_engram_turn_off_lock(
            session_id,
            checkpoint_token,
            None,
            EngramNextIntent::Wait,
            None,
        );
        let cleaned = error_message.trim();
        let preserve_accepted_turn_state = mode.preserves_accepted_turn_state();
        let (
            commit_result,
            created_messages,
            updated_messages,
            has_queued_prompts,
            failed_mailbox_notification,
            deferred_stop_callbacks,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let guard_matches = match mode {
                AtomicTurnFailureMode::RejectedDelivery {
                    token,
                    active_turn_generation,
                    owner_generation,
                } => {
                    inner.sessions[index].runtime.matches_runtime_token(token)
                        && inner.sessions[index].active_turn_generation
                            == active_turn_generation
                        && matches!(
                        inner.sessions[index].session.status,
                        SessionStatus::Active | SessionStatus::Approval
                    ) && inner.sessions[index].runtime_stop_is_owned_by(
                        RuntimeStopOwnerKind::LostRuntimeTerminalization,
                        token,
                        owner_generation,
                    )
                }
                AtomicTurnFailureMode::MatchingRuntime {
                    token,
                    active_turn_generation,
                    owner_generation,
                    ..
                } => {
                    inner.sessions[index].runtime.matches_runtime_token(token)
                        && inner.sessions[index].active_turn_generation
                            == active_turn_generation
                        && matches!(
                            inner.sessions[index].session.status,
                            SessionStatus::Active | SessionStatus::Approval
                        )
                        && inner.sessions[index].runtime_stop_is_owned_by(
                            RuntimeStopOwnerKind::LostRuntimeTerminalization,
                            token,
                            owner_generation,
                        )
                }
                AtomicTurnFailureMode::MissingRuntime {
                    active_turn_generation,
                    owner_generation,
                } => {
                    matches!(
                        inner.sessions[index].session.status,
                        SessionStatus::Active | SessionStatus::Approval
                    ) && matches!(inner.sessions[index].runtime, SessionRuntime::None)
                        && inner.sessions[index].active_turn_generation
                            == active_turn_generation
                        && inner.sessions[index].missing_runtime_stop_is_owned_by(
                            RuntimeStopOwnerKind::LostRuntimeTerminalization,
                            owner_generation,
                        )
                }
            };
            if !guard_matches {
                let still_owns_terminalization = match mode {
                    AtomicTurnFailureMode::MissingRuntime {
                        owner_generation, ..
                    } => inner.sessions[index].missing_runtime_stop_is_owned_by(
                            RuntimeStopOwnerKind::LostRuntimeTerminalization,
                            owner_generation,
                        ),
                    AtomicTurnFailureMode::MatchingRuntime {
                        token,
                        owner_generation,
                        ..
                    } => inner.sessions[index].runtime_stop_is_owned_by(
                        RuntimeStopOwnerKind::LostRuntimeTerminalization,
                        token,
                        owner_generation,
                    ),
                    AtomicTurnFailureMode::RejectedDelivery {
                        token,
                        owner_generation,
                        ..
                    } => inner.sessions[index].runtime_stop_is_owned_by(
                        RuntimeStopOwnerKind::LostRuntimeTerminalization,
                        token,
                        owner_generation,
                    ),
                };
                let deferred_stop_callbacks = if still_owns_terminalization {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.clear_runtime_stop();
                    std::mem::take(&mut record.deferred_stop_callbacks)
                } else {
                    Vec::new()
                };
                drop(inner);
                if let Some(token) = mode.runtime_token() {
                    self.replay_deferred_runtime_stop_callbacks(
                        session_id,
                        token,
                        deferred_stop_callbacks,
                    );
                }
                return Ok(false);
            }
            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let file_change_message_id = (preserve_accepted_turn_state
                && !inner.sessions[index].active_turn_file_changes.is_empty())
            .then(|| inner.next_message_id());
            let (
                created_messages,
                updated_messages,
                has_queued_prompts,
                failed_mailbox_notification,
                deferred_stop_callbacks,
            ) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let active_mailbox_notification = record.active_turn_mailbox_notification.take();
                if active_mailbox_notification.is_some()
                    && !matches!(mode, AtomicTurnFailureMode::RejectedDelivery { .. })
                {
                    // A failed mailbox turn is recoverable, but immediately
                    // draining the restored wake would retry the same poisoned
                    // queue head forever and starve every prompt behind it.
                    record.set_auto_dispatch_blocked(true);
                }
                // Synchronous command-channel rejection is requeued by
                // `record_rejected_turn_dispatch`, which still owns the exact
                // dispatch metadata. Post-accept failures own the record-held
                // delivery here.
                let failed_mailbox_notification = (!matches!(
                    mode,
                    AtomicTurnFailureMode::RejectedDelivery { .. }
                ))
                .then_some(active_mailbox_notification)
                .flatten();
                take_and_abandon_engram_pending_dispatch(record);
                let deferred_stop_callbacks = if mode.retained_runtime_token().is_some() {
                    record.clear_runtime_stop();
                    std::mem::take(&mut record.deferred_stop_callbacks)
                } else {
                    record.clear_runtime();
                    record.clear_runtime_reset();
                    record.clear_runtime_stop();
                    record.deferred_stop_callbacks.clear();
                    Vec::new()
                };
                let pending_interaction_indices =
                    cancel_pending_interaction_messages(&mut record.session.messages);
                clear_all_pending_requests(record);

                let mut created_message_indices = Vec::new();
                if let Some(message_id) = message_id {
                    let message_index = record.session.messages.len();
                    push_message_on_record(
                        record,
                        Message::Text {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            text: format!("Turn failed: {cleaned}"),
                            expanded_text: None,
                            source: None,
                        },
                    );
                    created_message_indices.push(message_index);
                }
                if let Some(message_id) = file_change_message_id {
                    let message_index = record.session.messages.len();
                    if push_active_turn_file_changes_on_record(record, message_id) {
                        created_message_indices.push(message_index);
                    }
                }
                record.session.status = SessionStatus::Error;
                record.session.preview = make_preview(cleaned);
                if preserve_accepted_turn_state {
                    finish_active_turn_file_change_tracking(record);
                } else {
                    clear_active_turn_file_change_tracking(record);
                }
                (
                    message_created_delta_parts_for_indices(record, created_message_indices),
                    message_updated_delta_parts_for_indices(
                        record,
                        pending_interaction_indices,
                    ),
                    !record.queued_prompts.is_empty(),
                    failed_mailbox_notification,
                    deferred_stop_callbacks,
                )
            };

            let commit_result = self.commit_persisted_delta_locked(&mut inner);
            if commit_result.is_err() {
                // The dead runtime cannot be restored. Keep memory
                // authoritative and make the terminal state visible even
                // when the persistence fallback itself failed.
                self.publish_state_locked(&inner);
            } else if created_messages.is_empty() && updated_messages.is_empty() {
                // `commit_persisted_delta_locked` does not broadcast by itself.
                // A message-less terminal transition still has to publish its
                // Error/runtime state at the committed revision.
                self.publish_state_locked(&inner);
            }
            (
                commit_result,
                created_messages,
                updated_messages,
                has_queued_prompts,
                failed_mailbox_notification,
                deferred_stop_callbacks,
            )
        };

        if let Some(token) = mode.retained_runtime_token() {
            self.replay_deferred_runtime_stop_callbacks(
                session_id,
                token,
                deferred_stop_callbacks,
            );
        }
        if let Some(notification) = failed_mailbox_notification.as_ref() {
            if let Err(error) = self.requeue_rejected_mailbox_notification(notification) {
                eprintln!(
                    "mailbox> failed restoring the terminalized wake for `{}` / `{}`: {error:#}",
                    notification.session_id, notification.mailbox_id
                );
            }
        }
        let revision = commit_result?;
        self.publish_message_created_delta_parts(revision, created_messages);
        self.publish_message_updated_delta_parts(revision, updated_messages);
        if let Err(error) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!(
                "state warning> failed to refresh delegation after atomic turn failure: {error:#}"
            );
        }
        self.resume_pending_orchestrator_transitions()?;
        if preserve_accepted_turn_state && has_queued_prompts {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|error| {
                    anyhow!("failed to deliver queued turn dispatch: {}", error.message)
                })?;
            }
        }
        Ok(true)
    }

    /// Applies a shared-Codex failure to the matching runtime, then recovers
    /// the same turn if the runtime handle vanished before the late response
    /// arrived. Both guards reject a live successor runtime.
    fn fail_turn_if_runtime_matches_or_missing(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        error_message: &str,
    ) -> Result<bool> {
        if let Some(owner_generation) = self.claim_turn_terminalization_if_runtime_matches(
            session_id,
            token,
            active_turn_generation,
        )? {
            return self.fail_turn_and_retain_runtime_if_owned(
                session_id,
                token,
                active_turn_generation,
                owner_generation,
                error_message,
            );
        }
        self.fail_active_turn_if_runtime_missing(
            session_id,
            active_turn_generation,
            error_message,
        )
    }

    /// Records a retry attempt on the transcript without ending the turn.
    /// Runtime-token guarded: stale or stopping runtimes return `false`.
    /// Keeps the turn
    /// in Active (or leaves Approval alone) and refreshes the preview
    /// while the runtime retries under the covers. De-duplicates
    /// consecutive identical retry messages so repeated retries don't
    /// spam the transcript.
    fn note_turn_retry_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        detail: &str,
    ) -> Result<bool> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(false);
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let duplicate_last_message = {
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(false);
            }
            if record.runtime_stop_in_progress {
                return Ok(false);
            }
            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Ok(false);
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
                source: None,
            });
        }

        record.session.preview = make_preview(cleaned);
        self.commit_locked(&mut inner)?;
        Ok(true)
    }

    /// Returns whether a delayed retry still belongs to the current live
    /// runtime and the turn has not entered its stop window.
    fn turn_retry_allowed_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| {
                record.runtime.matches_runtime_token(token)
                    && !record.runtime_stop_in_progress
                    && matches!(
                        record.session.status,
                        SessionStatus::Active | SessionStatus::Approval
                    )
            })
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
        self.mark_turn_error_if_runtime_matches_guarded(session_id, token, None, error_message)
    }

    fn mark_turn_error_if_runtime_and_generation_match(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        error_message: &str,
    ) -> Result<()> {
        self.mark_turn_error_if_runtime_matches_guarded(
            session_id,
            token,
            Some(active_turn_generation),
            error_message,
        )
    }

    fn mark_turn_error_if_runtime_matches_guarded(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        expected_active_turn_generation: Option<u64>,
        error_message: &str,
    ) -> Result<()> {
        self.checkpoint_engram_turn_off_lock(
            session_id,
            Some(token),
            expected_active_turn_generation,
            EngramNextIntent::Wait,
            None,
        );
        let cleaned = error_message.trim();
        let (commit_result, should_dispatch_next, failed_mailbox_notification) = {
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
            if expected_active_turn_generation.is_some_and(|generation| {
                record.active_turn_generation != generation
            }) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnError {
                        active_turn_generation: record.active_turn_generation,
                        message: cleaned.to_owned(),
                    });
                return Ok(());
            }
            take_and_abandon_engram_pending_dispatch(record);
            let failed_mailbox_notification = record.active_turn_mailbox_notification.take();
            if failed_mailbox_notification.is_some() {
                record.set_auto_dispatch_blocked(true);
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
            (
                self.commit_locked(&mut inner),
                has_queued_prompts,
                failed_mailbox_notification,
            )
        };
        if let Some(notification) = failed_mailbox_notification.as_ref() {
            if let Err(error) = self.requeue_rejected_mailbox_notification(notification) {
                eprintln!(
                    "mailbox> failed restoring the errored wake for `{}` / `{}`: {error:#}",
                    notification.session_id, notification.mailbox_id
                );
            }
        }
        commit_result?;
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
        self.finish_turn_ok_if_runtime_matches_guarded(session_id, token, None)
    }

    fn finish_turn_ok_if_runtime_and_generation_match(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
    ) -> Result<()> {
        self.finish_turn_ok_if_runtime_matches_guarded(
            session_id,
            token,
            Some(active_turn_generation),
        )
    }

    fn finish_turn_ok_if_runtime_matches_guarded(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        expected_active_turn_generation: Option<u64>,
    ) -> Result<()> {
        self.checkpoint_successful_engram_turn_off_lock(
            session_id,
            token,
            expected_active_turn_generation,
        );
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
                if matches!(token, RuntimeToken::Codex(_)) {
                    trace_shared_codex_event(
                        "finish_noop",
                        "state/finish_turn",
                        Some(session_id),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("runtime_mismatch"),
                    );
                }
                return Ok(());
            }
            if expected_active_turn_generation.is_some_and(|generation| {
                record.active_turn_generation != generation
            }) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                if matches!(token, RuntimeToken::Codex(_)) {
                    trace_shared_codex_event(
                        "finish_deferred",
                        "state/finish_turn",
                        Some(session_id),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("runtime_stop_in_progress"),
                    );
                }
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnCompleted {
                        active_turn_generation: record.active_turn_generation,
                    });
                return Ok(());
            }
            take_and_abandon_engram_pending_dispatch(record);

            if record.session.status == SessionStatus::Active {
                record.session.status = SessionStatus::Idle;
            }
            record.session.live_activity = None;
            if matches!(token, RuntimeToken::Codex(_)) {
                trace_shared_codex_event(
                    "finish_apply",
                    "state/finish_turn",
                    Some(session_id),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some("status_idle"),
                );
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
        self.handle_runtime_exit_if_matches_guarded(session_id, token, None, error_message)
    }

    fn handle_runtime_exit_if_runtime_and_generation_match(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        active_turn_generation: u64,
        error_message: Option<&str>,
    ) -> Result<()> {
        self.handle_runtime_exit_if_matches_guarded(
            session_id,
            token,
            Some(active_turn_generation),
            error_message,
        )
    }

    fn handle_runtime_exit_if_matches_guarded(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        expected_active_turn_generation: Option<u64>,
        error_message: Option<&str>,
    ) -> Result<()> {
        let is_quarantined_exit = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .is_some_and(|record| {
                    record.runtime.matches_runtime_token(token)
                        && expected_active_turn_generation.is_none_or(|generation| {
                            record.active_turn_generation == generation
                        })
                        && record.engram_mcp_runtime_quarantined
                })
        };
        if !is_quarantined_exit {
            self.checkpoint_engram_turn_off_lock(
                session_id,
                Some(token),
                expected_active_turn_generation,
                EngramNextIntent::Wait,
                None,
            );
        }
        let cleaned = error_message.map(str::trim).unwrap_or("");
        let (
            should_dispatch_next,
            pending_interaction_updates,
            created_messages,
            exited_mailbox_notification,
            commit_result,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let matches_runtime = inner.sessions[index].runtime.matches_runtime_token(token);
            if !matches_runtime {
                return Ok(());
            }
            if expected_active_turn_generation.is_some_and(|generation| {
                inner.sessions[index].active_turn_generation != generation
            }) {
                return Ok(());
            }
            if inner.sessions[index].runtime_stop_in_progress {
                let active_turn_generation = inner.sessions[index].active_turn_generation;
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::RuntimeExited {
                        active_turn_generation,
                        message: error_message.map(str::to_owned),
                    });
                return Ok(());
            }
            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let preserve_automatic_resume_block =
                inner.sessions[index].engram_mcp_runtime_quarantined
                    && inner.sessions[index].orchestrator_auto_dispatch_blocked;
            let quarantined_exit = inner.sessions[index].engram_mcp_runtime_quarantined;
            let message_id = (!quarantined_exit && (was_busy || !cleaned.is_empty()))
                .then(|| inner.next_message_id());
            let detail = if quarantined_exit {
                None
            } else if !cleaned.is_empty() || was_busy {
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
            let (
                has_queued_prompts,
                pending_interaction_updates,
                created_messages,
                exited_mailbox_notification,
            ) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let exited_mailbox_notification = was_busy
                    .then(|| record.active_turn_mailbox_notification.take())
                    .flatten();
                take_and_abandon_engram_pending_dispatch(record);
                record.clear_runtime();
                record.clear_runtime_reset();
                record.set_auto_dispatch_blocked(
                    preserve_automatic_resume_block || exited_mailbox_notification.is_some(),
                );
                record.clear_runtime_stop();
                record.deferred_stop_callbacks.clear();
                if quarantined_exit {
                    // This is the delayed success condition for a runtime that
                    // earlier failed revocation teardown. Keep the explicit
                    // automatic-resume latch, but do not turn the expected exit
                    // into a second failure message.
                    record.session.status = SessionStatus::Idle;
                }
                let pending_interaction_indices =
                    cancel_pending_interaction_messages(&mut record.session.messages);
                let mut created_message_indices = Vec::new();
                clear_all_pending_requests(record);
                if let Some(detail) = detail.as_ref() {
                    if let Some(message_id) = message_id {
                        let failed_message_index = record.session.messages.len();
                        record.session.messages.push(Message::Text {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            text: format!("Turn failed: {detail}"),
                            expanded_text: None,
                            source: None,
                        });
                        created_message_indices.push(failed_message_index);
                    }
                    record.session.status = SessionStatus::Error;
                    record.session.preview = make_preview(detail);
                }
                if let Some(message_id) = file_change_message_id {
                    let file_change_message_index = record.session.messages.len();
                    if push_active_turn_file_changes_on_record(record, message_id) {
                        created_message_indices.push(file_change_message_index);
                    }
                }
                finish_active_turn_file_change_tracking(record);
                (
                    !record.orchestrator_auto_dispatch_blocked
                        && !record.queued_prompts.is_empty(),
                    message_updated_delta_parts_for_indices(record, pending_interaction_indices),
                    message_created_delta_parts_for_indices(record, created_message_indices),
                    exited_mailbox_notification,
                )
            };
            let commit_result = self.commit_locked(&mut inner);
            if commit_result.is_err() {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.set_auto_dispatch_blocked(true);
                    clear_active_turn_file_change_tracking(record);
            }
            (
                has_queued_prompts,
                pending_interaction_updates,
                created_messages,
                exited_mailbox_notification,
                commit_result,
            )
        };
        if let Some(notification) = exited_mailbox_notification.as_ref() {
            if let Err(error) = self.requeue_rejected_mailbox_notification(notification) {
                eprintln!(
                    "mailbox> failed restoring the runtime-exited wake for `{}` / `{}`: {error:#}",
                    notification.session_id, notification.mailbox_id
                );
            }
        }
        let revision = commit_result?;
        self.publish_message_created_delta_parts(revision, created_messages);
        self.publish_message_updated_delta_parts(revision, pending_interaction_updates);

        if let Err(err) = self.refresh_delegation_for_child_session(session_id) {
            eprintln!("state warning> failed to refresh delegation after runtime exit: {err:#}");
        }

        // A shared Codex process hosts multiple logical sessions. Once a
        // runtime-exit path has cleared this session's stale handle, any queued
        // prompt must spawn or attach to a fresh shared app-server rather than
        // reusing the dying runtime still stored on AppState.
        let clear_codex_runtime_result = if should_dispatch_next {
            match token {
                RuntimeToken::Codex(runtime_id) => {
                    Some(self.clear_shared_codex_runtime_if_matches(runtime_id))
                }
                RuntimeToken::Claude(_) | RuntimeToken::Acp(_) => None,
            }
        } else {
            None
        };

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
        if let Some(result) = clear_codex_runtime_result {
            result?;
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

    /// Stores a Claude AskUserQuestion dialog keyed by transcript message id
    /// so the generic user-input route can answer the still-running runtime.
    fn register_claude_pending_user_input(
        &self,
        session_id: &str,
        message_id: String,
        request: ClaudePendingUserInput,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_user_inputs
            .insert(message_id, request);
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
    /// `submit_user_input` in `src/state.rs` looks it up when the
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
        let record = &mut inner.sessions[index];
        record
            .pending_acp_approvals
            .insert(message_id.clone(), approval);
        record.pending_acp_approval_order.push_back(message_id);
        Ok(())
    }

    /// Drops any Claude pending approval or user-input entries matching
    /// `request_id` and marks the backing transcript messages as canceled. Claude's
    /// cancellation events carry a `request_id` (the Claude CLI's internal
    /// identifier) rather than the `message_id` that keys the register —
    /// so this is the only clear path that walks the map to find matching
    /// entries instead of looking up by the store's key directly.
    fn clear_claude_pending_interaction_by_request(
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
        let approval_message_ids: Vec<String> = record
            .pending_claude_approvals
            .iter()
            .filter(|(_, approval)| approval.request_id == request_id)
            .map(|(message_id, _)| message_id.clone())
            .collect();
        let user_input_message_ids: Vec<String> = record
            .pending_claude_user_inputs
            .iter()
            .filter(|(_, request)| request.request_id == request_id)
            .map(|(message_id, _)| message_id.clone())
            .collect();

        if approval_message_ids.is_empty() && user_input_message_ids.is_empty() {
            return Ok(());
        }

        let mut changed_message_indices = Vec::new();
        for message_id in &approval_message_ids {
            changed_message_indices.push(set_approval_decision_on_record(
                record,
                message_id,
                ApprovalDecision::Canceled,
            )?);
            record.pending_claude_approvals.remove(message_id);
        }
        for message_id in &user_input_message_ids {
            changed_message_indices.push(set_user_input_request_state_on_record(
                record,
                message_id,
                InteractionRequestState::Canceled,
                None,
            )?);
            record.pending_claude_user_inputs.remove(message_id);
        }

        let resolved_preview = if !user_input_message_ids.is_empty() {
            user_input_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Canceled,
            )
        } else {
            approval_preview_text(record.session.agent.name(), ApprovalDecision::Canceled)
        };
        sync_session_interaction_state(record, resolved_preview);
        let updates = message_updated_delta_parts_for_indices(record, changed_message_indices);
        let revision = self.commit_locked(&mut inner)?;
        drop(inner);
        self.publish_message_updated_delta_parts(revision, updates);
        Ok(())
    }
}
