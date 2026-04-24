// Read-side of `AppState`: snapshot builders, the agent-readiness
// cache, remote SSE-fallback dedup, the workspace file watcher spawn,
// and a handful of session-state readers (`claude_approval_mode`,
// `cursor_mode`, `session_matches_runtime_token`, `clear_runtime`).
//
// Everything here either returns a value to a caller without
// mutating state, or performs targeted session-record cleanup
// (`clear_runtime`). Mutation-heavy routes live in their dedicated
// files — session CRUD in `session_crud.rs`, turn dispatch in
// `turn_dispatch.rs`, settings sync in `session_sync.rs`, and the
// commit/broadcast pipeline in `sse_broadcast.rs`.
//
// Snapshot semantics. `snapshot()` and `snapshot_from_inner()` are
// two different entry points with different freshness guarantees:
// `snapshot()` refreshes the agent-readiness cache via filesystem
// I/O *before* locking `inner`, then reads the freshly-populated
// cache under the lock, so the returned `StateResponse` reflects
// current CLI availability. `snapshot_from_inner()` is the hot-path
// builder used inside `commit_locked` / `publish_state_locked` where
// the lock is already held and filesystem I/O is not safe — it
// reuses whatever value `cached_agent_readiness()` happens to have.
// This is the cache-staleness tradeoff documented in
// `sse_broadcast.rs`.
//
// Remote SSE fallback dedup. When a remote-proxy SSE stream drops,
// this host falls back to polling the remote's `/state` endpoint.
// The three `_remote_sse_fallback_*` methods track which remotes
// already received a resync so the fallback loop doesn't re-push
// identical snapshots back to clients.

impl AppState {
    fn wire_session_from_record(record: &SessionRecord) -> Session {
        let mut session = record.session.clone();
        session.messages_loaded = true;
        session.message_count = session_message_count(record);
        session.session_mutation_stamp = Some(record.mutation_stamp);
        session
    }

    fn wire_session_summary_from_record(record: &SessionRecord) -> Session {
        let mut session = Self::wire_session_from_record(record);
        session.messages.clear();
        session.messages_loaded = false;
        session
    }

    /// Builds a metadata-first state snapshot with guaranteed-fresh agent readiness.
    ///
    /// The cache is refreshed (filesystem I/O) *before* locking `inner`, then
    /// the snapshot reads `cached_agent_readiness()` *under* the `inner` lock —
    /// the same path used by `commit_locked` / `publish_state_locked`.  This
    /// ensures that a `snapshot()` call at revision N uses the same cached
    /// readiness value that was published in the SSE event for revision N.
    #[cfg(not(test))]
    fn snapshot(&self) -> StateResponse {
        self.summary_snapshot()
    }

    /// Test-only full snapshot inspection helper.
    ///
    /// Production `/api/state` and SSE state events are metadata-first. The
    /// Rust unit suite historically uses `state.snapshot()` to assert internal
    /// transcript mutations directly, so test builds keep that inspection shape
    /// while route/SSE tests call `summary_snapshot()` through the real handlers.
    #[cfg(test)]
    fn snapshot(&self) -> StateResponse {
        self.full_snapshot()
    }

    fn summary_snapshot(&self) -> StateResponse {
        let _ = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner(&inner)
    }

    fn summary_snapshot_with_full_session(&self, session_id: &str) -> StateResponse {
        let agent_readiness = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner_with_full_session(&inner, agent_readiness, session_id)
    }

    #[cfg(test)]
    fn full_snapshot(&self) -> StateResponse {
        let _ = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.full_snapshot_from_inner(&inner)
    }

    fn agent_readiness_snapshot(&self) -> Vec<AgentReadiness> {
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let snapshot = collect_agent_readiness(&self.default_workdir);
        let mut cache = self
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned");
        *cache = AgentReadinessCache::fresh(snapshot);
        cache.snapshot.clone()
    }

    fn cached_agent_readiness_if_fresh(&self) -> Option<Vec<AgentReadiness>> {
        let cache = self
            .agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned");
        let now = std::time::Instant::now();
        (!cache.needs_refresh(now)).then(|| cache.snapshot.clone())
    }

    fn cached_agent_readiness(&self) -> Vec<AgentReadiness> {
        self.agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned")
            .snapshot
            .clone()
    }

