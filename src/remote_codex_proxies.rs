// Remote Codex thread action proxies.
//
// Codex thread actions (fork, archive, unarchive, compact, rollback)
// mutate the remote Codex runtime's thread state and then mirror the
// resulting `StateResponse` back into local state. Pattern per
// method:
//
// 1. Resolve the `RemoteSessionTarget` from the local proxy session
//    id — errors out if the session isn't bound to a remote.
// 2. POST the thread-action payload to the remote's matching
//    endpoint under the `remote_session_id` namespace.
// 3. Fold the returned `StateResponse` back into local state via
//    `sync_remote_state_for_target`, which gates on revision to
//    avoid applying stale snapshots.
//
// The one exception is `proxy_remote_fork_codex_thread`: fork creates
// a brand-new remote session, so the local proxy record has to be
// newly persisted rather than updated in-place. It returns
// `CreateSessionResponse` instead of the usual `StateResponse`.
//
// The underlying thread actions themselves (what the remote does
// when it receives these calls) are in `codex_thread_actions.rs`.

impl AppState {

    /// Forks a remote Codex thread and persists a brand-new local proxy
    /// `SessionRecord` for the forked session, carrying the forked
    /// session's `remote_session_id` — this is the session-creation
    /// variant rather than a plain state-mutating proxy.
    fn proxy_remote_fork_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<CreateSessionResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_response: CreateSessionResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/fork",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        // Reject mismatched session identity on the wire — see
        // `create_remote_session_proxy` for rationale.
        if remote_response.session.id != remote_response.session_id {
            return Err(ApiError::bad_gateway(
                "remote forked session id mismatch: `session.id` does not equal `sessionId`",
            ));
        }
        let remote_session = remote_response.session.clone();
        let local_project_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.local_session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.project_id.clone()
        };
        let (revision, local_session_id, local_session, changed, delta_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            // Gate `update_existing` on the remote's applied-revision
            // tracking — see `create_remote_session_proxy` in
            // `remote_create_proxies.rs` for the full rationale. A
            // fork race against active streaming can leave the SSE
            // bridge at a later revision than this response carries,
            // in which case refreshing from the POST payload would
            // regress the mirrored state.
            let update_existing = !inner
                .should_skip_remote_applied_revision(
                    &target.remote.id,
                    remote_response.revision,
                );
            let (local_session_id, changed) = ensure_remote_proxy_session_record(
                &mut inner,
                &target.remote.id,
                &remote_session,
                local_project_id,
                update_existing,
            );
            if update_existing {
                inner.note_remote_applied_revision(
                    &target.remote.id,
                    remote_response.revision,
                );
            }
            let local_record = inner
                .find_session_index(&local_session_id)
                .and_then(|index| inner.sessions.get(index))
                .cloned()
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let local_session = AppState::wire_session_from_record(&local_record);
            let delta_session = AppState::wire_session_summary_from_record(&local_record);
            let revision = if changed {
                self.commit_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote forked session proxy: {err:#}"
                        ))
                    })?
            } else {
                inner.revision
            };
            (revision, local_session_id, local_session, changed, delta_session)
        };
        // Skip the SSE announcement on the no-change branch — see
        // the shared rationale on
        // [`AppState::announce_remote_session_created_if_changed`]
        // and its invocation from `remote_create_proxies.rs`.
        self.announce_remote_session_created_if_changed(
            changed,
            revision,
            &local_session_id,
            delta_session,
        );

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            session: local_session,
            revision,
            // Use THIS server's instance id, not the remote's — the
            // client's restart-detection ref is keyed to the local
            // instance it connects to, not the remote backend.
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn proxy_remote_archive_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/archive",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_unarchive_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/unarchive",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_compact_codex_thread(
        &self,
        session_id: &str,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/compact",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

    fn proxy_remote_rollback_codex_thread(
        &self,
        session_id: &str,
        num_turns: usize,
    ) -> Result<StateResponse, ApiError> {
        let Some(target) = self.remote_session_target(session_id)? else {
            return Err(ApiError::bad_request("session is not assigned to a remote"));
        };
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/sessions/{}/codex/thread/rollback",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            Some(json!({ "numTurns": num_turns })),
        )?;
        self.sync_remote_state_for_target(&target, remote_state)?;
        Ok(self.snapshot())
    }

}
