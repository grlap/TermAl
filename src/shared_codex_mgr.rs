// Shared Codex app-server runtime management for `AppState`.
//
// Codex is the odd one out among the agent backends. Where Claude
// (`src/claude.rs`) and ACP (`src/acp.rs`) each spawn a dedicated
// per-session subprocess, Codex runs as ONE long-lived
// `codex app-server` process that hosts every Codex-backed session in
// this `AppState`. Threads are Codex's native unit of conversation,
// and a single app-server can serve many threads concurrently, so a
// shared process is both correct and much cheaper than N per-session
// children.
//
// Lifecycle. The runtime is lazy: nothing starts until the first
// Codex session asks for it (`shared_codex_runtime`), at which point
// `spawn_shared_codex_runtime` (in `src/codex.rs`) forks the process
// and performs the JSON-RPC `initialize` / `initialized` handshake.
// Every later Codex session that spawns piggybacks on the same
// runtime. Because every session shares one child, if that child
// exits — crash, stdout EOF, watchdog-detected stdin timeout, or
// clean shutdown — ALL Codex sessions go down together;
// `handle_shared_codex_runtime_exit` fans the failure out and marks
// each of them via `handle_runtime_exit_if_matches` (see
// `src/turn_lifecycle.rs`).
//
// Mutex pattern. `shared_codex_runtime` lives in `Arc<Mutex<Option<_>>>`
// on `AppState`, intentionally separate from `AppState.inner`. The
// first-time handshake blocks for up to a few minutes, so holding the
// main state mutex during spawn would freeze every other session. The
// runtime mutex is only held long enough to clone-or-spawn the handle.
//
// Staleness guard (`_if_matches`). A fresh runtime can spin up before
// an exit handler for a prior runtime finishes racing. Every mutation
// here is gated on a `runtime_id` match so a late exit callback for an
// already-replaced runtime cannot clobber a newer one. Same pattern
// as the `RuntimeToken` guards in `src/turn_lifecycle.rs`.
//
// Wire protocol. JSON-RPC 2.0 over stdio: TermAl sends requests with
// string ids, Codex replies with results keyed by the same id, and
// Codex also pushes notifications (no id) for streaming turn events.
// Transport + dispatch details live in `src/codex_rpc.rs`
// (`send_codex_json_rpc_request`, `wait_for_codex_json_rpc_response`)
// and `src/codex_events.rs` (inbound message routing). The thread
// action helper here (`resolve_codex_thread_action_context`) is the
// shared prologue for the operations in `src/codex_thread_actions.rs`.

impl AppState {
    /// Returns the shared Codex app-server runtime, spawning it on first
    /// demand. Subsequent callers get a clone of the same handle so every
    /// Codex-backed session in this `AppState` funnels through one child
    /// process and one JSON-RPC wire.
    ///
    /// The runtime mutex is held only long enough to clone-or-spawn the
    /// handle, not across `AppState.inner`, so a slow `initialize`
    /// handshake cannot freeze other sessions. When the backing child
    /// exits, [`Self::clear_shared_codex_runtime_if_matches`] zeros this
    /// slot so the next caller triggers a fresh spawn.
    fn shared_codex_runtime(&self) -> Result<SharedCodexRuntime> {
        let mut shared_runtime = self
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        if let Some(runtime) = shared_runtime.clone() {
            return Ok(runtime);
        }

        let runtime = spawn_shared_codex_runtime(self.clone())?;
        *shared_runtime = Some(runtime.clone());
        Ok(runtime)
    }

