// Remote HTTP routes — AppState methods that proxy local API calls to
// another termal backend when the target session/project/orchestrator
// lives on a remote rather than in-process.
//
// Proxy pattern. Every inbound route first asks `remote_*_target` /
// `remote_scope_for_request` whether the caller's target belongs to a
// remote. If so the route forwards via this module instead of mutating
// local state directly, then reconciles the local proxy record from the
// response; otherwise it falls through to the local handler.
//
// Transport. Each active remote has a dedicated ssh `-L` forward onto a
// local port (allocated in src/remote_ssh.rs). The
// `BlockingHttpClientHandle` on `RemoteRegistry` issues blocking requests
// to `http://127.0.0.1:<forwarded_port>/api/...` and openssh tunnels them
// to the remote termal's http server on REMOTE_SERVER_PORT — the remote
// serves the same `/api` surface and does not know it is being proxied.
//
// Scoping. `RemoteScope { remote, remote_session_id?, remote_project_id? }`
// travels with every proxied call; `apply_remote_scope_to_query` /
// `apply_remote_scope_to_body` (src/remote_ssh.rs) splice it into a
// `sessionId` / `projectId` query parameter or JSON body field before
// the request goes out. `RemoteSessionTarget`, `RemoteOrchestratorTarget`,
// `RemoteProjectBinding` (src/remote.rs) are the narrower variants.
//
// State sync. State-mutating routes return a `StateResponse`;
// `sync_remote_state_for_target` / `apply_remote_state_snapshot` fold
// those into local state only when the remote revision is newer, then
// persist + publish. Out-of-band, `restore_remote_event_bridges` (called
// on boot) and `RemoteRegistry::start_event_bridge` spawn a long-running
// thread per remote that opens `/api/events` and feeds it to
// `process_remote_event_stream` in src/remote_sync.rs; that fan-out
// calls back into `apply_remote_state_snapshot` / `apply_remote_delta_event`
// here, and `resync_remote_state_snapshot` (src/remote_sync.rs) is the
// recovery path when a delta fails or an SSE-fallback flag is set.
//
// Timeouts. Most calls use REMOTE_REQUEST_TIMEOUT (30s). Terminal streams
// and `/api/events` reads use `request_without_timeout`; terminal command
// paths use REMOTE_TERMINAL_COMMAND_TIMEOUT; `remote_post_json_with_timeout`
// lets a caller pick its own budget.
//
// Errors. `decode_remote_json` (src/remote_ssh.rs) caps response reads at
// MAX_REMOTE_ERROR_BODY_BYTES and runs bodies through
// `sanitize_remote_error_body` before folding into `ApiError`, so hostile
// or oversized remote responses cannot flood local logs or ui toasts.
//
// Cross-references: src/remote.rs (RemoteRegistry, RemoteConnection,
// BlockingHttpClientHandle, scope + target/binding types); src/remote_ssh.rs
// (ssh argv, validators, decode_remote_json, apply_remote_scope_to_*);
// src/remote_sync.rs (remote → local application + event stream);
// src/remote_terminal.rs (terminal stream forwarding); src/tests/remote.rs
// (pin tests).

impl AppState {
    // -- event bridge lifecycle --

