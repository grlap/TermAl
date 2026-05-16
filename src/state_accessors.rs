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
// Snapshot semantics. `snapshot()` and `snapshot_from_inner()` both
// build metadata-first production-shaped state snapshots. `snapshot()`
// refreshes the agent-readiness cache via filesystem I/O *before*
// locking `inner`, then reads the freshly-populated cache under the
// lock, so the returned `StateResponse` reflects current CLI
// availability. `snapshot_from_inner()` is the hot-path builder used
// inside `commit_locked` / `publish_state_locked` where the lock is
// already held and filesystem I/O is not safe — it reuses whatever
// value `cached_agent_readiness()` happens to have. This is the
// cache-staleness tradeoff documented in `sse_broadcast.rs`.
//
// Tests that need to inspect internal transcript state must call the
// explicit `full_snapshot()` helper instead of relying on a cfg(test)
// behavior split in `snapshot()`.
//
// Remote SSE fallback dedup. When a remote-proxy SSE stream drops,
// this host falls back to polling the remote's `/state` endpoint.
// The three `_remote_sse_fallback_*` methods track which remotes
// already received a resync so the fallback loop doesn't re-push
// identical snapshots back to clients.

#[cfg(test)]
const REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_TAIL_HYDRATION_MAX_MESSAGES: usize = 500;

/// Returns true for locally-typed remote hydration misses that can fall back to
/// metadata without hiding local lookup or protocol errors.
///
/// Visible-pane hydration uses this to return a cached summary. Delta replay
/// (`hydrate_remote_session_via_delta_replay` in `remote_routes.rs`) uses the
/// same set to mean "targeted transcript repair is unavailable; try the narrow
/// delta apply instead." The typed kind is the recovery contract; status
/// controls the wire response but does not change local recoverability.
fn is_recoverable_remote_hydration_miss(err: &ApiError) -> bool {
    matches!(
        err.kind,
        Some(
            ApiErrorKind::RemoteConnectionUnavailable
                | ApiErrorKind::RemoteSessionHydrationFreshnessRace
                | ApiErrorKind::RemoteSessionMissingFullTranscript
        )
    )
}

fn select_visible_session_hydration_fallback_error(
    original: ApiError,
    fallback: ApiError,
) -> ApiError {
    // A typed local miss proves the cached summary no longer exists, so it is
    // more actionable for the visible-pane caller than the recoverable remote
    // hydration miss that triggered fallback. Other fallback failures preserve
    // the original remote error because the local path only failed while trying
    // to produce a degraded response.
    // `LocalSessionMissing` is produced by local session lookup/fallback paths;
    // new producers should review this selection policy before reusing it.
    if fallback.kind == Some(ApiErrorKind::LocalSessionMissing) {
        fallback
    } else {
        original
    }
}

#[cfg(test)]
mod visible_session_hydration_error_tests {
    use super::*;

    struct TempStateDir {
        path: PathBuf,
    }