    /// Returns one visible session with its state revision.
    fn get_session(&self, session_id: &str) -> Result<SessionResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        Ok(SessionResponse {
            revision: inner.revision,
            session: Self::wire_session_from_record(&inner.sessions[index]),
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn invalidate_agent_readiness_cache(&self) {
        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        self.agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned")
            .invalidated = true;
    }

    /// Returns whether a remote fallback-driven /api/state resync can be
    /// skipped because the same or a newer fallback revision was already
    /// recovered for that remote within the current event-stream lifetime.
    fn should_skip_remote_sse_fallback_resync(
        &self,
        remote_id: &str,
        fallback_revision: u64,
    ) -> bool {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .get(remote_id)
            .is_some_and(|last_revision| *last_revision >= fallback_revision)
    }

    /// Records that a remote fallback-driven /api/state resync recovered the
    /// given fallback revision.
    fn note_remote_sse_fallback_resync(&self, remote_id: &str, fallback_revision: u64) {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .entry(remote_id.to_owned())
            .and_modify(|last_revision| {
                *last_revision = (*last_revision).max(fallback_revision);
            })
            .or_insert(fallback_revision);
    }

    /// Clears the latest applied remote revision when event-stream continuity
    /// is lost, such as after a disconnect or restart.
    fn clear_remote_applied_revision(&self, remote_id: &str) {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .remove(remote_id);
    }

    /// Clears remote fallback resync tracking when event-stream continuity is
    /// lost, such as after a disconnect or restart.
    fn clear_remote_sse_fallback_resync(&self, remote_id: &str) {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .remove(remote_id);
    }




    #[cfg(not(test))]
    fn spawn_workspace_file_watcher(&self) {
        let state = self.clone();
        std::thread::Builder::new()
            .name("termal-file-watch".to_owned())
            .spawn(move || run_workspace_file_watcher(state))
            .expect("failed to spawn file watcher thread");
    }


    /// Builds a snapshot using the latest cached agent readiness **without refreshing**.
    ///
    /// This is the hot-path builder used inside `commit_locked` / `publish_state_locked`
    /// where the `inner` mutex is held and filesystem I/O is not safe.  Callers that
    /// need guaranteed-fresh readiness (e.g. after an explicit cache invalidation) should
    /// drop the `inner` lock and use [`snapshot()`](Self::snapshot) instead.
    ///
    /// **Design tradeoff:** after the cache TTL expires, mutation paths through
    /// `commit_locked` will publish SSE events with stale readiness until a
    /// [`snapshot()`](Self::snapshot) call (e.g. `GET /api/state`, SSE reconnect)
    /// refreshes the cache.  This staleness can span multiple revisions — it is
    /// not bounded to a single mutation cycle.  This is acceptable because agent
    /// readiness changes only when CLI tools are installed or removed (extremely
    /// rare during an active session), and any `snapshot()` call refreshes the
    /// cache as a side effect even when the frontend drops the response, so the
    /// following mutation carries the fresh value.  Paths where freshness matters
    /// (`create_session`, `update_app_settings`) pre-refresh the cache before
    /// entering the critical section.
    fn snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        self.snapshot_from_inner_with_agent_readiness(inner, self.cached_agent_readiness())
    }

    fn snapshot_from_inner_with_agent_readiness(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(Self::wire_session_summary_from_record)
                .collect(),
        }
    }

    fn snapshot_from_inner_with_full_session(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
        full_session_id: &str,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(|record| {
                    if record.session.id == full_session_id {
                        Self::wire_session_from_record(record)
                    } else {
                        Self::wire_session_summary_from_record(record)
                    }
                })
                .collect(),
        }
    }

    #[cfg(test)]
    fn full_snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        self.full_snapshot_from_inner_with_agent_readiness(inner, self.cached_agent_readiness())
    }

    #[cfg(test)]
    fn full_snapshot_from_inner_with_agent_readiness(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            server_instance_id: self.server_instance_id.clone(),
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(Self::wire_session_from_record)
                .collect(),
        }
    }








    /// Returns the effective Claude approval mode for a session
    /// (falling back to the app default when the session hasn't
    /// overridden it). Used by the runtime spawn helpers and the
    /// "approve all" UI toggle.
    fn claude_approval_mode(&self, session_id: &str) -> Result<ClaudeApprovalMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode))
    }

    /// Returns the effective Cursor agent mode for a session
    /// (`Agent` / `Composer` / etc., falling back to the app
    /// default). Used by the Cursor spawn helpers.
    fn cursor_mode(&self, session_id: &str) -> Result<CursorMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .cursor_mode
            .unwrap_or_else(default_cursor_mode))
    }

    /// Compares the session's current `SessionRuntime` handle against
    /// an expected `RuntimeToken`. Used by the `_if_runtime_matches`
    /// guard wrappers in `turn_lifecycle.rs` to drop stray events
    /// from torn-down runtimes (see `session_runtime.rs` for the
    /// token lifecycle).
    fn session_matches_runtime_token(&self, session_id: &str, token: &RuntimeToken) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| record.runtime.matches_runtime_token(token))
    }

    /// Zeros out the session's runtime state — drops the
    /// `SessionRuntime` handle, clears pending approvals / user
    /// inputs / file-change tracking / deferred stop callbacks —
    /// leaving the session at a clean `SessionStatus::Idle`. Invoked
    /// when a runtime exit has been fully processed and nothing
    /// should remain bound to the dead process.
    fn clear_runtime(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let had_changes = !matches!(record.runtime, SessionRuntime::None)
            || record.runtime_reset_required
            || record.runtime_stop_in_progress
            || has_pending_requests(record);
        if !had_changes {
            return Ok(());
        }

        record.runtime = SessionRuntime::None;
        record.runtime_reset_required = false;
        record.orchestrator_auto_dispatch_blocked = false;
        record.runtime_stop_in_progress = false;
        record.deferred_stop_callbacks.clear();
        clear_active_turn_file_change_tracking(record);
        clear_all_pending_requests(record);
        self.commit_locked(&mut inner)?;
        Ok(())
    }



}
