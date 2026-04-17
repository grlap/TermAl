// Session-level identity plumbing: message IDs, agent-side "external"
// session IDs (Claude's `session_id`, Codex's `threadId`, ACP's
// conversation id), and the Codex-specific thread state that sits
// alongside them. Every method here that mutates a session is either
// a one-shot setter or the runtime-token-guarded `_if_runtime_matches`
// variant (see `turn_lifecycle.rs` for the RuntimeToken staleness
// pattern — a stray event from a torn-down runtime must not clobber
// state owned by the new runtime).
//
// Why "external" vs "TermAl" id. Every session has two identities:
// the stable TermAl-side `session.id` (generated on create, persisted
// in `~/.termal/*.json`, used by all SSE traffic) and an agent-
// assigned `external_session_id` that the agent reuses across
// `thread/resume` + `ClaudeSessionHandle::resume` to stay on the same
// conversation thread across subprocess restarts. The agent never
// sees TermAl ids; TermAl never shows external ids to users. This
// file is where the two are bound/unbound as threads start, resume,
// and finish.
//
// Codex thread state. When the shared Codex runtime (see
// `shared_codex_mgr.rs`) pushes a `thread/started` or `thread/resumed`
// notification, `set_codex_thread_state_if_runtime_matches` stamps
// the per-session `codex_thread_state` so the UI's thread indicator
// updates in sync with the external id.

impl AppState {
    /// Generates the next sequential message id for this `AppState`.
    /// Message ids are monotonic within the app (not per-session) so
    /// deltas that reference a message id are unambiguous across all
    /// SSE streams.
    fn allocate_message_id(&self) -> String {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.next_message_id()
    }

    /// Binds the agent-assigned external session id to a TermAl
    /// session record unconditionally. Mostly used by startup
    /// discovery paths where no runtime is active yet — live agent
    /// callbacks should use
    /// [`Self::set_external_session_id_if_runtime_matches`] instead.
    fn set_external_session_id(&self, session_id: &str, external_session_id: String) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        set_record_external_session_id(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), Some(external_session_id));
        if inner.sessions[index]
            .session
            .agent
            .supports_codex_prompt_settings()
        {
            let external_session_id = inner.sessions[index].external_session_id.clone();
            inner.allow_discovered_codex_thread(external_session_id.as_deref());
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Sets external session ID only if the session still belongs to the expected runtime.
    ///
    /// Returns the applied/skipped outcome, or `Err` when commit/persistence
    /// fails after the in-memory mutation has begun. This avoids the TOCTOU gap of a separate
    /// `session_matches_runtime_token` check followed by `set_external_session_id`.
    fn set_external_session_id_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        external_session_id: String,
    ) -> Result<RuntimeMatchOutcome> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(RuntimeMatchOutcome::SessionMissing);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(RuntimeMatchOutcome::RuntimeMismatch);
        }
        set_record_external_session_id(record, Some(external_session_id));
        if inner.sessions[index]
            .session
            .agent
            .supports_codex_prompt_settings()
        {
            let external_session_id = inner.sessions[index].external_session_id.clone();
            inner.allow_discovered_codex_thread(external_session_id.as_deref());
        }
        self.commit_locked(&mut inner)?;
        Ok(RuntimeMatchOutcome::Applied)
    }

    /// Clears external session ID when the expected runtime still owns the session.
    ///
    /// When `suppress_rediscovery` is `true` and the session agent supports Codex
    /// prompt settings, the cleared thread ID is added to the ignored-discovery
    /// set so it does not resurface as a new imported session. Pass `true` for
    /// newly created threads (`thread/start`) that would otherwise be orphaned,
    /// and `false` for resumed pre-existing threads (`thread/resume`) whose
    /// discovery state should be preserved.
    fn clear_external_session_id_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        external_session_id: &str,
        suppress_rediscovery: bool,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(());
        }
        if record.external_session_id.as_deref() != Some(external_session_id) {
            return Ok(());
        }

        let should_ignore_thread =
            suppress_rediscovery && record.session.agent.supports_codex_prompt_settings();
        set_record_external_session_id(record, None);
        if should_ignore_thread {
            inner.ignore_discovered_codex_thread(Some(external_session_id));
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Stamps the per-session Codex thread state when the RuntimeToken
    /// still matches the current Codex runtime.
    ///
    /// Invoked from the shared Codex runtime's event dispatcher on
    /// `thread/started`, `thread/resumed`, and similar lifecycle
    /// notifications. The `_if_runtime_matches` guard ensures a
    /// stale notification from a prior app-server cannot overwrite
    /// the thread state owned by the current runtime (see
    /// `shared_codex_mgr.rs` for why the shared runtime can be
    /// replaced mid-flight).
    fn set_codex_thread_state_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        thread_state: CodexThreadState,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(());
        }
        if record.runtime_stop_in_progress {
            return Ok(());
        }

        let next_state = normalized_codex_thread_state(
            record.session.agent,
            record.external_session_id.as_deref(),
            Some(thread_state),
        );
        if record.session.codex_thread_state == next_state {
            return Ok(());
        }

        record.session.codex_thread_state = next_state;
        self.commit_locked(&mut inner)?;
        Ok(())
    }
}
