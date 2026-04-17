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
        let remote_session = remote_response
            .session
            .clone()
            .or_else(|| {
                remote_response.state.as_ref().and_then(|state| {
                    state
                        .sessions
                        .iter()
                        .find(|session| session.id == remote_response.session_id)
                        .cloned()
                })
            })
            .ok_or_else(|| ApiError::bad_gateway("remote forked session was not returned"))?;
        let local_project_id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.local_session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.project_id.clone()
        };
        let (revision, local_session_id, local_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = remote_response.state.as_ref().is_some_and(|state| {
                apply_remote_state_if_newer_locked(
                    &mut inner,
                    &target.remote.id,
                    state,
                    Some(&target.remote_session_id),
                )
            });
            let (local_session_id, changed) = ensure_remote_proxy_session_record(
                &mut inner,
                &target.remote.id,
                &remote_session,
                local_project_id,
                applied_remote_revision,
            );
            if applied_remote_revision {
                inner.note_remote_applied_revision(
                    &target.remote.id,
                    remote_response
                        .state
                        .as_ref()
                        .map(|state| state.revision)
                        .unwrap_or(remote_response.revision),
                );
            }
            let local_record = inner
                .find_session_index(&local_session_id)
                .and_then(|index| inner.sessions.get(index))
                .cloned()
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let local_session = local_record.session.clone();
            let revision = if applied_remote_revision {
                self.bump_revision_and_persist_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist remote forked session proxy: {err:#}"
                    ))
                })?
            } else if changed {
                self.commit_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote forked session proxy: {err:#}"
                        ))
                    })?
            } else {
                inner.revision
            };
            (revision, local_session_id, local_session)
        };
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: local_session.id.clone(),
            session: local_session.clone(),
        });

        Ok(CreateSessionResponse {
            session_id: local_session_id,
            session: Some(local_session),
            revision,
            state: None,
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