    /// Sends a JSON-RPC request to the shared Codex runtime and blocks on
    /// the response.
    ///
    /// Queues a `CodexRuntimeCommand::JsonRpcRequest` onto the runtime's
    /// writer channel so the request is serialized with all other traffic,
    /// then waits on a one-shot reply channel with a one-second grace
    /// window past `timeout` (the runtime itself enforces `timeout` for
    /// the remote call). The three error paths map to `ApiError`: a
    /// runtime-level transport or queueing failure and a missing result
    /// both surface as `internal`; a Codex-side JSON-RPC error or
    /// explicit timeout comes back as `bad_request`. Used by the Codex
    /// thread actions in `src/codex_thread_actions.rs` and the
    /// model-list pagination path in `src/codex_rpc.rs`.
    fn perform_codex_json_rpc_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let runtime = self.shared_codex_runtime().map_err(|err| {
            ApiError::internal(format!("failed to start shared Codex runtime: {err:#}"))
        })?;
        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<Value, String>>();
        runtime
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcRequest {
                method: method.to_owned(),
                params,
                timeout,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!("failed to queue Codex request `{method}`: {err}"))
            })?;

        match response_rx.recv_timeout(timeout + Duration::from_secs(1)) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(detail)) => Err(ApiError::bad_request(format!(
                "Codex request `{method}` failed: {detail}"
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out waiting for Codex request `{method}`"
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "Codex request `{method}` did not return a result"
            ))),
        }
    }

    /// Collects the thread id plus the record-level state that every
    /// Codex thread action (fork, archive, unarchive, compact, rollback)
    /// needs for dispatch.
    ///
    /// Pulled out as a helper because every action in
    /// `src/codex_thread_actions.rs` needs the same pre-flight bundle
    /// and the lookup is non-trivial: it walks visible sessions to find
    /// the record, rejects the call if the session isn't Codex, enforces
    /// the live-idle-thread guard (no `Active` / `Approval` status, no
    /// queued prompts), requires an `external_session_id` (Codex's
    /// `threadId`), and normalizes the thread state / approval policy /
    /// sandbox mode / reasoning effort back to concrete values for the
    /// subsequent `thread/*` JSON-RPC call.
    fn resolve_codex_thread_action_context(
        &self,
        session_id: &str,
    ) -> Result<CodexThreadActionContext, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];

        if record.session.agent != Agent::Codex {
            return Err(ApiError::bad_request(
                "Codex thread actions are only available for Codex sessions",
            ));
        }
        if matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) {
            return Err(ApiError::conflict(
                "wait for the current Codex turn to finish before using thread actions",
            ));
        }
        if !record.queued_prompts.is_empty() {
            return Err(ApiError::conflict(
                "wait for queued Codex prompts to finish before using thread actions",
            ));
        }

        let thread_id = record.external_session_id.clone().ok_or_else(|| {
            ApiError::bad_request(
                "Codex thread actions are only available after the session has started a thread",
            )
        })?;

        Ok(CodexThreadActionContext {
            approval_policy: record
                .session
                .approval_policy
                .unwrap_or(record.codex_approval_policy),
            model: record.session.model.clone(),
            model_options: record.session.model_options.clone(),
            name: record.session.name.clone(),
            project_id: record.session.project_id.clone(),
            reasoning_effort: record
                .session
                .reasoning_effort
                .unwrap_or(record.codex_reasoning_effort),
            sandbox_mode: record
                .session
                .sandbox_mode
                .unwrap_or(record.codex_sandbox_mode),
            thread_id,
            thread_state: normalized_codex_thread_state(
                record.session.agent,
                record.external_session_id.as_deref(),
                record.session.codex_thread_state,
            ),
            workdir: record.session.workdir.clone(),
        })
    }

    /// Zeros the shared runtime slot if and only if it still holds the
    /// runtime with `runtime_id`, then kills that child.
    ///
    /// The staleness guard is load-bearing: when a runtime exits, several
    /// threads (the writer, the stdout reader, the `wait()` thread, a
    /// stdin watchdog) can race to report the failure, and in the
    /// meantime another session may have already triggered
    /// [`Self::shared_codex_runtime`] to spawn a replacement. Clearing
    /// the slot unconditionally would clobber the fresh runtime and
    /// leave its clients orphaned. Only the handler whose runtime still
    /// matches may take the slot and terminate the child.
    fn clear_shared_codex_runtime_if_matches(&self, runtime_id: &str) -> Result<()> {
        let removed_runtime = {
            let mut shared_runtime = self
                .shared_codex_runtime
                .lock()
                .expect("shared Codex runtime mutex poisoned");
            if shared_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.runtime_id == runtime_id)
            {
                shared_runtime.take()
            } else {
                None
            }
        };

        if let Some(runtime) = removed_runtime {
            runtime.kill().with_context(|| {
                format!("failed to terminate shared Codex runtime `{runtime_id}`")
            })?;
        }

        Ok(())
    }

    /// Cascades a shared-runtime exit onto every Codex-backed session
    /// that was using it, then drops the runtime from `AppState`.
    ///
    /// Because one Codex child hosts all Codex sessions at once, its
    /// death invalidates every session running on it. This scans
    /// `AppState.inner` for records whose `SessionRuntime::Codex(handle)`
    /// carries the matching `runtime_id`, then calls
    /// `handle_runtime_exit_if_matches` (see `src/turn_lifecycle.rs`)
    /// for each — that helper is itself runtime-token guarded, so a
    /// session that has already been stopped or rebound silently no-ops.
    /// `error_message` (e.g. a watchdog timeout detail or a non-zero
    /// process exit status) is propagated to the per-session failure
    /// so the UI shows a concrete reason. Finally
    /// [`Self::clear_shared_codex_runtime_if_matches`] releases the slot
    /// so the next Codex session triggers a fresh spawn.
    fn handle_shared_codex_runtime_exit(
        &self,
        runtime_id: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| match &record.runtime {
                    SessionRuntime::Codex(handle) if handle.runtime_id == runtime_id => {
                        Some(record.session.id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let token = RuntimeToken::Codex(runtime_id.to_owned());
        for session_id in session_ids {
            self.handle_runtime_exit_if_matches(&session_id, &token, error_message)?;
        }
        self.clear_shared_codex_runtime_if_matches(runtime_id)?;
        Ok(())
    }
}