    /// Re-opens an event bridge to every remote that currently owns a local
    /// proxy session. Called once at boot after the persisted state is
    /// loaded so inbound SSE deltas keep flowing without waiting for a
    /// first outbound request to touch each remote.
    fn restore_remote_event_bridges(&self) {
        let remotes = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| record.remote_id.as_deref())
                .filter_map(|remote_id| inner.find_remote(remote_id))
                .cloned()
                .collect::<Vec<_>>()
        };

        for remote in remotes {
            self.remote_registry
                .start_event_bridge(self.clone(), &remote);
        }
    }

    // -- scope resolution --
    // Turn local identifiers into `RemoteSessionTarget` /
    // `RemoteOrchestratorTarget` / `RemoteScope` (or `None` if the target
    // lives locally), looking up the associated `RemoteConfig` and the
    // `remote_session_id` / `remote_orchestrator_id` recorded on the local
    // proxy record. These are the first call every proxy method makes.

    /// Resolves a local session id to its remote counterpart, returning
    /// `None` if the session has no `remote_id`/`remote_session_id` and
    /// therefore lives locally. Errors only if the local session or the
    /// remote config itself is missing.
    fn remote_session_target(
        &self,
        session_id: &str,
    ) -> Result<Option<RemoteSessionTarget>, ApiError> {
        let (remote_id, remote_session_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            let Some(remote_id) = record.remote_id.clone() else {
                return Ok(None);
            };
            let Some(remote_session_id) = record.remote_session_id.clone() else {
                return Ok(None);
            };
            (remote_id, remote_session_id)
        };
        let remote = self.lookup_remote_config(&remote_id)?;
        Ok(Some(RemoteSessionTarget {
            local_session_id: session_id.to_owned(),
            remote,
            remote_session_id,
        }))
    }

    /// Mirror of `remote_session_target` for orchestrator instances.
    fn remote_orchestrator_target(
        &self,
        instance_id: &str,
    ) -> Result<Option<RemoteOrchestratorTarget>, ApiError> {
        let (remote_id, remote_orchestrator_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            let Some(remote_id) = instance.remote_id.clone() else {
                return Ok(None);
            };
            let Some(remote_orchestrator_id) = instance.remote_orchestrator_id.clone() else {
                return Ok(None);
            };
            (remote_id, remote_orchestrator_id)
        };
        let remote = self.lookup_remote_config(&remote_id)?;
        Ok(Some(RemoteOrchestratorTarget {
            local_instance_id: instance_id.to_owned(),
            remote,
            remote_orchestrator_id,
        }))
    }

    /// Peeks whether a terminal request with the given identifiers would
    /// resolve to a remote scope, using only in-memory state (no network
    /// I/O). Callers use this to decide which concurrency semaphore to
    /// acquire before invoking `remote_scope_for_request`, which can
    /// otherwise trigger `ensure_remote_project_binding`'s unbounded
    /// `POST /api/projects` call outside the 429 rate limit on a burst of
    /// first-time remote terminal requests.
    fn terminal_request_is_remote(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(session_id) = normalize_optional_identifier(session_id) {
            if let Some(index) = inner.find_session_index(session_id) {
                let record = &inner.sessions[index];
                if record.remote_id.is_some() && record.remote_session_id.is_some() {
                    return true;
                }
            }
        }

        if let Some(project_id) = normalize_optional_identifier(project_id) {
            if let Some(project) = inner.find_project(project_id) {
                if project.remote_id != LOCAL_REMOTE_ID {
                    return true;
                }
            }
        }

        false
    }

    /// Generic target resolver used by routes that accept either a session
    /// id or a project id (terminal endpoints, some diagnostics). Prefers
    /// the session's remote when both are provided and, for project-only
    /// requests, calls `ensure_remote_project_binding` which may issue a
    /// `POST /api/projects` if this is the first time the project is being
    /// bound on this remote — callers on a hot path should gate that with
    /// `terminal_request_is_remote` first.
    fn remote_scope_for_request(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<Option<RemoteScope>, ApiError> {
        if let Some(session_id) = normalize_optional_identifier(session_id) {
            if let Some(target) = self.remote_session_target(session_id)? {
                return Ok(Some(RemoteScope {
                    remote: target.remote,
                    remote_project_id: None,
                    remote_session_id: Some(target.remote_session_id),
                }));
            }
        }

        if let Some(project_id) = normalize_optional_identifier(project_id) {
            if let Some(binding) = self.ensure_remote_project_binding(project_id)? {
                return Ok(Some(RemoteScope {
                    remote: binding.remote,
                    remote_project_id: Some(binding.remote_project_id),
                    remote_session_id: None,
                }));
            }
        }

        Ok(None)
    }

    // -- scoped http proxy helpers --
    // Thin wrappers around `RemoteRegistry::request_json` that splice the
    // active `RemoteScope` into the query string (GET/DELETE/some PUTs) or
    // body object (POST/PUT) via `apply_remote_scope_to_*` before sending.
    // All default to REMOTE_REQUEST_TIMEOUT; `_with_timeout` overrides it,
    // `_response_without_timeout` returns the raw response so long-running
    // bodies (streams, events) can be read incrementally.

    fn remote_get_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        mut query: Vec<(String, String)>,
    ) -> Result<T, ApiError> {
        apply_remote_scope_to_query(scope, &mut query);
        self.remote_registry
            .request_json(&scope.remote, Method::GET, path, &query, None)
    }

    fn remote_post_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    fn remote_post_json_with_timeout<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json_with_timeout(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
            timeout,
        )
    }

    fn remote_post_response_without_timeout(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<BlockingHttpResponse, ApiError> {
        self.remote_registry.request_without_timeout(
            &scope.remote,
            Method::POST,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    fn remote_put_json<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        body: Value,
    ) -> Result<T, ApiError> {
        self.remote_registry.request_json(
            &scope.remote,
            Method::PUT,
            path,
            &[],
            Some(apply_remote_scope_to_body(scope, body)),
        )
    }

    fn remote_put_json_with_query_scope<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        path: &str,
        mut query: Vec<(String, String)>,
        body: Value,
    ) -> Result<T, ApiError> {
        apply_remote_scope_to_query(scope, &mut query);
        self.remote_registry
            .request_json(&scope.remote, Method::PUT, path, &query, Some(body))
    }

    // -- remote config + project binding --
    // Helpers for finding a `RemoteConfig` by id and lazily creating the
    // paired remote project (when a local project first needs a remote
    // counterpart) so subsequent proxy calls can use `remote_project_id`.

    fn lookup_remote_config(&self, remote_id: &str) -> Result<RemoteConfig, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_remote(remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))
    }

    /// Returns the `RemoteProjectBinding` for a local project, creating the
    /// remote project via `POST /api/projects` and persisting the
    /// `remote_project_id` on the local record if one does not yet exist.
    /// Returns `None` for local-only projects (those with
    /// `remote_id == LOCAL_REMOTE_ID`).
    fn ensure_remote_project_binding(
        &self,
        project_id: &str,
    ) -> Result<Option<RemoteProjectBinding>, ApiError> {
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("project not found"))?
        };
        if project.remote_id == LOCAL_REMOTE_ID {
            return Ok(None);
        }

        let remote = self.lookup_remote_config(&project.remote_id)?;
        validate_remote_connection_config(&remote)?;
        if let Some(remote_project_id) = project.remote_project_id.clone() {
            return Ok(Some(RemoteProjectBinding {
                local_project_id: project.id,
                remote,
                remote_project_id,
            }));
        }

        let response: CreateProjectResponse = self.remote_registry.request_json(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": project.name,
                "rootPath": project.root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )?;

        let remote_project_id = response.project_id;
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .projects
                .iter()
                .position(|candidate| candidate.id == project.id)
                .ok_or_else(|| ApiError::not_found("project not found"))?;
            if inner.projects[index].remote_project_id.as_deref()
                != Some(remote_project_id.as_str())
            {
                inner.projects[index].remote_project_id = Some(remote_project_id.clone());
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist remote project binding: {err:#}"))
                })?;
            }
        }

        Ok(Some(RemoteProjectBinding {
            local_project_id: project.id,
            remote,
            remote_project_id,
        }))
    }
    // -- project + session + orchestrator creation proxies --
    // These handle the 'first touch' path: create the remote-side object
    // via POST, then upsert a local proxy record so subsequent lookups can
    // resolve through `remote_*_target`. Each one also kicks off or
    // reuses an event bridge on success so the newly-created entity's
    // deltas start streaming back.


    // -- session proxies --
    // One method per remote session route. The shape is uniform: resolve
    // the `RemoteSessionTarget`, forward the call, then fold the returned
    // `StateResponse` (when present) into local state via
    // `sync_remote_state_for_target`. Individual methods below are only
    // annotated when they deviate from that shape.

    /// Session-scoped counterpart to `apply_remote_state_snapshot`: folds
    /// a `StateResponse` returned from a remote session route into local
    /// state only if its revision is newer, persists the change, and
    /// records the applied remote revision. Used by most session proxies
    /// after they forward the call.
    fn sync_remote_state_for_target(
        &self,
        target: &RemoteSessionTarget,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !apply_remote_state_if_newer_locked(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
        ) {
            return Ok(());
        }
        inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    // -- orchestrator lifecycle proxies --
    // Pause / resume / stop all go through `proxy_remote_orchestrator_state_action`
    // which factors out the common 'forward POST, fold new state if newer,
    // persist' plumbing.

    fn proxy_remote_pause_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "pause")
    }

    fn proxy_remote_resume_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "resume")
    }

    fn proxy_remote_stop_orchestrator_instance(
        &self,
        target: RemoteOrchestratorTarget,
    ) -> Result<StateResponse, ApiError> {
        self.proxy_remote_orchestrator_state_action(target, "stop")
    }

    /// Shared implementation for the `pause`/`resume`/`stop` orchestrator
    /// routes: POSTs to `/api/orchestrators/<remote_id>/<action>`, folds
    /// the returned `StateResponse` in if newer, persists, and returns a
    /// fresh local snapshot.
    fn proxy_remote_orchestrator_state_action(
        &self,
        target: RemoteOrchestratorTarget,
        action: &str,
    ) -> Result<StateResponse, ApiError> {
        let remote_state: StateResponse = self.remote_registry.request_json(
            &target.remote,
            Method::POST,
            &format!(
                "/api/orchestrators/{}/{}",
                encode_uri_component(&target.remote_orchestrator_id),
                action
            ),
            &[],
            None,
        )?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if apply_remote_state_if_newer_locked(&mut inner, &target.remote.id, &remote_state, None)
        {
            inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist remote orchestrator `{}` state: {err:#}",
                    target.local_instance_id
                ))
            })?;
        }
        Ok(self.snapshot_from_inner(&inner))
    }

    // -- inbound remote event application --
    // Called from the event-bridge thread in src/remote_sync.rs to fold
    // inbound `state` snapshots and `delta` frames into local state. Each
    // applied frame also publishes a local `DeltaEvent` so connected
    // browsers see the update on their own SSE stream.

    /// Folds a full `StateResponse` from a remote into local state only
    /// if its revision is newer than what we have applied from that
    /// remote; no-op otherwise. Used both by routes that return a fresh
    /// snapshot and by the `state` event handler in `process_remote_event_stream`.
    fn apply_remote_state_snapshot(
        &self,
        remote_id: &str,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None) {
            return Ok(());
        }
        inner.note_remote_applied_revision(remote_id, remote_state.revision);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    /// Applies a single `DeltaEvent` from a remote's SSE stream to local
    /// state and re-publishes it under the matching local session /
    /// orchestrator ids. Remote ids in the payload (session_id,
    /// project_id, orchestrator_id) are remapped to their local proxy
    /// counterparts before publish. Errors here cause
    /// `dispatch_remote_event` (src/remote_sync.rs) to fall back to
    /// `resync_remote_state_snapshot`.
    fn apply_remote_delta_event(
        &self,
        remote_id: &str,
        event: DeltaEvent,
    ) -> Result<(), anyhow::Error> {
        let remote_revision = delta_event_revision(&event);
        match event {
            DeltaEvent::SessionCreated {
                session,
                session_id,
                ..
            } => {
                if session.id != session_id {
                    return Err(anyhow!(
                        "remote created session payload id `{}` did not match event id `{session_id}`",
                        session.id
                    ));
                }
                let (local_session, revision) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let local_project_id = local_project_id_for_remote_project(
                        &local_project_ids_by_remote_project_id,
                        session.project_id.as_deref(),
                    );
                    let (local_session_id, changed) = ensure_remote_proxy_session_record(
                        &mut inner,
                        remote_id,
                        &session,
                        local_project_id.map(LocalProjectId::into_inner),
                        true,
                    );
                    let local_record = inner
                        .find_session_index(&local_session_id)
                        .and_then(|index| inner.sessions.get(index))
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                    let local_session = AppState::wire_session_from_record(&local_record);
                    let revision = if changed {
                        self.commit_session_created_locked(&mut inner, &local_record)?
                    } else {
                        inner.revision
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session, revision)
                };
                self.publish_delta(&DeltaEvent::SessionCreated {
                    revision,
                    session_id: local_session.id.clone(),
                    session: local_session,
                });
            }
            DeltaEvent::MessageCreated {
                message,
                message_id,
                message_index,
                preview,
                session_id,
                status,
                ..
            } => {
                let (local_session_id, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if message_index_on_record(record, &message_id).is_none() {
                            insert_message_on_record(record, message_index, message.clone());
                        }
                        record.session.preview = preview.clone();
                        record.session.status = status;
                        (record.session.id.clone(), record.mutation_stamp)
                    };
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, revision, session_mutation_stamp)
                };
                self.publish_delta(&DeltaEvent::MessageCreated {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message,
                    preview,
                    status,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
            }
            DeltaEvent::TextDelta {
                delta,
                message_id,
                preview,
                session_id,
                ..
            } => {
                let (local_session_id, message_index, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, message_index, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let message_index = message_index_on_record(record, &message_id)
                            .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                        let Some(message) = record.session.messages.get_mut(message_index) else {
                            return Err(anyhow!(
                                "remote message index `{message_index}` is out of bounds"
                            ));
                        };
                        match message {
                            Message::Text { text, .. } => text.push_str(&delta),
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a text message"
                                ));
                            }
                        }
                        if let Some(next_preview) = preview.as_ref() {
                            record.session.preview = next_preview.clone();
                        }
                        (
                            record.session.id.clone(),
                            message_index,
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, message_index, revision, session_mutation_stamp)
                };
                self.publish_delta(&DeltaEvent::TextDelta {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    delta,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
            }
            DeltaEvent::TextReplace {
                message_id,
                preview,
                session_id,
                text,
                ..
            } => {
                let (local_session_id, message_index, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, message_index, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let message_index = message_index_on_record(record, &message_id)
                            .ok_or_else(|| anyhow!("remote message `{message_id}` not found"))?;
                        let Some(message) = record.session.messages.get_mut(message_index) else {
                            return Err(anyhow!(
                                "remote message index `{message_index}` is out of bounds"
                            ));
                        };
                        match message {
                            Message::Text {
                                text: current_text, ..
                            } => {
                                current_text.clear();
                                current_text.push_str(&text);
                            }
                            _ => {
                                return Err(anyhow!(
                                    "remote message `{message_id}` is not a text message"
                                ));
                            }
                        }
                        if let Some(next_preview) = preview.as_ref() {
                            record.session.preview = next_preview.clone();
                        }
                        (
                            record.session.id.clone(),
                            message_index,
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (local_session_id, message_index, revision, session_mutation_stamp)
                };
                self.publish_delta(&DeltaEvent::TextReplace {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    text,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
            }
            DeltaEvent::CommandUpdate {
                command,
                command_language,
                message_id,
                message_index,
                output,
                output_language,
                preview,
                session_id,
                status,
                ..
            } => {
                let (
                    local_session_id,
                    created_message,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, created_message, session_status, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let created_message = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index) else {
                                return Err(anyhow!(
                                    "remote message index `{existing_index}` is out of bounds"
                                ));
                            };
                            match message {
                                Message::Command {
                                    command: existing_command,
                                    command_language: existing_command_language,
                                    output: existing_output,
                                    output_language: existing_output_language,
                                    status: existing_status,
                                    ..
                                } => {
                                    *existing_command = command.clone();
                                    *existing_command_language = command_language.clone();
                                    *existing_output = output.clone();
                                    *existing_output_language = output_language.clone();
                                    *existing_status = status;
                                    None
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a command message"
                                    ));
                                }
                            }
                        } else {
                            let message = Message::Command {
                                id: message_id.clone(),
                                timestamp: stamp_now(),
                                author: Author::Assistant,
                                command: command.clone(),
                                command_language: command_language.clone(),
                                output: output.clone(),
                                output_language: output_language.clone(),
                                status,
                            };
                            insert_message_on_record(record, message_index, message.clone());
                            Some(message)
                        };
                        record.session.preview = preview.clone();
                        (
                            record.session.id.clone(),
                            created_message,
                            record.session.status,
                            record.mutation_stamp,
                        )
                    };
                    let revision = if created_message.is_some() {
                        self.commit_persisted_delta_locked(&mut inner)?
                    } else {
                        self.commit_delta_locked(&mut inner)?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        created_message,
                        revision,
                        session_status,
                        session_mutation_stamp,
                    )
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        message,
                        preview,
                        status: session_status,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                } else {
                    self.publish_delta(&DeltaEvent::CommandUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        command,
                        command_language,
                        output,
                        output_language,
                        status,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
            }
            DeltaEvent::ParallelAgentsUpdate {
                agents,
                message_id,
                message_index,
                preview,
                session_id,
                ..
            } => {
                let (
                    local_session_id,
                    created_message,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, created_message, session_status, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let created_message = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index) else {
                                return Err(anyhow!(
                                    "remote message index `{existing_index}` is out of bounds"
                                ));
                            };
                            match message {
                                Message::ParallelAgents {
                                    agents: existing_agents,
                                    ..
                                } => {
                                    *existing_agents = agents.clone();
                                    None
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a parallel-agents message"
                                    ));
                                }
                            }
                        } else {
                            let message = Message::ParallelAgents {
                                id: message_id.clone(),
                                timestamp: stamp_now(),
                                author: Author::Assistant,
                                agents: agents.clone(),
                            };
                            insert_message_on_record(record, message_index, message.clone());
                            Some(message)
                        };
                        record.session.preview = preview.clone();
                        (
                            record.session.id.clone(),
                            created_message,
                            record.session.status,
                            record.mutation_stamp,
                        )
                    };
                    let revision = if created_message.is_some() {
                        self.commit_persisted_delta_locked(&mut inner)?
                    } else {
                        self.commit_delta_locked(&mut inner)?
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        created_message,
                        revision,
                        session_status,
                        session_mutation_stamp,
                    )
                };
                if let Some(message) = created_message {
                    self.publish_delta(&DeltaEvent::MessageCreated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        message,
                        preview,
                        status: session_status,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                } else {
                    self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index,
                        agents,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
            }
            DeltaEvent::OrchestratorsUpdated {
                orchestrators,
                sessions,
                ..
            } => {
                let (revision, localized_orchestrators) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                        return Ok(());
                    }
                    let local_project_ids_by_remote_project_id =
                        remote_project_id_map(&inner, remote_id);
                    let remote_sessions_by_id = (!sessions.is_empty()).then(|| {
                        sessions
                            .iter()
                            .map(|session| (session.id.as_str(), session))
                            .collect::<HashMap<_, _>>()
                    });
                    let rollback_state = (
                        inner.next_session_number,
                        inner.sessions.clone(),
                        inner.orchestrator_instances.clone(),
                    );
                    if let Err(err) = sync_remote_orchestrators_inner(
                        &mut inner,
                        remote_id,
                        &orchestrators,
                        &local_project_ids_by_remote_project_id,
                        remote_sessions_by_id.as_ref(),
                    ) {
                        inner.next_session_number = rollback_state.0;
                        inner.sessions = rollback_state.1;
                        inner.orchestrator_instances = rollback_state.2;
                        return Err(err);
                    }
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (revision, inner.orchestrator_instances.clone())
                };
                self.publish_orchestrators_updated(revision, localized_orchestrators);
            }
            DeltaEvent::CodexUpdated { .. } => {
                // CodexState is process-global runtime metadata, not localized
                // remote proxy state. Mark the remote revision consumed so this
                // informational delta does not force a snapshot resync.
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                    return Ok(());
                }
                inner.note_remote_applied_revision(remote_id, remote_revision);
            }
        }
        Ok(())
    }
}
