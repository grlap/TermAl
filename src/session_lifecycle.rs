// Session lifecycle: the kill/stop/cancel-queued-prompt entry points on
// `AppState`. Three operations, three levels of destructiveness:
//
//   kill_session                > stop_session                > cancel_queued_prompt
//   (tears runtime + record)      (stops turn, keeps record)    (drops one queued prompt)
//
// `stop_session` has a `_with_options` variant because the user-facing default
// suppresses every automatic continuation after Stop, while narrowly scoped
// internal callers may opt into queue dispatch. Orchestrator cleanup also tags
// the stop as part of an instance's cleanup wave so transition scheduling skips
// the session while cleanup is in flight. Callers outside orchestrator cleanup
// use the default options and go through the plain `stop_session` wrapper. See
// `src/tests/orchestrator.rs::aborted_stop_*` for orchestrator-stop invariants.
//
// Each route branches on the session's runtime: Claude dedicated runtimes are
// killed by terminating the child process; Codex sessions on the shared
// app-server send a `turn/interrupt` JSON-RPC and detach the session (the
// shared helper keeps running for the other sessions still attached to it);
// ACP sessions send a `cancel` notification to the ACP agent. See
// `src/session_runtime.rs::shutdown_removed_runtime` + `KillableRuntime`.
//
// Stop semantics are non-trivial because the runtime may not confirm stop
// immediately (or at all). `stop_session_with_options` sets
// `runtime_stop_in_progress` on the `SessionRecord` before dispatching the
// stop; during this window, incoming runtime callbacks for the stopping
// session (`turn_completed`, `runtime_exit`, ...) get buffered onto
// `deferred_stop_callbacks` rather than applied inline — applying them
// mid-stop would race the stop machinery. See
// `src/state.rs::handle_shared_codex_runtime_exit` + the deferred-callback
// replay path in `src/tests/session_stop.rs`. If a second stop arrives while
// the user-facing asynchronous Stop is still in flight, the route returns the
// same `Stopping` snapshot without starting another worker. Synchronous
// internal cleanup retains the stricter 409 contract (see
// `src/tests/session_stop_runtime.rs::stop_session_returns_conflict_when_already_stopping`).
//
// Remote proxying: if `session.remote_target` is set, each route short-circuits
// to the remote backend (`proxy_remote_kill_session`,
// `proxy_remote_cancel_queued_prompt`, `proxy_remote_stop_session` in
// `src/remote.rs`) and never touches a local runtime.

#[cfg(test)]
struct TestStopFenceGate {
    claimed_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
type TestStopFenceGateKey = (usize, String);

#[cfg(test)]
static TEST_STOP_FENCE_GATES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<TestStopFenceGateKey, TestStopFenceGate>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
struct TestStopFenceGateControl {
    key: TestStopFenceGateKey,
    claimed_rx: std::sync::mpsc::Receiver<()>,
    release_tx: std::sync::mpsc::Sender<()>,
}

#[cfg(test)]
impl TestStopFenceGateControl {
    fn wait_until_claimed(&self) {
        self.claimed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("Stop should claim its callback fence");
    }