    impl TempStateDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("test root should be created");
            Self { path }
        }

        fn path(&self) -> &FsPath {
            &self.path
        }
    }

    impl Drop for TempStateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn recoverable_remote_hydration_misses_are_typed() {
        for kind in [
            ApiErrorKind::RemoteConnectionUnavailable,
            ApiErrorKind::RemoteSessionHydrationFreshnessRace,
            ApiErrorKind::RemoteSessionMissingFullTranscript,
        ] {
            for status in [StatusCode::BAD_GATEWAY, StatusCode::SERVICE_UNAVAILABLE] {
                let err = ApiError::from_status(status, "copy can change").with_kind(kind);
                assert!(is_recoverable_remote_hydration_miss(&err));
            }
        }
    }

    #[test]
    fn remote_hydration_recovery_does_not_parse_copy() {
        let legacy_connection_copy = ApiError::bad_gateway(
            "Could not connect to remote \"SSH Lab\" over SSH. Check the host, network, and SSH settings, then try again.",
        );
        let legacy_freshness_copy = ApiError::bad_gateway(
            "remote session response revision 5 cannot be safely applied; latest synchronized remote state revision is 4 and the transcript may have changed",
        );

        assert!(!is_recoverable_remote_hydration_miss(
            &legacy_connection_copy
        ));
        assert!(!is_recoverable_remote_hydration_miss(
            &legacy_freshness_copy
        ));
    }

    #[test]
    fn local_not_found_fallback_error_wins_over_recoverable_remote_error() {
        let original = ApiError::bad_gateway("remote unavailable")
            .with_kind(ApiErrorKind::RemoteConnectionUnavailable);
        let fallback =
            ApiError::local_session_missing();

        let selected = select_visible_session_hydration_fallback_error(original, fallback);

        assert_eq!(selected.status, StatusCode::NOT_FOUND);
        assert_eq!(selected.message, "session not found");
    }

    #[test]
    fn untyped_not_found_fallback_error_preserves_recoverable_remote_error() {
        let original = ApiError::bad_gateway("remote unavailable")
            .with_kind(ApiErrorKind::RemoteConnectionUnavailable);
        let fallback = ApiError::not_found("generic not found");

        let selected = select_visible_session_hydration_fallback_error(original, fallback);

        assert_eq!(selected.status, StatusCode::BAD_GATEWAY);
        assert_eq!(selected.message, "remote unavailable");
        assert_eq!(
            selected.kind,
            Some(ApiErrorKind::RemoteConnectionUnavailable)
        );
    }

    #[test]
    fn transient_fallback_error_preserves_recoverable_remote_error() {
        let original = ApiError::bad_gateway("remote unavailable")
            .with_kind(ApiErrorKind::RemoteConnectionUnavailable);
        let fallback = ApiError::internal("fallback failed");

        let selected = select_visible_session_hydration_fallback_error(original, fallback);

        assert_eq!(selected.status, StatusCode::BAD_GATEWAY);
        assert_eq!(selected.message, "remote unavailable");
        assert_eq!(
            selected.kind,
            Some(ApiErrorKind::RemoteConnectionUnavailable)
        );
    }

    #[test]
    fn hydration_fallback_response_tags_missing_local_session() {
        let root = TempStateDir::new("termal-visible-session-fallback");
        let state_path = root.path().join("state.json");
        let templates_path = root.path().join("orchestrators.json");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            state_path,
            templates_path,
        )
        .expect("test state should initialize");

        let err = match state.get_session_hydration_fallback_response("missing-session") {
            Ok(_) => panic!("missing local session should return a typed error"),
            Err(err) => err,
        };

        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.kind, Some(ApiErrorKind::LocalSessionMissing));
    }

    #[test]
    fn wire_sessions_expose_remote_owner_metadata() {
        let root = TempStateDir::new("termal-remote-owner-wire");
        let state_path = root.path().join("state.json");
        let templates_path = root.path().join("orchestrators.json");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            state_path,
            templates_path,
        )
        .expect("test state should initialize");

        let local_session_id = state
            .create_session(CreateSessionRequest {
                name: Some("Local Session".to_owned()),
                agent: Some(Agent::Codex),
                workdir: Some(root.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            })
            .expect("local session should be created")
            .session_id;
        let remote_session = Session {
            id: "remote-session-1".to_owned(),
            name: "Remote Proxy".to_owned(),
            emoji: Agent::Codex.avatar().to_owned(),
            agent: Agent::Codex,
            workdir: root.path().to_string_lossy().into_owned(),
            project_id: None,
            remote_id: Some("untrusted-upstream-remote".to_owned()),
            model: Agent::Codex.default_model().to_owned(),
            model_options: Vec::new(),
            approval_policy: Some(default_codex_approval_policy()),
            reasoning_effort: Some(default_codex_reasoning_effort()),
            sandbox_mode: Some(default_codex_sandbox_mode()),
            cursor_mode: None,
            claude_effort: None,
            claude_approval_mode: None,
            gemini_approval_mode: None,
            external_session_id: None,
            agent_commands_revision: 0,
            codex_thread_state: None,
            status: SessionStatus::Idle,
            preview: "Remote session ready.".to_owned(),
            messages: Vec::new(),
            messages_loaded: true,
            message_count: 0,
            markers: Vec::new(),
            pending_prompts: Vec::new(),
            session_mutation_stamp: Some(7),
            parent_delegation_id: None,
        };
        state
            .apply_remote_delta_event(
                "ssh-lab",
                DeltaEvent::SessionCreated {
                    revision: 1,
                    session_id: remote_session.id.clone(),
                    session: remote_session,
                },
            )
            .expect("remote session-created delta should localize");
        let remote_session_id = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_remote_session_index("ssh-lab", "remote-session-1")
                .expect("localized remote session should exist");
            inner.sessions[index].session.id.clone()
        };

        let summary = state.summary_snapshot();
        let remote_summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == remote_session_id)
            .expect("remote summary session should exist");
        assert_eq!(remote_summary_session.remote_id.as_deref(), Some("ssh-lab"));
        let local_summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == local_session_id)
            .expect("local summary session should exist");
        assert!(local_summary_session.remote_id.is_none());

        let remote_full = state
            .get_session(&remote_session_id)
            .expect("remote full session should be available");
        assert_eq!(remote_full.session.remote_id.as_deref(), Some("ssh-lab"));
        let local_full = state
            .get_session(&local_session_id)
            .expect("local full session should be available");
        assert!(local_full.session.remote_id.is_none());
    }

    #[test]
    fn summary_snapshot_omits_pending_prompt_content() {
        let root = TempStateDir::new("termal-summary-pending-prompts");
        let state_path = root.path().join("state.json");
        let templates_path = root.path().join("orchestrators.json");
        let state = AppState::new_with_paths(
            root.path().to_string_lossy().into_owned(),
            state_path,
            templates_path,
        )
        .expect("test state should initialize");
        let session_id = state
            .create_session(CreateSessionRequest {
                name: Some("Queued Session".to_owned()),
                agent: Some(Agent::Codex),
                workdir: Some(root.path().to_string_lossy().into_owned()),
                project_id: None,
                model: None,
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: None,
                claude_approval_mode: None,
                claude_effort: None,
                gemini_approval_mode: None,
            })
            .expect("session should be created")
            .session_id;
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("session should exist");
            inner.sessions[index].session.pending_prompts.push(PendingPrompt {
                attachments: Vec::new(),
                id: "pending-1".to_owned(),
                timestamp: "10:00".to_owned(),
                text: "Sensitive queued prompt".to_owned(),
                expanded_text: Some("Expanded sensitive queued prompt".to_owned()),
            });
        }

        let summary = state.summary_snapshot();
        let summary_session = summary
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("summary session should be present");
        assert!(summary_session.pending_prompts.is_empty());

        let targeted = state.summary_snapshot_with_full_session(&session_id);
        let targeted_session = targeted
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .expect("targeted session should be present");
        assert_eq!(targeted_session.pending_prompts.len(), 1);
        assert_eq!(
            targeted_session.pending_prompts[0].text,
            "Sensitive queued prompt"
        );
        assert_eq!(
            targeted_session.pending_prompts[0].expanded_text.as_deref(),
            Some("Expanded sensitive queued prompt")
        );
    }
}

