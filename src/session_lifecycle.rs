// Session lifecycle: the kill/stop/cancel-queued-prompt entry points on
// `AppState`. Three operations, three levels of destructiveness:
//
//   kill_session                > stop_session                > cancel_queued_prompt
//   (tears runtime + record)      (stops turn, keeps record)    (drops one queued prompt)
//
// `stop_session` has a `_with_options` variant because orchestrator cleanup
// paths need to suppress auto-dispatch of queued prompts (so the orchestrator
// transition can take priority) and tag the stop as part of an instance's
// cleanup wave (so transition scheduling skips the session while cleanup is in
// flight). Callers outside orchestrator cleanup use the default options and go
// through the plain `stop_session` wrapper. See
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
// the first is still in flight, the guard returns HTTP 409 Conflict (see
// `src/tests/session_stop_runtime.rs::stop_session_returns_conflict_when_already_stopping`).
//
// Remote proxying: if `session.remote_target` is set, each route short-circuits
// to the remote backend (`proxy_remote_kill_session`,
// `proxy_remote_cancel_queued_prompt`, `proxy_remote_stop_session` in
// `src/remote.rs`) and never touches a local runtime.

impl AppState {

    /// Destructively removes a session: tears down its runtime (kill
    /// child process for Claude/ACP, `turn/interrupt` + detach for shared
    /// Codex), removes the `SessionRecord` from `StateInner`, garbage-collects
    /// any orphan hidden Claude spares for the same workdir/project,
    /// suppresses rediscovery of detached Codex threads, and persists the new
    /// state. No undo. Triggered from the UI trash icon. Proxied to the
    /// remote backend when `session.remote_target` is set.
    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_kill_session(session_id);
        }
        let (
            runtime_to_kill,
            hidden_runtimes_to_kill,
            delegation_runtimes_to_kill,
            revision,
            delegation_lifecycle_deltas,
        ) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let workdir = inner.sessions[index].session.workdir.clone();
            let project_id = inner.sessions[index].session.project_id.clone();
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
            inner.remove_session_at(index);

            let mut hidden_runtimes = Vec::new();
            if agent == Agent::Claude {
                let visible_profiles = inner
                    .sessions
                    .iter()
                    .filter(|session_record| {
                        !session_record.hidden
                            && !session_record.is_remote_proxy()
                            && session_record.session.agent == Agent::Claude
                            && session_record.session.workdir == workdir
                            && session_record.session.project_id == project_id
                    })
                    .map(claude_spare_profile)
                    .collect::<Vec<_>>();
                inner.retain_sessions(|session_record| {
                    let should_consider = session_record.hidden
                        && !session_record.is_remote_proxy()
                        && session_record.session.agent == Agent::Claude
                        && session_record.session.workdir == workdir
                        && session_record.session.project_id == project_id;
                    if !should_consider {
                        return true;
                    }

                    let keep = visible_profiles
                        .iter()
                        .any(|profile| *profile == claude_spare_profile(session_record));
                    if !keep {
                        if let SessionRuntime::Claude(handle) = &session_record.runtime {
                            hidden_runtimes.push(KillableRuntime::Claude(handle.clone()));
                        }
                    }
                    keep
                });
            }

            if agent.supports_codex_prompt_settings() {
                inner.ignore_discovered_codex_thread(external_session_id.as_deref());
            }
            inner.normalize_orchestrator_instances();

            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            (
                runtime,
                hidden_runtimes,
                delegation_reconciliation.runtimes_to_kill,
                revision,
                delegation_reconciliation.lifecycle_deltas,
            )
        };

        if let Some(runtime) = runtime_to_kill {
            if let Err(err) =
                shutdown_removed_runtime(runtime, &format!("session `{session_id}`"))
            {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for runtime in hidden_runtimes_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, "a hidden Claude spare") {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for runtime in delegation_runtimes_to_kill {
            if let Err(err) =
                shutdown_removed_runtime(runtime, "a removed parent delegation child")
            {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for delta in delegation_lifecycle_deltas {
            self.publish_delegation_lifecycle_delta(revision, delta);
        }

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
    /// (auto-dispatch the next queued prompt on success, not part of an
    /// orchestrator cleanup wave).
    fn stop_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        self.stop_session_with_options(session_id, StopSessionOptions::default())
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
    /// - `dispatch_queued_prompts_on_success`: if `true` (the default),
    ///   the next queued prompt is auto-dispatched once the stop
    ///   completes; orchestrator cleanup paths set this to `false` so the
    ///   orchestrator's own transition can take priority.
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
        let (runtime_to_stop, stop_failure_is_best_effort) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

            if record.runtime_stop_in_progress {
                return Err(ApiError::conflict("session is already stopping"));
            }

            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
            }

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
                SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
                SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
                SessionRuntime::None => {
                    return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
                }
            };
            let stop_failure_is_best_effort = runtime.stop_failure_is_best_effort();

            // Preserve the public session status until the stop succeeds so borrowed state reads
            // never observe a contradictory transient Idle snapshot while shutdown is still pending.
            // `deferred_stop_callbacks` is guaranteed to be empty here because the guard above
            // already returned if `runtime_stop_in_progress` was true (and callbacks can only
            // defer when that flag is set).
            record.runtime_stop_in_progress = true;

            (runtime, stop_failure_is_best_effort)
        };

        let mut clear_external_session_id = false;
        if let Err(err) =
            shutdown_removed_runtime(runtime_to_stop, &format!("session `{session_id}`"))
        {
            if stop_failure_is_best_effort {
                eprintln!(
                    "session cleanup warning> failed to stop session `{session_id}` cleanly: {err:#}"
                );
                clear_external_session_id = true;
            } else {
                let (mut deferred_callbacks, token) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    let index = inner
                        .find_visible_session_index(session_id)
                        .ok_or_else(|| ApiError::not_found("session not found"))?;
                    let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                    record.runtime_stop_in_progress = false;
                    let deferred_callbacks = std::mem::take(&mut record.deferred_stop_callbacks);
                    let token = record.runtime.runtime_token();
                    (deferred_callbacks, token)
                };

                // Replay any terminal callbacks that arrived during the failed shutdown window.
                // The flag is now cleared so the callback methods will proceed normally.
                if let Some(token) = token {
                    deferred_callbacks.sort_by_key(|deferred| {
                        matches!(deferred, DeferredStopCallback::RuntimeExited(_))
                    });
                    for deferred in deferred_callbacks {
                        let replay_result = match deferred {
                            DeferredStopCallback::TurnFailed(msg) => {
                                self.fail_turn_if_runtime_matches(session_id, &token, &msg)
                            }
                            DeferredStopCallback::TurnError(msg) => {
                                self.mark_turn_error_if_runtime_matches(session_id, &token, &msg)
                            }
                            DeferredStopCallback::TurnCompleted => {
                                self.finish_turn_ok_if_runtime_matches(session_id, &token)
                            }
                            DeferredStopCallback::RuntimeExited(msg) => self
                                .handle_runtime_exit_if_matches(session_id, &token, msg.as_deref()),
                        };
                        if let Err(replay_err) = replay_result {
                            eprintln!(
                                "session cleanup warning> failed to replay deferred stop callback \
                                 for session `{session_id}`: {replay_err:#}"
                            );
                        }
                    }
                }

                return Err(ApiError::internal(format!(
                    "failed to stop session `{session_id}` cleanly: {err:#}"
                )));
            }
        }
        let orchestrator_stop_instance_id = options.orchestrator_stop_instance_id.clone();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let message_id = inner.next_message_id();
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let mut thread_id_to_suppress = None;
            {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                record.runtime = SessionRuntime::None;
                record.runtime_reset_required = false;
                record.runtime_stop_in_progress = false;
                record.deferred_stop_callbacks.clear();
                cancel_pending_interaction_messages(&mut record.session.messages);
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
                record.session.status = SessionStatus::Idle;
                record.session.preview = "Turn stopped by user.".to_owned();
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "Turn stopped by user.".to_owned(),
                    expanded_text: None,
                });
                if let Some(message_id) = file_change_message_id {
                    push_active_turn_file_changes_on_record(record, message_id);
                }
            }

            // Suppress rediscovery of the detached thread after the record
            // borrow is released. Without this, the still-running thread
            // would resurface as a new imported session on the next
            // import_discovered_codex_threads pass.
            if let Some(ref thread_id) = thread_id_to_suppress {
                inner.ignore_discovered_codex_thread(Some(thread_id));
            }

            finish_active_turn_file_change_tracking(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
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
            let has_queued_prompts = options.dispatch_queued_prompts_on_success
                && !inner.sessions[index].queued_prompts.is_empty();
            if let Err(err) = self.commit_locked(&mut inner) {
                if added_stopped_session_id {
                    if let Some(instance_index) = stopped_orchestrator_instance_index {
                        inner.orchestrator_instances[instance_index]
                            .stopped_session_ids_during_stop
                            .retain(|candidate| candidate != session_id);
                    }
                }
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .orchestrator_auto_dispatch_blocked = true;
                return Err(ApiError::internal(format!(
                    "failed to persist session state: {err:#}"
                )));
            }
            has_queued_prompts
        };

        if let Some(orchestrator_instance_id) = orchestrator_stop_instance_id.as_deref() {
            self.note_stopped_orchestrator_session(orchestrator_instance_id, session_id);
        }

        if should_dispatch_next {
            if let Some(dispatch) =
                self.dispatch_next_queued_turn(session_id, false)
                    .map_err(|err| {
                        ApiError::internal(format!("failed to dispatch queued prompt: {err:#}"))
                    })?
            {
                deliver_turn_dispatch(self, dispatch)?;
            }
        }

        Ok(self.snapshot())
    }
}