    fn release(&self) {
        self.release_tx
            .send(())
            .expect("Stop gate should remain connected");
    }
}

#[cfg(test)]
impl Drop for TestStopFenceGateControl {
    fn drop(&mut self) {
        TEST_STOP_FENCE_GATES
            .lock()
            .expect("test Stop fence gate mutex poisoned")
            .remove(&self.key);
    }
}

#[cfg(test)]
fn test_stop_fence_gate_key(state: &AppState, session_id: &str) -> TestStopFenceGateKey {
    (Arc::as_ptr(&state.inner) as usize, session_id.to_owned())
}

#[cfg(test)]
fn install_test_stop_fence_gate(
    state: &AppState,
    session_id: &str,
) -> TestStopFenceGateControl {
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let key = test_stop_fence_gate_key(state, session_id);
    TEST_STOP_FENCE_GATES
        .lock()
        .expect("test Stop fence gate mutex poisoned")
        .insert(
            key.clone(),
            TestStopFenceGate {
                claimed_tx,
                release_rx,
            },
        );
    TestStopFenceGateControl {
        key,
        claimed_rx,
        release_tx,
    }
}

#[cfg(test)]
fn wait_at_test_stop_fence_gate(state: &AppState, session_id: &str) {
    let gate = TEST_STOP_FENCE_GATES
        .lock()
        .expect("test Stop fence gate mutex poisoned")
        .remove(&test_stop_fence_gate_key(state, session_id));
    if let Some(gate) = gate {
        gate.claimed_tx
            .send(())
            .expect("test Stop fence observer should remain connected");
        gate.release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("test Stop fence gate should be released");
    }
}

#[derive(Clone)]
struct RequestedStopClaim {
    runtime_token: Option<RuntimeToken>,
    owner_generation: u64,
}

impl AppState {
    /// Destructively removes a session: tears down its runtime (kill
    /// child process for Claude/ACP, `turn/interrupt` + detach for shared
    /// Codex), removes the `SessionRecord` from `StateInner`, garbage-collects
    /// any delegated child sessions it owns transitively, suppresses rediscovery
    /// of detached Codex threads, and persists the new state. No undo. Triggered
    /// from the UI trash icon. Proxied to the remote backend when
    /// `session.remote_target` is set.
    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_kill_session(session_id);
        }
        let engram_session_ids_to_terminate = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut collected = vec![session_id.to_owned()];
            let mut cursor = 0;
            while cursor < collected.len() {
                let parent_session_id = collected[cursor].clone();
                for delegation in &inner.delegations {
                    if delegation.parent_session_id == parent_session_id
                        && !collected.contains(&delegation.child_session_id)
                    {
                        collected.push(delegation.child_session_id.clone());
                    }
                }
                cursor += 1;
            }
            collected
        };
        for terminating_session_id in &engram_session_ids_to_terminate {
            self.checkpoint_engram_turn_off_lock(
                terminating_session_id,
                None,
                None,
                EngramNextIntent::Exit,
                None,
            );
            self.wait_for_engram_checkpoint_completion(terminating_session_id);
            self.shutdown_engram_session_process_if_bound(terminating_session_id);
        }
        let (
            runtime_to_kill,
            delegation_runtimes_to_kill,
            revision,
            delegation_lifecycle_deltas,
            delegation_wait_refresh,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let agent = inner.sessions[index].session.agent;
            let external_session_id = inner.sessions[index].external_session_id.clone();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
                SessionRuntime::None => None,
            };
            let delegation_reconciliation =
                reconcile_delegations_for_removed_session_locked(&mut inner, session_id);
            // Delegation reconciliation may remove child sessions before the
            // requested parent is removed, which can shift session indexes.
            let index = inner
                .find_visible_session_index(session_id)
                .expect("session should still exist after delegation reconciliation");
            inner.remove_session_at(index);

            for thread_id in &delegation_reconciliation.codex_thread_ids_to_ignore {
                inner.ignore_discovered_codex_thread(Some(thread_id));
            }

            if agent.supports_codex_prompt_settings() {
                inner.ignore_discovered_codex_thread(external_session_id.as_deref());
            }
            inner.normalize_orchestrator_instances();
            let delegation_wait_refresh = refresh_delegation_waits_locked(&mut inner);

            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            (
                runtime,
                delegation_reconciliation.runtimes_to_kill,
                revision,
                delegation_reconciliation.lifecycle_deltas,
                delegation_wait_refresh,
            )
        };

        if let Err(err) = self.mailbox_store.mark_session_left(session_id) {
            eprintln!(
                "mailbox cleanup> failed to mark deleted session `{session_id}` as left: {err:#}"
            );
        }

        if let Err(err) = self.prune_telegram_state_for_deleted_session(session_id) {
            eprintln!(
                "telegram settings> failed to prune deleted session `{session_id}`: {}",
                err.message
            );
        }

        if let Some(runtime) = runtime_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, &format!("session `{session_id}`"))
            {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for runtime in delegation_runtimes_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, "a removed parent delegation child")
            {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for delta in delegation_lifecycle_deltas {
            self.publish_delegation_lifecycle_delta(revision, delta);
        }
        if delegation_wait_refresh.did_mutate() {
            self.publish_delegation_wait_consumed_deltas(
                revision,
                &delegation_wait_refresh.consumed_waits,
            );
        }
        self.dispatch_delegation_wait_resumes(revision, delegation_wait_refresh.dispatch_parents);

        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to reconcile orchestrator transitions: {err:#}"
                ))
            })?;
        Ok(self.snapshot())
    }

    /// Non-destructively removes a single queued prompt from a session
    /// without touching the currently-running turn or the runtime. Matches
    /// the queued prompt by its `pending_prompt.id`; returns 404 if no
    /// queued prompt with that id exists on the session. Used when the user
    /// has queued multiple prompts and wants to cancel one specific entry.
    /// Proxied to the remote backend when `session.remote_target` is set.
    fn cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_cancel_queued_prompt(session_id, prompt_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let original_len = record.queued_prompts.len();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        if record.queued_prompts.len() == original_len {
            return Err(ApiError::not_found("queued prompt not found"));
        }
        sync_pending_prompts(record);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        drop(inner);
        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to reconcile orchestrator transitions: {err:#}"
                ))
            })?;
        Ok(self.snapshot())
    }

    /// Public entry point for stopping a session's current turn while
    /// keeping the session alive. Convenience wrapper around
    /// `stop_session_with_options` with `StopSessionOptions::default()`
    /// (leave queued work paused on success, not part of an orchestrator
    /// cleanup wave).
    #[cfg_attr(not(test), allow(dead_code))]
    fn stop_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        self.stop_session_with_options(session_id, StopSessionOptions::default())
    }

    /// Starts the user-facing Stop operation and returns the persisted
    /// `Stopping` snapshot without waiting for the agent interrupt or Engram
    /// checkpoint. The runtime-stop ownership claim is established before the
    /// state becomes visible, so runtime callbacks are deferred throughout the
    /// handoff to the background worker. Repeated requests while that claim is
    /// active are idempotent.
    fn request_stop_session(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_stop_session(session_id);
        }

        let options = StopSessionOptions::default();
        let (response, claim) = self.begin_requested_stop_session(session_id, &options)?;
        let Some(claim) = claim else {
            return Ok(response);
        };

        let state = self.clone();
        let worker_session_id = session_id.to_owned();
        let worker_claim = claim.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("termal-stop-{session_id}"))
            .spawn(move || {
                if let Err(error) = state.stop_local_session_with_options(
                    &worker_session_id,
                    options,
                    Some(worker_claim.clone()),
                ) {
                    state.record_requested_stop_failure(
                        &worker_session_id,
                        &worker_claim,
                        &error.message,
                    );
                }
            })
        {
            let detail = format!("failed to start background Stop worker: {error}");
            self.record_requested_stop_failure(session_id, &claim, &detail);
            return Err(ApiError::internal(detail));
        }

        Ok(response)
    }

    fn begin_requested_stop_session(
        &self,
        session_id: &str,
        options: &StopSessionOptions,
    ) -> std::result::Result<(StateResponse, Option<RequestedStopClaim>), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;

        if inner.sessions[index].session.status == SessionStatus::Stopping {
            return Ok((self.snapshot_from_inner(&inner), None));
        }
        if inner.sessions[index].runtime_stop_in_progress {
            return Err(ApiError::conflict("session is already stopping"));
        }
        if !matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) {
            return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
        }

        let original = inner.sessions[index].clone();
        let runtime_token = inner.sessions[index].runtime.runtime_token();
        let owner_generation = {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let owner_generation = match runtime_token.as_ref() {
                Some(runtime_token) => record.claim_runtime_stop(
                    RuntimeStopOwnerKind::UserStop,
                    runtime_token.clone(),
                ),
                None => record.claim_missing_runtime_stop(RuntimeStopOwnerKind::UserStop),
            };
            record.session.status = SessionStatus::Stopping;
            record.session.preview = SESSION_STOPPING_MESSAGE.to_owned();
            if options.pause_automatic_resumes_on_success {
                record.orchestrator_auto_dispatch_blocked = true;
                clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
            }
            owner_generation
        };

        if let Err(error) = self.commit_locked(&mut inner) {
            inner.sessions[index] = original;
            return Err(ApiError::internal(format!(
                "failed to persist stopping session state: {error:#}"
            )));
        }
        let response = self.snapshot_from_inner(&inner);
        Ok((
            response,
            Some(RequestedStopClaim {
                runtime_token,
                owner_generation,
            }),
        ))
    }

    fn record_requested_stop_failure(
        &self,
        session_id: &str,
        claim: &RequestedStopClaim,
        detail: &str,
    ) {
        let cleaned = detail.trim();
        let failure_text = if cleaned.is_empty() {
            "Stop failed in the background.".to_owned()
        } else {
            format!("Stop failed in the background: {cleaned}")
        };
        let (revision, creates, replay) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_visible_session_index(session_id) else {
                return;
            };
            if inner.sessions[index].session.status != SessionStatus::Stopping {
                return;
            }
            let owns_stop = match claim.runtime_token.as_ref() {
                Some(runtime_token) => inner.sessions[index].runtime_stop_is_owned_by(
                    RuntimeStopOwnerKind::UserStop,
                    runtime_token,
                    claim.owner_generation,
                ),
                None => inner.sessions[index].missing_runtime_stop_is_owned_by(
                    RuntimeStopOwnerKind::UserStop,
                    claim.owner_generation,
                ),
            };
            // A failing finalizer may already have released its own fence
            // before returning the error. Record that failure, but never let
            // an obsolete worker overwrite a newer stop owner's Stopping
            // state.
            if !owns_stop && inner.sessions[index].runtime_stop_in_progress {
                return;
            }
            let message_id = inner.next_message_id();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let replay = owns_stop.then(|| {
                let runtime_token = record.runtime.runtime_token();
                record.clear_runtime_stop();
                let callbacks = std::mem::take(&mut record.deferred_stop_callbacks);
                (runtime_token, callbacks)
            });
            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(&failure_text);
            let message_index = push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: failure_text,
                    expanded_text: None,
                    source: None,
                },
            );
            let creates = message_created_delta_parts_for_indices(record, vec![message_index]);
            let revision = match self.commit_locked(&mut inner) {
                Ok(revision) => revision,
                Err(error) => {
                    eprintln!(
                        "session stop> failed persisting background failure for `{session_id}`: {error:#}"
                    );
                    self.publish_state_locked(&inner);
                    inner.revision
                }
            };
            (revision, creates, replay)
        };
        self.publish_message_created_delta_parts(revision, creates);
        if let Some((Some(runtime_token), callbacks)) = replay {
            self.replay_deferred_runtime_stop_callbacks(session_id, &runtime_token, callbacks);
        }
    }

    /// Full stop implementation. Enters the `runtime_stop_in_progress`
    /// guard (returning HTTP 409 Conflict if a stop is already in flight),
    /// routes to the right runtime (Claude kill, Codex shared-app
    /// `turn/interrupt`, ACP `cancel` notification), defers terminal
    /// callbacks until the runtime confirms stop or times out, then clears
    /// the runtime, marks the session `Idle`, appends a "Turn stopped by
    /// user." message, and persists. Proxied to the remote backend when
    /// `session.remote_target` is set.
    ///
    /// Options:
    /// - `dispatch_queued_prompts_on_success`: if `true`, the next queued
    ///   prompt is auto-dispatched once the stop completes. The default is
    ///   `false`.
    /// - `pause_automatic_resumes_on_success`: if `true` (the default),
    ///   explicit Stop pauses automatic mailbox/workflow wakes and consumes
    ///   outstanding delegation waits until a new user prompt explicitly
    ///   resumes the session. Internal orchestrator cleanup sets this to
    ///   `false` so aborted cleanup can preserve its durable work queue.
    /// - `orchestrator_stop_instance_id`: if `Some`, tags the stop as part
    ///   of that orchestrator instance's cleanup wave so transition
    ///   scheduling skips the session while cleanup is in flight (the
    ///   session id is appended to `stopped_session_ids_during_stop` on
    ///   the instance).
    fn stop_session_with_options(
        &self,
        session_id: &str,
        options: StopSessionOptions,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_stop_session(session_id);
        }
        self.stop_local_session_with_options(session_id, options, None)
    }

    fn stop_local_session_with_options(
        &self,
        session_id: &str,
        options: StopSessionOptions,
        requested_claim: Option<RequestedStopClaim>,
    ) -> std::result::Result<StateResponse, ApiError> {
        let (runtime_to_stop, stop_failure_is_best_effort, stop_token, stop_owner_generation) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");

            if let Some(claim) = requested_claim.as_ref() {
                let owns_stop = match claim.runtime_token.as_ref() {
                    Some(runtime_token) => record.runtime_stop_is_owned_by(
                        RuntimeStopOwnerKind::UserStop,
                        runtime_token,
                        claim.owner_generation,
                    ),
                    None => record.missing_runtime_stop_is_owned_by(
                        RuntimeStopOwnerKind::UserStop,
                        claim.owner_generation,
                    ),
                };
                if !owns_stop || record.session.status != SessionStatus::Stopping {
                    return Err(ApiError::conflict(
                        "background Stop no longer owns the session",
                    ));
                }
            } else {
                if record.runtime_stop_in_progress {
                    return Err(ApiError::conflict("session is already stopping"));
                }

                if !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval
                ) {
                    return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
                }
            }

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
                SessionRuntime::None => None,
            };
            let stop_failure_is_best_effort = runtime
                .as_ref()
                .is_some_and(KillableRuntime::stop_failure_is_best_effort);
            let stop_token = record.runtime.runtime_token();

            // Preserve the public session status until the stop succeeds so borrowed state reads
            // never observe a contradictory transient Idle snapshot while shutdown is still pending.
            // `deferred_stop_callbacks` is guaranteed to be empty here because the guard above
            // already returned if `runtime_stop_in_progress` was true (and callbacks can only
            // defer when that flag is set).
            let stop_owner_generation = match requested_claim.as_ref() {
                Some(claim) => claim.owner_generation,
                None => match stop_token.as_ref() {
                    Some(stop_token) => record.claim_runtime_stop(
                        RuntimeStopOwnerKind::UserStop,
                        stop_token.clone(),
                    ),
                    None => record.claim_missing_runtime_stop(RuntimeStopOwnerKind::UserStop),
                },
            };

            (
                runtime,
                stop_failure_is_best_effort,
                stop_token,
                stop_owner_generation,
            )
        };

        #[cfg(test)]
        wait_at_test_stop_fence_gate(self, session_id);

        let clear_external_session_id = if let Some(runtime_to_stop) = runtime_to_stop {
            match shutdown_stopped_runtime(runtime_to_stop, &format!("session `{session_id}`")) {
                Ok(()) => false,
                Err(err) => {
                if stop_failure_is_best_effort {
                    let pending_revocation_target = {
                        let mut inner = self.inner.lock().expect("state mutex poisoned");
                        let index = inner
                            .find_visible_session_index(session_id)
                            .ok_or_else(|| ApiError::not_found("session not found"))?;
                        take_pending_engram_mcp_revocation_after_stop_failure_locked(
                            &mut inner,
                            index,
                            stop_owner_generation,
                            options.clone(),
                        )
                    };
                    if let Some(target) = pending_revocation_target {
                        // A shared Codex interrupt failure already detached the
                        // session. Finalize that exact failure under the
                        // transferred revocation fence instead of retrying an
                        // interrupt that can only no-op after detachment.
                        let mut outcome = self.finalize_revoked_engram_mcp_runtimes(
                            EngramMcpRuntimeRevocationShutdownBatch {
                                shutdowns: vec![EngramMcpRuntimeRevocationShutdown {
                                    target,
                                    shutdown_error: Some(format!(
                                        "shared Codex interrupt failed after detach; the old thread may remain alive with its prior MCP capabilities until Codex unloads it: {err:#}"
                                    )),
                                    retain_runtime_for_retry: false,
                                    suppress_codex_thread_resume: true,
                                }],
                                pending_session_ids: Vec::new(),
                            },
                        );
                        self.resume_revoked_engram_mcp_sessions(&mut outcome);
                        return match self.finish_revoked_engram_mcp_runtime_outcome(outcome) {
                            Ok(_) => Err(ApiError::internal(format!(
                                "failed to stop session `{session_id}` cleanly after shared Codex detach: {err:#}"
                            ))),
                            Err(cleanup_error) => Err(ApiError::internal(format!(
                                "failed to stop session `{session_id}` cleanly: {err:#}; {}",
                                cleanup_error.message
                            ))),
                        };
                    }
                    eprintln!(
                        "session cleanup warning> failed to stop session `{session_id}` cleanly: {err:#}"
                    );
                    true
                } else {
                    let (deferred_callbacks, token, pending_revocation_target) = {
                        let mut inner = self.inner.lock().expect("state mutex poisoned");
                        let index = inner
                            .find_visible_session_index(session_id)
                            .ok_or_else(|| ApiError::not_found("session not found"))?;
                        let pending_revocation_target =
                            take_pending_engram_mcp_revocation_after_stop_failure_locked(
                                &mut inner,
                                index,
                                stop_owner_generation,
                                options.clone(),
                            );
                        if pending_revocation_target.is_some() {
                            (Vec::new(), None, pending_revocation_target)
                        } else {
                            let record = inner
                                .session_mut_by_index(index)
                                .expect("session index should be valid");
                            if !record.runtime_stop_is_owned_by(
                                RuntimeStopOwnerKind::UserStop,
                                stop_token
                                    .as_ref()
                                    .expect("a failed runtime shutdown must retain its token"),
                                stop_owner_generation,
                            ) {
                                return Err(ApiError::internal(format!(
                                    "failed to stop session `{session_id}` cleanly after stop ownership changed: {err:#}"
                                )));
                            }
                            record.clear_runtime_stop();
                            let deferred_callbacks =
                                std::mem::take(&mut record.deferred_stop_callbacks);
                            let token = record.runtime.runtime_token();
                            (deferred_callbacks, token, None)
                        }
                    };

                    if let Some(target) = pending_revocation_target {
                        let cleanup_result = self.teardown_revoked_engram_mcp_runtimes(
                            EngramMcpRuntimeRevocationBatch {
                                targets: vec![target],
                                pending_session_ids: Vec::new(),
                                newly_pending_session_ids: Vec::new(),
                            },
                            "pending Engram MCP revocation after failed Stop",
                        );
                        return match cleanup_result {
                            Ok(_) => Ok(self.snapshot()),
                            Err(cleanup_error) => Err(ApiError::internal(format!(
                                "failed to stop session `{session_id}` cleanly: {err:#}; pending Engram MCP revocation also degraded: {}",
                                cleanup_error.message
                            ))),
                        };
                    }

                    // Replay any terminal callbacks that arrived during the failed shutdown window.
                    // The flag is now cleared so the callback methods will proceed normally.
                    if let Some(token) = token {
                        self.replay_deferred_runtime_stop_callbacks(
                            session_id,
                            &token,
                            deferred_callbacks,
                        );
                    }

                    return Err(ApiError::internal(format!(
                        "failed to stop session `{session_id}` cleanly: {err:#}"
                    )));
                }
            }
            }
        } else {
            false
        };
        let checkpoint_failure = match self.checkpoint_engram_turn_off_lock(
            session_id,
            None,
            None,
            EngramNextIntent::Wait,
            None,
        ) {
            EngramCheckpointOutcome::Failed(detail) => Some(detail),
            EngramCheckpointOutcome::Skipped | EngramCheckpointOutcome::Succeeded => None,
        };
        let prepared_queued_turn = options
            .dispatch_queued_prompts_on_success
            .then(|| self.prepare_next_queued_turn_engram_off_lock(session_id))
            .flatten();
        let orchestrator_stop_instance_id = options.orchestrator_stop_instance_id.clone();
        let suppress_automatic_resume = options.pause_automatic_resumes_on_success;
        let transition = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let owns_stop = match stop_token.as_ref() {
                Some(stop_token) => inner.sessions[index].runtime_stop_is_owned_by(
                    RuntimeStopOwnerKind::UserStop,
                    stop_token,
                    stop_owner_generation,
                ),
                None => inner.sessions[index].missing_runtime_stop_is_owned_by(
                    RuntimeStopOwnerKind::UserStop,
                    stop_owner_generation,
                ),
            };
            if !owns_stop {
                let pending_engram = prepared_queued_turn
                    .as_ref()
                    .and_then(|prepared| prepared.pending_engram.clone());
                if abandon_engram_pending_dispatch(
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid"),
                    pending_engram,
                ) {
                    if let Err(error) = self.commit_locked(&mut inner) {
                        inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid")
                            .orchestrator_auto_dispatch_blocked = true;
                        self.publish_state_locked(&inner);
                        return Err(ApiError::internal(format!(
                            "failed to persist abandoned queued dispatch after Stop ownership changed: {error:#}"
                        )));
                    }
                }
                drop(inner);
                return Ok(self.snapshot());
            }
            let message_id = inner.next_message_id();
            let file_change_message_id =
                (!inner.sessions[index].active_turn_file_changes.is_empty())
                    .then(|| inner.next_message_id());
            let mut thread_id_to_suppress = None;
            let (
                pending_interaction_indices,
                mut created_message_indices,
                stopped_mailbox_notification,
            ) = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                let stopped_mailbox_notification =
                    record.active_turn_mailbox_notification.take();
                take_and_abandon_engram_pending_dispatch(record);
                record.clear_runtime();
                record.clear_runtime_reset();
                record.clear_runtime_stop();
                record.deferred_stop_callbacks.clear();
                if suppress_automatic_resume {
                    record.orchestrator_auto_dispatch_blocked = true;
                    // A wait may already have completed and queued its fan-in
                    // before Stop acquired the state lock. Drop all automatic
                    // workflow continuations here; user and mailbox prompts
                    // remain durable but paused behind the explicit-resume
                    // latch.
                    clear_queued_prompts_by_source(record, QueuedPromptSource::Orchestrator);
                }
                let pending_interaction_indices =
                    cancel_pending_interaction_messages(&mut record.session.messages);
                let mut created_message_indices = Vec::new();
                clear_all_pending_requests(record);
                if clear_external_session_id {
                    // Interrupt failures can leave the detached Codex thread running, so any
                    // queued or future prompt must start a fresh thread instead of resuming it.
                    // Capture the thread id before clearing so we can suppress its rediscovery
                    // after the record borrow is released.
                    if record.session.agent.supports_codex_prompt_settings() {
                        thread_id_to_suppress = record.external_session_id.clone();
                    }
                    set_record_external_session_id(record, None);
                }
                let (terminal_status, terminal_text) = match checkpoint_failure.as_deref() {
                    Some(detail) => (
                        SessionStatus::Error,
                        format!(
                            "Stop completed, but the Engram checkpoint failed: {detail}"
                        ),
                    ),
                    None => (
                        SessionStatus::Idle,
                        SESSION_STOPPED_BY_USER_MESSAGE.to_owned(),
                    ),
                };
                record.session.status = terminal_status;
                record.session.preview = make_preview(&terminal_text);
                let stopped_message_index = record.session.messages.len();
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: terminal_text,
                    expanded_text: None,
                    source: None,
                });
                created_message_indices.push(stopped_message_index);
                if let Some(message_id) = file_change_message_id {
                    let file_change_message_index = record.session.messages.len();
                    if push_active_turn_file_changes_on_record(record, message_id) {
                        created_message_indices.push(file_change_message_index);
                    }
                }
                finish_active_turn_file_change_tracking(record);
                (
                    pending_interaction_indices,
                    created_message_indices,
                    stopped_mailbox_notification,
                )
            };

            // Suppress rediscovery of the detached thread after the record
            // borrow is released. Without this, the still-running thread
            // would resurface as a new imported session on the next
            // import_discovered_codex_threads pass.
            if let Some(ref thread_id) = thread_id_to_suppress {
                inner.ignore_discovered_codex_thread(Some(thread_id));
            }

            // Stop itself remains authoritative in memory if persistence
            // fails because the interrupted runtime cannot be resurrected.
            // Delegation waits are different: consuming them is only valid as
            // part of the durable Stop commit, so retain their exact ordering
            // for the commit-error rollback below.
            let delegation_waits_before_stop =
                suppress_automatic_resume.then(|| inner.delegation_waits.clone());
            let stopped_wait_refresh = if suppress_automatic_resume {
                consume_delegation_waits_for_stopped_parent_locked(&mut inner, session_id)
            } else {
                DelegationWaitRefresh::default()
            };

            let mut stopped_orchestrator_instance_index = None;
            let mut added_stopped_session_id = false;
            if let Some(orchestrator_instance_id) = orchestrator_stop_instance_id.as_deref() {
                if let Some(instance_index) = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == orchestrator_instance_id)
                {
                    stopped_orchestrator_instance_index = Some(instance_index);
                    let stopped_session_ids = &mut inner.orchestrator_instances[instance_index]
                        .stopped_session_ids_during_stop;
                    if !stopped_session_ids
                        .iter()
                        .any(|candidate| candidate == session_id)
                    {
                        stopped_session_ids.push(session_id.to_owned());
                        stopped_session_ids.sort();
                        added_stopped_session_id = true;
                    }
                }
            }
            let should_dispatch_next = options.dispatch_queued_prompts_on_success
                && prepared_queued_turn.as_ref().is_some_and(|prepared| {
                    inner.sessions[index].engram.dispatch_generation
                        == prepared.dispatch_generation
                        && inner.sessions[index]
                            .queued_prompts
                            .front()
                            .is_some_and(|queued| {
                                queued.pending_prompt.id == prepared.prompt_id
                            })
                });
            let post_stop_record = should_dispatch_next.then(|| inner.sessions[index].clone());
            let queued_turn_result = if should_dispatch_next {
                let pending_engram = prepared_queued_turn
                    .as_ref()
                    .and_then(|prepared| prepared.pending_engram.clone());
                let pending_engram_for_abandon = pending_engram.clone();
                let result = self
                    .start_next_queued_turn_locked(&mut inner, index, false, pending_engram)
                    .map_err(|err| ApiError::internal(format!("{err:#}")));
                match &result {
                    Ok(Some(_)) => {
                        let successor_message_index = inner.sessions[index]
                            .session
                            .messages
                            .len()
                            .checked_sub(1)
                            .expect("started queued turn should append one message");
                        created_message_indices.push(successor_message_index);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        inner.sessions[index] = post_stop_record
                            .as_ref()
                            .expect("queued dispatch should retain a rollback record")
                            .clone();
                    }
                }
                if !matches!(result, Ok(Some(_))) {
                    abandon_engram_pending_dispatch(
                        inner
                            .session_mut_by_index(index)
                            .expect("session index should remain valid"),
                        pending_engram_for_abandon,
                    );
                }
                result
            } else {
                if let Some(pending_engram) = prepared_queued_turn
                    .as_ref()
                    .and_then(|prepared| prepared.pending_engram.clone())
                {
                    abandon_engram_pending_dispatch(
                        inner
                            .session_mut_by_index(index)
                            .expect("session index should remain valid"),
                        Some(pending_engram),
                    );
                }
                Ok(None)
            };

            let (pending_interaction_updates, created_messages) = {
                let record = &inner.sessions[index];
                (
                    message_updated_delta_parts_for_indices(record, pending_interaction_indices),
                    message_created_delta_parts_for_indices(record, created_message_indices),
                )
            };

            match self.commit_locked(&mut inner) {
                Ok(revision) => Ok((
                    queued_turn_result,
                    pending_interaction_updates,
                    created_messages,
                    stopped_wait_refresh,
                    stopped_mailbox_notification,
                    revision,
                )),
                Err(err) => {
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

                    let queued_runtime_to_shutdown = match (
                        queued_turn_result.as_ref(),
                        &inner.sessions[index].runtime,
                    ) {
                        (Ok(Some(_)), SessionRuntime::Claude(handle)) => {
                            Some(KillableRuntime::Claude(handle.clone()))
                        }
                        (Ok(Some(_)), SessionRuntime::Codex(handle)) => {
                            Some(KillableRuntime::Codex(handle.clone()))
                        }
                        (Ok(Some(_)), SessionRuntime::Acp(handle)) => {
                            Some(KillableRuntime::Acp(handle.clone()))
                        }
                        (Ok(Some(_)), SessionRuntime::None)
                        | (Ok(None), _)
                        | (Err(_), _) => None,
                    };

                    if let Some(post_stop_record) = post_stop_record {
                        inner.sessions[index] = post_stop_record;
                        abandon_engram_pending_dispatch(
                            inner
                                .session_mut_by_index(index)
                                .expect("session index should remain valid"),
                            prepared_queued_turn
                                .as_ref()
                                .and_then(|prepared| prepared.pending_engram.clone()),
                        );
                    }

                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    record.orchestrator_auto_dispatch_blocked = true;
                    clear_active_turn_file_change_tracking(record);
                    Err((
                        ApiError::internal(format!("failed to persist session state: {err:#}")),
                        queued_runtime_to_shutdown,
                        stopped_mailbox_notification,
                    ))
                }
            }
        };
        let (
            queued_turn_result,
            pending_interaction_updates,
            created_messages,
            stopped_wait_refresh,
            stopped_mailbox_notification,
            revision,
        ) = match transition {
                Ok(transition) => transition,
                Err((error, queued_runtime_to_shutdown, stopped_mailbox_notification)) => {
                    if let Some(runtime) = queued_runtime_to_shutdown {
                        if let Err(cleanup_err) = shutdown_removed_runtime(
                            runtime,
                            &format!("uncommitted queued successor for session `{session_id}`"),
                        ) {
                            eprintln!(
                                "session cleanup warning> failed to tear down uncommitted queued successor for session `{session_id}`: {cleanup_err:#}"
                            );
                        }
                    }
                    if let Some(notification) = stopped_mailbox_notification.as_ref() {
                        if let Err(requeue_error) =
                            self.requeue_rejected_mailbox_notification(notification)
                        {
                            eprintln!(
                                "mailbox> failed restoring the stopped wake after persistence failure for `{}` / `{}`: {requeue_error:#}",
                                notification.session_id, notification.mailbox_id
                            );
                        }
                    }
                    return Err(error);
                }
        };
        self.publish_message_created_delta_parts(revision, created_messages);
        self.publish_message_updated_delta_parts(revision, pending_interaction_updates);
        self.publish_delegation_wait_consumed_deltas(
            revision,
            &stopped_wait_refresh.consumed_waits,
        );
        if let Some(notification) = stopped_mailbox_notification.as_ref() {
            if let Err(error) = self.requeue_rejected_mailbox_notification(notification) {
                eprintln!(
                    "mailbox> failed restoring the stopped wake for `{}` / `{}`: {error:#}",
                    notification.session_id, notification.mailbox_id
                );
            }
        }

        if let Some(orchestrator_instance_id) = orchestrator_stop_instance_id.as_deref() {
            self.note_stopped_orchestrator_session(orchestrator_instance_id, session_id);
        }

        if let Some(started) = queued_turn_result? {
            deliver_turn_dispatch(self, started.dispatch)?;
        }

        Ok(self.snapshot())
    }
}