impl AppState {
    fn wire_session_from_record(record: &SessionRecord) -> Session {
        let mut session = record.session.clone();
        // The record owns remote-proxy identity; the wire field is a derived
        // UI/API projection and embedded session snapshots are not authoritative.
        session.remote_id = record.remote_id.clone();
        session.messages_loaded = record.session.messages_loaded;
        session.message_count = session_message_count(record);
        session.session_mutation_stamp = Some(record.mutation_stamp);
        session
    }

    fn session_tail_start_index(record: &SessionRecord, message_limit: usize) -> usize {
        let retained_message_count =
            message_limit.min(SESSION_TAIL_HYDRATION_MAX_MESSAGES);
        record
            .session
            .messages
            .len()
            .saturating_sub(retained_message_count)
    }

    fn wire_session_tail_from_record(
        record: &SessionRecord,
        message_limit: usize,
        messages_loaded: bool,
    ) -> Session {
        let mut session = Self::wire_session_summary_from_record(record);
        let source_messages = &record.session.messages;
        let start_index = Self::session_tail_start_index(record, message_limit);
        debug_assert!(
            !messages_loaded || start_index == 0,
            "tail projection cannot mark a strict suffix as fully loaded"
        );
        session.messages = source_messages[start_index..].to_vec();
        session.messages_loaded = messages_loaded;
        session
    }

    fn wire_session_summary_from_record(record: &SessionRecord) -> Session {
        let session = &record.session;
        let summary = Session {
            id: session.id.clone(),
            name: session.name.clone(),
            emoji: session.emoji.clone(),
            agent: session.agent,
            workdir: session.workdir.clone(),
            project_id: session.project_id.clone(),
            // Keep this in sync with `wire_session_from_record`: record metadata
            // is the source of truth for remote-proxy ownership.
            remote_id: record.remote_id.clone(),
            model: session.model.clone(),
            model_options: session.model_options.clone(),
            approval_policy: session.approval_policy,
            reasoning_effort: session.reasoning_effort,
            sandbox_mode: session.sandbox_mode,
            cursor_mode: session.cursor_mode,
            claude_effort: session.claude_effort,
            claude_approval_mode: session.claude_approval_mode,
            gemini_approval_mode: session.gemini_approval_mode,
            external_session_id: session.external_session_id.clone(),
            agent_commands_revision: session.agent_commands_revision,
            codex_thread_state: session.codex_thread_state,
            status: session.status,
            preview: session.preview.clone(),
            messages: Vec::new(),
            messages_loaded: false,
            message_count: session_message_count(record),
            markers: session.markers.clone(),
            // Global state snapshots are metadata-first. Pending prompts can
            // contain user-authored prompt bodies, so expose them only through
            // targeted full-session responses.
            pending_prompts: Vec::new(),
            session_mutation_stamp: Some(record.mutation_stamp),
            parent_delegation_id: session.parent_delegation_id.clone(),
        };
        Self::debug_assert_session_summary_matches_full_projection(record, &summary);
        summary
    }

    #[cfg(debug_assertions)]
    fn debug_assert_session_summary_matches_full_projection(
        record: &SessionRecord,
        summary: &Session,
    ) {
        let full = Self::wire_session_from_record(record);
        debug_assert_eq!(summary.id, full.id);
        debug_assert_eq!(summary.name, full.name);
        debug_assert_eq!(summary.emoji, full.emoji);
        debug_assert_eq!(summary.agent, full.agent);
        debug_assert_eq!(summary.workdir, full.workdir);
        debug_assert_eq!(summary.project_id, full.project_id);
        debug_assert_eq!(summary.remote_id, full.remote_id);
        debug_assert_eq!(summary.model, full.model);
        debug_assert_eq!(summary.model_options, full.model_options);
        debug_assert_eq!(summary.approval_policy, full.approval_policy);
        debug_assert_eq!(summary.reasoning_effort, full.reasoning_effort);
        debug_assert_eq!(summary.sandbox_mode, full.sandbox_mode);
        debug_assert_eq!(summary.cursor_mode, full.cursor_mode);
        debug_assert_eq!(summary.claude_effort, full.claude_effort);
        debug_assert_eq!(summary.claude_approval_mode, full.claude_approval_mode);
        debug_assert_eq!(summary.gemini_approval_mode, full.gemini_approval_mode);
        debug_assert_eq!(summary.external_session_id, full.external_session_id);
        debug_assert_eq!(
            summary.agent_commands_revision,
            full.agent_commands_revision
        );
        debug_assert_eq!(summary.codex_thread_state, full.codex_thread_state);
        debug_assert_eq!(summary.status, full.status);
        debug_assert_eq!(summary.preview, full.preview);
        debug_assert_eq!(summary.message_count, full.message_count);
        debug_assert_eq!(summary.markers, full.markers);
        debug_assert!(summary.pending_prompts.is_empty());
        debug_assert_eq!(summary.session_mutation_stamp, full.session_mutation_stamp);
        debug_assert_eq!(summary.parent_delegation_id, full.parent_delegation_id);
    }

    #[cfg(not(debug_assertions))]
    fn debug_assert_session_summary_matches_full_projection(
        _record: &SessionRecord,
        _summary: &Session,
    ) {
    }

    /// Builds a metadata-first state snapshot with guaranteed-fresh agent readiness.
    ///
    /// The cache is refreshed (filesystem I/O) *before* locking `inner`, then
    /// the snapshot reads `cached_agent_readiness()` *under* the `inner` lock —
    /// the same path used by `commit_locked` / `publish_state_locked`.  This
    /// ensures that a `snapshot()` call at revision N uses the same cached
    /// readiness value that was published in the SSE event for revision N.
    fn snapshot(&self) -> StateResponse {
        self.summary_snapshot()
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

    /// Test-only full snapshot inspection helper.
    ///
    /// Production `/api/state`, action responses, and SSE state events are
    /// metadata-first. Tests that need full transcripts use this helper so
    /// `snapshot()` keeps the same shape in test and production builds.
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
        enum SessionLookup {
            Ready(SessionResponse),
            HydrateRemoteProxy,
        }

        let lookup = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            if record.remote_id.is_some()
                && record.remote_session_id.is_some()
                && !record.session.messages_loaded
            {
                SessionLookup::HydrateRemoteProxy
            } else {
                SessionLookup::Ready(SessionResponse {
                    revision: inner.revision,
                    session: Self::wire_session_from_record(record),
                    server_instance_id: self.server_instance_id.clone(),
                })
            }
        };

        match lookup {
            SessionLookup::Ready(response) => Ok(response),
            SessionLookup::HydrateRemoteProxy => {
                let target = match self.remote_session_target(session_id) {
                    Ok(Some(target)) => target,
                    Ok(None) => {
                        eprintln!(
                            "remote session hydration for {session_id} fell back to cached summary: missing remote target"
                        );
                        // There is no upstream remote error to preserve here:
                        // the proxy record changed between the initial
                        // visibility check and target resolution. Return the
                        // cached summary directly; if the local record also
                        // disappeared, the fallback helper tags that miss as
                        // `LocalSessionMissing`.
                        return self.get_session_hydration_fallback_response(session_id);
                    }
                    Err(err) => return Err(err),
                };
                self.hydrate_remote_session_target(
                    &target,
                    None,
                    None,
                    REMOTE_VISIBLE_SESSION_HYDRATION_TIMEOUT,
                )
                .or_else(|err| self.recover_visible_session_hydration(session_id, err))
            }
        }
    }

    /// Returns a visible session suffix without marking the transcript fully loaded.
    fn get_session_tail(
        &self,
        session_id: &str,
        message_limit: usize,
    ) -> Result<SessionResponse, ApiError> {
        let should_hydrate_remote_proxy = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            record.remote_id.is_some()
                && record.remote_session_id.is_some()
                && !record.session.messages_loaded
        };
        if should_hydrate_remote_proxy {
            // Reuse the full-session remote repair path, then project the
            // now-localized transcript into the requested tail window below.
            let _ = self.get_session(session_id)?;
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let record = &inner.sessions[index];
        let tail_start_index = Self::session_tail_start_index(record, message_limit);
        let messages_loaded = record.session.messages_loaded && tail_start_index == 0;
        Ok(SessionResponse {
            revision: inner.revision,
            session: Self::wire_session_tail_from_record(record, message_limit, messages_loaded),
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Recover a visible remote-proxy hydration miss with the best local
    /// cached summary. Non-recoverable remote errors pass through unchanged;
    /// if the local summary disappeared too, the typed local miss wins over
    /// the transient remote miss.
    fn recover_visible_session_hydration(
        &self,
        session_id: &str,
        err: ApiError,
    ) -> Result<SessionResponse, ApiError> {
        if !is_recoverable_remote_hydration_miss(&err) {
            return Err(err);
        }
        eprintln!(
            "remote session hydration for {session_id} fell back to cached summary: status={} kind={:?} message={}",
            err.status, err.kind, err.message
        );
        self.get_session_hydration_fallback_response(session_id)
            .map_err(|fallback_err| {
                eprintln!(
                    "remote session hydration fallback for {session_id} failed: status={} kind={:?} message={}",
                    fallback_err.status, fallback_err.kind, fallback_err.message
                );
                // `select_*` consumes both errors; log before this call if
                // future diagnostics need fields from either value.
                select_visible_session_hydration_fallback_error(err, fallback_err)
            })
    }

    /// Return the best local session shape when remote full-transcript
    /// hydration cannot produce a fresh loaded response. Unloaded remote-proxy
    /// sessions deliberately remain metadata-only (`messages_loaded = false`)
    /// so clients keep them in the hydration retry path. Callers should only
    /// use this for recoverable visible-pane misses; protocol and local
    /// persistence failures must keep surfacing as errors. This helper acquires
    /// `inner` itself; callers must not invoke it while holding the state lock.
    fn get_session_hydration_fallback_response(
        &self,
        session_id: &str,
    ) -> Result<SessionResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(ApiError::local_session_missing)?;
        let record = &inner.sessions[index];
        let session = if record.remote_id.is_some()
            && record.remote_session_id.is_some()
            && !record.session.messages_loaded
        {
            Self::wire_session_summary_from_record(record)
        } else {
            Self::wire_session_from_record(record)
        };
        Ok(SessionResponse {
            revision: inner.revision,
            session,
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
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.remote_applied_revisions.remove(remote_id);
        inner.remote_snapshot_applied_revisions.remove(remote_id);
        inner
            .remote_transcript_snapshot_applied_revisions
            .remove(remote_id);
        inner
            .remote_session_transcript_applied_revisions
            .remove(remote_id);
        self.remote_delta_replay_cache
            .lock()
            .expect("remote delta replay cache mutex poisoned")
            .remove_remote(remote_id);
        // Do not clear remote_delta_hydrations_in_flight here: those markers
        // are owned by RAII guards in the in-flight hydration callers. Removing
        // them during continuity cleanup would allow duplicate fetches while
        // the original request is still running.
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
            delegations: inner
                .delegations
                .iter()
                .map(delegation_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
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
            delegations: inner
                .delegations
                .iter()
                .map(delegation_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
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
            delegations: inner
                .delegations
                .iter()
                .map(delegation_summary_from_record)
                .collect(),
            delegation_waits: inner.delegation_waits.clone(),
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
