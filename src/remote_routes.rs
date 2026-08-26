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
// on boot) and `RemoteRegistry::start_event_bridge_by_id` spawn a long-running
// thread per remote that opens `/api/events` and feeds it to
// `process_remote_event_stream` in src/remote_sync.rs; that fan-out
// calls back into `apply_remote_state_snapshot` here and
// `apply_remote_delta_event` in src/remote_delta_apply.rs;
// `resync_remote_state_snapshot_with_authority` (src/remote_sync.rs) is the recovery path
// when a delta fails or an SSE-fallback flag is set.
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

#[derive(Clone, Copy)]
struct RemoteDeltaHydrationExpectation {
    message_count: u32,
    session_mutation_stamp: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteDeltaHydrationOutcome {
    Continue,
    SkipApplied,
    SkipInFlight,
}

struct RemoteDeltaHydrationInFlightGuard {
    in_flight: Arc<Mutex<HashSet<RemoteDeltaHydrationKey>>>,
    key: RemoteDeltaHydrationKey,
}

impl Drop for RemoteDeltaHydrationInFlightGuard {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .remove(&self.key);
    }
}

impl AppState {
    // -- event bridge lifecycle --

    /// Re-opens an event bridge to every remote that currently owns a local
    /// proxy session. Called once at boot after the persisted state is
    /// loaded so inbound SSE deltas keep flowing without waiting for a
    /// first outbound request to touch each remote.
    fn restore_remote_event_bridges(&self) {
        let mut remote_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| record.remote_id.as_deref())
                // Persisted sessions can outlive a removed remote. They remain
                // visible as detached history, but no bridge can be restored
                // until settings define that authority again.
                .filter(|remote_id| inner.find_remote(remote_id).is_some())
                .map(str::to_owned)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        remote_ids.sort();

        for remote_id in remote_ids {
            self.start_remote_event_bridge_by_id(&remote_id);
        }
    }

    /// Starts the event bridge from the current settings-owned configuration.
    ///
    /// Remote operations can retain a config snapshot while network I/O is in
    /// flight. Resolving by id here prevents bridge startup from reintroducing
    /// a stale endpoint after a concurrent settings update.
    fn start_remote_event_bridge_by_id(&self, remote_id: &str) {
        self.remote_registry
            .start_event_bridge_by_id(self.clone(), remote_id);
    }

    /// Starts a bridge only if the decoded response's exact connection still
    /// owns routing authority. Create flows use this instead of resolving by
    /// id so a response from endpoint A cannot accidentally claim endpoint B.
    fn start_remote_event_bridge_for_lease(
        &self,
        lease: &RemoteRequestLease,
    ) -> Result<(), ApiError> {
        self.remote_registry
            .start_event_bridge_for_lease(self.clone(), lease)
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
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            let Some((remote_id, remote_session_id)) = record
                .remote_proxy_identity()
                .map_err(|err| ApiError::internal(format!("invalid session proxy: {err:#}")))?
            else {
                return Ok(None);
            };
            (remote_id.to_owned(), remote_session_id.to_owned())
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
                // This is only a semaphore-selection hint. Invalid identities
                // deliberately choose the local permit here; the subsequent
                // authoritative resolver rejects them with a typed error.
                if record.is_remote_proxy() {
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

    fn remote_post_json_with_timeout_and_authority_for_lease<T: DeserializeOwned>(
        &self,
        scope: &RemoteScope,
        lease: RemoteRequestLease,
        path: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<(T, RemoteStreamingAuthority), ApiError> {
        if !same_remote_routing_config(&lease.pinned, &scope.remote) {
            return Err(ApiError::internal(
                "remote terminal fallback lease does not match its resolved scope",
            ));
        }
        self.remote_registry.request_json_with_timeout_for_lease(
            lease,
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
    ) -> Result<RemoteStreamingResponse, ApiError> {
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
        self.ensure_remote_project_binding_with_missing_status(
            project_id,
            RemoteCreateMissingProjectStatus::NotFound,
        )
    }

    fn ensure_remote_project_binding_with_missing_status(
        &self,
        project_id: &str,
        missing_project_status: RemoteCreateMissingProjectStatus,
    ) -> Result<Option<RemoteProjectBinding>, ApiError> {
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(project_id)
                .cloned()
                .ok_or_else(|| {
                    missing_project_status.error(format!("unknown project `{project_id}`"))
                })?
        };
        if project.remote_id == LOCAL_REMOTE_ID {
            return Ok(None);
        }

        let remote = self.lookup_remote_config(&project.remote_id)?;
        validate_remote_connection_config(&remote)?;
        if let Some(remote_project_id) = project.remote_project_id.clone() {
            #[cfg(test)]
            self.remote_registry
                .run_test_before_existing_remote_project_revalidation();
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let current_project = inner.find_project(project_id).ok_or_else(|| {
                missing_project_status.error(format!("unknown project `{project_id}`"))
            })?;
            if current_project.remote_id != project.remote_id
                || current_project.remote_project_id.as_deref()
                    != Some(remote_project_id.as_str())
            {
                return Err(ApiError::conflict(
                    REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE,
                ));
            }
            let current_remote = inner
                .find_remote(&current_project.remote_id)
                .cloned()
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "unknown remote `{}`",
                        current_project.remote_id
                    ))
                })?;
            validate_remote_connection_config(&current_remote)?;
            let binding = RemoteProjectBinding {
                local_project_id: current_project.id.clone(),
                remote: current_remote,
                remote_project_id: remote_project_id.clone(),
            };
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to retry remote project binding persistence: {err:#}"
                    ))
                })?;
            return Ok(Some(binding));
        }

        let (response, response_lease): (CreateProjectResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_lease(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": project.name,
                "rootPath": project.root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )
        .map_err(remote_create_authority_error)?;

        let response_remote_project_id = response.project_id;
        let (remote, remote_project_id) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            self.ensure_remote_create_request_current_locked(&inner, &response_lease)?;
            let current_remote = inner
                .find_remote(&remote.id)
                .cloned()
                .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{}`", remote.id)))?;
            if !same_remote_routing_config(&current_remote, &remote) {
                return Err(ApiError::conflict(
                    REMOTE_CONNECTION_CHANGED_DURING_CREATE,
                ));
            }
            let index = inner
                .projects
                .iter()
                .position(|candidate| candidate.id == project.id)
                .ok_or_else(|| {
                    missing_project_status.error(format!("unknown project `{}`", project.id))
                })?;
            if inner.projects[index].remote_id != project.remote_id
                || inner.projects[index].root_path != project.root_path
            {
                return Err(ApiError::conflict(
                    REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE,
                ));
            }
            let remote_project_id = if let Some(existing) =
                inner.projects[index].remote_project_id.clone()
            {
                // First writer wins when duplicate lazy-binding requests race.
                // The later remote-side project may be orphaned, but local
                // authority must never oscillate between POST responses.
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to retry remote project binding persistence: {err:#}"
                        ))
                    })?;
                existing
            } else {
                inner.projects[index].remote_project_id =
                    Some(response_remote_project_id.clone());
                self.commit_remote_localization_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist remote project binding: {err:#}"))
                })?;
                response_remote_project_id
            };
            (current_remote, remote_project_id)
        };

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
        response_lease: &RemoteRequestLease,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_request_current_locked(&inner, response_lease)?;
        self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to retry dirty remote state persistence: {err:#}"
                ))
            })?;
        if !apply_remote_state_if_newer_locked(
            &mut inner,
            &target.remote.id,
            &remote_state,
            Some(&target.remote_session_id),
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        ) {
            return Ok(());
        }
        inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
        self.commit_remote_localization_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    fn command_status_replay_code(status: CommandStatus) -> u8 {
        match status {
            CommandStatus::Running => 0,
            CommandStatus::Success => 1,
            CommandStatus::Error => 2,
        }
    }

    fn session_status_replay_code(status: SessionStatus) -> u8 {
        match status {
            SessionStatus::Active => 0,
            SessionStatus::Idle => 1,
            SessionStatus::Approval => 2,
            SessionStatus::Error => 3,
        }
    }

    fn remote_delta_payload_fingerprint<T: Serialize>(payload: &T) -> Option<String> {
        match serde_json::to_vec(payload) {
            Ok(encoded) => Some(format!("{:x}", Sha256::digest(encoded))),
            Err(err) => {
                eprintln!(
                    "remote delta replay> failed to fingerprint {} payload: {err}",
                    std::any::type_name::<T>()
                );
                None
            }
        }
    }

    fn remote_delta_session_payload_fingerprint(session: &Session) -> Option<String> {
        let mut normalized = session.clone();
        // `localize_remote_session` discards inbound wire ownership, so replay
        // identity must ignore it too.
        normalized.remote_id = None;
        Self::remote_delta_payload_fingerprint(&normalized)
    }

    fn remote_delta_text_fingerprint(payload: &str) -> String {
        format!("{:x}", Sha256::digest(payload.as_bytes()))
    }

    /// Builds the exact replay-suppression key for one remote delta.
    ///
    /// Returns `None` when any payload field cannot be JSON-serialized. The
    /// monotonic `remote_applied_revisions` watermark remains the authoritative
    /// ordering defense, so the replay cache is safe to skip per delta.
    fn remote_delta_replay_key_for_generation(
        remote_id: &str,
        authority_generation: u64,
        event: &DeltaEvent,
    ) -> Option<RemoteDeltaReplayKey> {
        let payload = match event {
            DeltaEvent::SessionCreated {
                session_id,
                session,
                ..
            } => RemoteDeltaReplayPayload::SessionCreated {
                session_id: session_id.clone(),
                message_count: session.message_count,
                session_fingerprint: Self::remote_delta_session_payload_fingerprint(session)?,
                session_mutation_stamp: session.session_mutation_stamp,
            },
            DeltaEvent::MessageCreated {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::MessageCreated {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                message_fingerprint: Self::remote_delta_payload_fingerprint(message)?,
                preview_fingerprint: Self::remote_delta_text_fingerprint(preview),
                status: Self::session_status_replay_code(*status),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::MessageUpdated {
                session_id,
                message_id,
                message_index,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::MessageUpdated {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                message_fingerprint: Self::remote_delta_payload_fingerprint(message)?,
                preview_fingerprint: Self::remote_delta_text_fingerprint(preview),
                status: Self::session_status_replay_code(*status),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::TextDelta {
                session_id,
                message_id,
                message_index,
                message_count,
                text_start_byte,
                delta,
                preview,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::TextDelta {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                text_start_byte: *text_start_byte,
                delta_fingerprint: Self::remote_delta_text_fingerprint(delta),
                preview_fingerprint: preview.as_deref().map(Self::remote_delta_text_fingerprint),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::TextReplace {
                session_id,
                message_id,
                message_index,
                message_count,
                text,
                preview,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::TextReplace {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                text_fingerprint: Self::remote_delta_text_fingerprint(text),
                preview_fingerprint: preview.as_deref().map(Self::remote_delta_text_fingerprint),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::CommandUpdate {
                session_id,
                message_id,
                message_index,
                message_count,
                command,
                command_language,
                output,
                output_language,
                status,
                preview,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::CommandUpdate {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                command_fingerprint: Self::remote_delta_text_fingerprint(command),
                command_language: command_language.clone(),
                output_fingerprint: Self::remote_delta_text_fingerprint(output),
                output_language: output_language.clone(),
                status: Self::command_status_replay_code(*status),
                preview_fingerprint: Self::remote_delta_text_fingerprint(preview),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                message_id,
                message_index,
                message_count,
                agents,
                preview,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::ParallelAgentsUpdate {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
                // Parallel-agent deltas replace the displayed agent list as a
                // unit, so one list-level fingerprint captures order, add,
                // remove, and per-agent field changes without retaining text.
                agents_fingerprint: Self::remote_delta_payload_fingerprint(agents)?,
                preview_fingerprint: Self::remote_delta_text_fingerprint(preview),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::ConversationMarkerCreated {
                session_id,
                marker,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::ConversationMarkerCreated {
                session_id: session_id.clone(),
                marker_id: marker.id.clone(),
                marker_fingerprint: Self::remote_delta_payload_fingerprint(marker)?,
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::ConversationMarkerUpdated {
                session_id,
                marker,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::ConversationMarkerUpdated {
                session_id: session_id.clone(),
                marker_id: marker.id.clone(),
                marker_fingerprint: Self::remote_delta_payload_fingerprint(marker)?,
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::ConversationMarkerDeleted {
                session_id,
                marker_id,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::ConversationMarkerDeleted {
                session_id: session_id.clone(),
                marker_id: marker_id.clone(),
                session_mutation_stamp: *session_mutation_stamp,
            },
            DeltaEvent::CodexUpdated { revision: _, codex } => {
                RemoteDeltaReplayPayload::CodexUpdated {
                    codex_fingerprint: Self::remote_delta_payload_fingerprint(codex)?,
                }
            }
            DeltaEvent::OrchestratorsUpdated {
                orchestrators,
                sessions,
                ..
            } => RemoteDeltaReplayPayload::OrchestratorsUpdated {
                orchestrator_fingerprints: orchestrators
                    .iter()
                    .map(Self::remote_delta_payload_fingerprint)
                    .collect::<Option<Vec<_>>>()?,
                session_fingerprints: sessions
                    .iter()
                    .map(Self::remote_delta_session_payload_fingerprint)
                    .collect::<Option<Vec<_>>>()?,
            },
            DeltaEvent::DelegationCreated { .. }
            | DeltaEvent::DelegationWaitCreated { .. }
            | DeltaEvent::DelegationWaitConsumed { .. }
            | DeltaEvent::DelegationWaitResumeDispatchFailed { .. }
            | DeltaEvent::DelegationUpdated { .. }
            | DeltaEvent::DelegationCompleted { .. }
            | DeltaEvent::DelegationFailed { .. }
            | DeltaEvent::DelegationCanceled { .. } => return None,
        };
        Some(RemoteDeltaReplayKey {
            remote_id: remote_id.to_owned(),
            authority_generation,
            revision: delta_event_revision(event),
            payload,
        })
    }

    #[cfg(test)]
    fn remote_delta_replay_key(
        remote_id: &str,
        event: &DeltaEvent,
    ) -> Option<RemoteDeltaReplayKey> {
        Self::remote_delta_replay_key_for_generation(remote_id, 0, event)
    }

    /// Explicit no-op for `None` keys so callers can plumb optional replay
    /// keys without branching.
    fn should_skip_remote_applied_delta_replay(&self, key: &Option<RemoteDeltaReplayKey>) -> bool {
        key.as_ref().is_some_and(|key| {
            self.remote_delta_replay_cache
                .lock()
                .expect("remote delta replay cache mutex poisoned")
                .contains(key)
        })
    }

    /// Explicit no-op for `None` keys; an unserializable delta still advances
    /// through the monotonic revision watermark after it applies.
    fn note_remote_applied_delta_replay(&self, key: &Option<RemoteDeltaReplayKey>) {
        if let Some(key) = key {
            self.remote_delta_replay_cache
                .lock()
                .expect("remote delta replay cache mutex poisoned")
                .insert(key.clone());
        }
    }

    fn fetch_remote_session_tail_target(
        &self,
        target: &RemoteSessionTarget,
        message_limit: usize,
        min_remote_revision: Option<u64>,
        delta_expectation: Option<RemoteDeltaHydrationExpectation>,
        request_timeout: Duration,
        expected_remote: Option<&RemoteConfig>,
        expected_connection: Option<&RemoteConnection>,
        expected_state_continuity_generation: Option<u64>,
    ) -> Result<SessionResponse, ApiError> {
        let query = vec![("tail".to_owned(), message_limit.to_string())];
        let (remote_response, response_lease): (SessionResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_timeout_and_lease(
            &target.remote,
            Method::GET,
            &format!(
                "/api/sessions/{}",
                encode_uri_component(&target.remote_session_id)
            ),
            &query,
            None,
            request_timeout,
        )?;

        let response_validation = (|| -> Result<(), ApiError> {
            if remote_response.session.id != target.remote_session_id {
                return Err(ApiError::bad_gateway(format!(
                "remote session response id `{}` did not match requested session `{}`",
                remote_response.session.id, target.remote_session_id
                )));
            }
            if remote_response.session.messages.len() > message_limit {
                return Err(ApiError::bad_gateway(format!(
                "remote session tail returned {} messages, exceeding requested limit {message_limit}",
                remote_response.session.messages.len()
                )));
            }
            let loaded_message_count =
                u32::try_from(remote_response.session.messages.len()).unwrap_or(u32::MAX);
            if loaded_message_count > remote_response.session.message_count {
                return Err(ApiError::bad_gateway(format!(
                "remote session tail length {loaded_message_count} exceeded messageCount {}",
                remote_response.session.message_count
                )));
            }
            if remote_response.session.messages_loaded
                != (loaded_message_count == remote_response.session.message_count)
            {
                return Err(ApiError::bad_gateway(
                    "remote session tail returned inconsistent messagesLoaded metadata",
                ));
            }
            if let Some(min_revision) = min_remote_revision {
                if remote_response.revision < min_revision {
                    return Err(ApiError::bad_gateway(format!(
                    "remote session response revision {} is older than required revision {min_revision}",
                    remote_response.revision
                    )));
                }
                if remote_response.revision > min_revision {
                    let metadata_matches_triggering_delta =
                        delta_expectation.is_some_and(|expectation| {
                            expectation.session_mutation_stamp.is_some()
                                && remote_response.session.message_count
                                    == expectation.message_count
                                && remote_response.session.session_mutation_stamp
                                    == expectation.session_mutation_stamp
                        });
                    if !metadata_matches_triggering_delta {
                        return Err(ApiError::bad_gateway(format!(
                        "remote session response revision {} is newer than targeted repair revision {min_revision} without matching session mutation metadata",
                        remote_response.revision
                        )));
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = response_validation {
            return Err(self.prefer_current_remote_response_error(&response_lease, error));
        }

        let (revision, session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(expected_remote) = expected_remote {
                self.ensure_remote_apply_authority_locked(
                    &inner,
                    expected_remote,
                    expected_connection,
                )?;
            }
            if let (Some(connection), Some(generation)) = (
                expected_connection,
                expected_state_continuity_generation,
            ) {
                connection.ensure_state_continuity_generation(generation)?;
            }
            self.ensure_remote_request_current_locked(&inner, &response_lease)?;
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to retry dirty remote session persistence: {err:#}"
                    ))
                })?;
            let index = inner
                .find_remote_session_index(&target.remote.id, &target.remote_session_id)
                .or_else(|| inner.find_session_index(&target.local_session_id))
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            if let Some(remote_revision) = min_remote_revision {
                if inner.should_skip_remote_session_applied_delta_revision(
                    &target.remote.id,
                    &target.remote_session_id,
                    remote_revision,
                ) {
                    let record = inner
                        .sessions
                        .get(index)
                        .ok_or_else(|| ApiError::not_found("session not found"))?;
                    return Ok(SessionResponse {
                        revision: inner.revision,
                        session: Self::wire_session_from_record(record),
                        server_instance_id: self.server_instance_id.clone(),
                    });
                }
            }
            let record = inner
                .sessions
                .get(index)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let latest_remote_revision = inner
                .remote_applied_revisions
                .get(&target.remote.id)
                .copied()
                .unwrap_or_default()
                .max(
                    inner
                        .remote_session_transcript_applied_revisions
                        .get(&target.remote.id)
                        .and_then(|sessions| sessions.get(&target.remote_session_id))
                        .copied()
                        .unwrap_or_default(),
                );
            let response_is_compatible_at_current_revision =
                match (
                    record.session.session_mutation_stamp,
                    remote_response.session.session_mutation_stamp,
                ) {
                    (Some(current_stamp), Some(response_stamp)) => {
                        response_stamp > current_stamp
                            || (response_stamp == current_stamp
                                && remote_response.session.message_count
                                    == record.session.message_count)
                    }
                    (Some(_), None) => false,
                    (None, _) => true,
                };
            if remote_response.revision < latest_remote_revision
                || (remote_response.revision == latest_remote_revision
                    && !response_is_compatible_at_current_revision)
            {
                return Err(ApiError::conflict(format!(
                    "remote session tail revision {} is stale relative to synchronized revision {latest_remote_revision}",
                    remote_response.revision
                )));
            }

            let local_project_ids_by_remote_project_id =
                remote_project_id_map(&inner, &target.remote.id);
            let local_project_id = local_project_id_for_remote_project(
                &local_project_ids_by_remote_project_id,
                remote_response.session.project_id.as_deref(),
            )
            .map(LocalProjectId::into_inner)
            .or_else(|| inner.sessions[index].session.project_id.clone());
            let session = {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                apply_remote_session_to_record(
                    record,
                    &target.remote.id,
                    local_project_id,
                    &remote_response.session,
                );
                Self::wire_session_from_record(record)
            };
            let bounded_tail_materialized = remote_response.session.message_count == 0
                || !remote_response.session.messages.is_empty();
            if bounded_tail_materialized {
                inner.note_remote_session_transcript_applied_revision(
                    &target.remote.id,
                    &target.remote_session_id,
                    remote_response.revision,
                );
            }
            if let Some(remote_revision) = min_remote_revision {
                inner.note_remote_applied_revision(&target.remote.id, remote_revision);
            }
            let revision = self.commit_remote_localization_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist bounded remote session tail: {err:#}"
                ))
            })?;
            (revision, session)
        };

        Ok(SessionResponse {
            revision,
            session,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn fetch_remote_session_history_target(
        &self,
        target: &RemoteSessionTarget,
        before: Option<&str>,
        after: Option<&str>,
        around: Option<usize>,
        from_start: bool,
        message_limit: usize,
        request_timeout: Duration,
    ) -> Result<SessionHistoryResponse, ApiError> {
        let mut query = vec![("limit".to_owned(), message_limit.to_string())];
        if let Some(before) = before {
            query.push(("before".to_owned(), before.to_owned()));
        }
        if let Some(after) = after {
            query.push(("after".to_owned(), after.to_owned()));
        }
        if let Some(around) = around {
            query.push(("around".to_owned(), around.to_string()));
        }
        if from_start {
            query.push(("from".to_owned(), "start".to_owned()));
        }
        let (remote_page, response_lease): (SessionHistoryResponse, RemoteRequestLease) =
            self.remote_registry.request_json_with_timeout_and_lease(
                &target.remote,
                Method::GET,
                &format!(
                    "/api/sessions/{}/history",
                    encode_uri_component(&target.remote_session_id)
                ),
                &query,
                None,
                request_timeout,
            )?;
        let response_validation = (|| -> Result<(), ApiError> {
            if remote_page.messages.len() > message_limit {
                return Err(ApiError::bad_gateway(format!(
                "remote session history returned {} messages, exceeding requested limit {message_limit}",
                remote_page.messages.len()
                )));
            }
            if remote_page.has_more != remote_page.next_before.is_some() {
                return Err(ApiError::bad_gateway(
                    "remote session history returned inconsistent cursor metadata",
                ));
            }
            if remote_page.has_newer != remote_page.next_after.is_some() {
                return Err(ApiError::bad_gateway(
                    "remote session history returned inconsistent forward cursor metadata",
                ));
            }
            if remote_page.has_newer
                && remote_page.next_after.as_deref()
                    != remote_page.messages.last().map(|message| message.id())
            {
                return Err(ApiError::bad_gateway(
                    "remote session history forward cursor did not match the last returned message",
                ));
            }
            if remote_page.has_more
                && remote_page.next_before.as_deref()
                    != remote_page.messages.first().map(|message| message.id())
            {
                return Err(ApiError::bad_gateway(
                    "remote session history cursor did not match the first returned message",
                ));
            }
            Ok(())
        })();
        if let Err(error) = response_validation {
            return Err(self.prefer_current_remote_response_error(&response_lease, error));
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_request_current_locked(&inner, &response_lease)?;
        let index = inner
            .find_remote_session_index(&target.remote.id, &target.remote_session_id)
            .or_else(|| inner.find_session_index(&target.local_session_id))
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .sessions
            .get(index)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let metadata_matches_current_session =
            remote_page.message_count == record.session.message_count
                && record.session.session_mutation_stamp.is_some()
                && Some(remote_page.session_mutation_stamp)
                    == record.session.session_mutation_stamp;
        let latest_remote_revision = inner
            .remote_applied_revisions
            .get(&target.remote.id)
            .copied()
            .unwrap_or_default();
        let compatible_without_remote_mutation_stamp =
            record.session.session_mutation_stamp.is_none()
                && remote_page.message_count == record.session.message_count
                && remote_page.revision >= latest_remote_revision;
        if !metadata_matches_current_session && !compatible_without_remote_mutation_stamp {
            return Err(ApiError::conflict(
                "remote session history changed while the page was loading; retry the request",
            ));
        }
        Ok(SessionHistoryResponse {
            messages: remote_page.messages,
            next_before: remote_page.next_before,
            has_more: remote_page.has_more,
            next_after: remote_page.next_after,
            has_newer: remote_page.has_newer,
            message_start_index: remote_page.message_start_index,
            message_count: remote_page.message_count,
            revision: inner.revision,
            session_mutation_stamp: inner.sessions[index].mutation_stamp,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    fn fetch_remote_session_overview_target(
        &self,
        target: &RemoteSessionTarget,
        bucket_count: usize,
        request_timeout: Duration,
    ) -> Result<SessionOverviewResponse, ApiError> {
        let query = vec![("buckets".to_owned(), bucket_count.to_string())];
        let (mut remote_overview, response_lease):
            (SessionOverviewResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_timeout_and_lease(
                &target.remote,
                Method::GET,
                &format!(
                    "/api/sessions/{}/overview",
                    encode_uri_component(&target.remote_session_id)
                ),
                &query,
                None,
                request_timeout,
            )?;
        let response_validation = (|| -> Result<(), ApiError> {
            if remote_overview.session_id != target.remote_session_id {
                return Err(ApiError::bad_gateway(format!(
                "remote session overview id `{}` did not match requested session `{}`",
                remote_overview.session_id, target.remote_session_id
                )));
            }
            if remote_overview.buckets.len() > bucket_count {
                return Err(ApiError::bad_gateway(format!(
                "remote session overview returned {} buckets, exceeding requested limit {bucket_count}",
                remote_overview.buckets.len()
                )));
            }
            let bucket_message_count: u64 = remote_overview
                .buckets
                .iter()
                .map(|bucket| u64::from(bucket.c))
                .sum();
            if bucket_message_count != u64::from(remote_overview.message_count) {
                return Err(ApiError::bad_gateway(
                    "remote session overview returned inconsistent bucket counts",
                ));
            }
            if remote_overview
                .buckets
                .iter()
                .any(|bucket| bucket.u > bucket.c)
            {
                return Err(ApiError::bad_gateway(
                    "remote session overview returned inconsistent author counts",
                ));
            }
            Ok(())
        })();
        if let Err(error) = response_validation {
            return Err(self.prefer_current_remote_response_error(&response_lease, error));
        }

        let inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_request_current_locked(&inner, &response_lease)?;
        let index = inner
            .find_remote_session_index(&target.remote.id, &target.remote_session_id)
            .or_else(|| inner.find_session_index(&target.local_session_id))
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .sessions
            .get(index)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if remote_overview.message_count != record.session.message_count
            || record.session.session_mutation_stamp.is_some_and(|stamp| {
                stamp != remote_overview.session_mutation_stamp
            })
        {
            return Err(ApiError::conflict(
                "remote session overview changed while loading; retry the request",
            ));
        }
        remote_overview.session_id = target.local_session_id.clone();
        Ok(remote_overview)
    }

    fn repair_remote_session_tail_after_delta_error(
        &self,
        remote_id: &str,
        event: &DeltaEvent,
    ) -> Result<bool, anyhow::Error> {
        self.repair_remote_session_tail_after_delta_error_with_authority(
            remote_id,
            None,
            None,
            event,
        )
    }

    fn repair_remote_session_tail_after_delta_error_for_bridge(
        &self,
        remote: &RemoteConfig,
        connection: &RemoteConnection,
        event: &DeltaEvent,
    ) -> Result<bool, anyhow::Error> {
        self.repair_remote_session_tail_after_delta_error_with_authority(
            &remote.id,
            Some(remote),
            Some(connection),
            event,
        )
    }

    fn repair_remote_session_tail_after_delta_error_with_authority(
        &self,
        remote_id: &str,
        expected_remote: Option<&RemoteConfig>,
        expected_connection: Option<&RemoteConnection>,
        event: &DeltaEvent,
    ) -> Result<bool, anyhow::Error> {
        let Some((remote_session_id, message_count, session_mutation_stamp)) =
            remote_delta_session_transcript_metadata(event)
        else {
            return Ok(false);
        };
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(expected_remote) = expected_remote {
                self.ensure_remote_apply_authority_locked(
                    &inner,
                    expected_remote,
                    expected_connection,
                )
                .map_err(|err| anyhow::Error::new(RemoteAuthorityApplyError(err)))?;
            }
            let Some(index) =
                inner.find_remote_session_index(remote_id, remote_session_id)
            else {
                return Ok(false);
            };
            let record = &inner.sessions[index];
            let remote = inner
                .find_remote(remote_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown remote `{remote_id}`"))?;
            RemoteSessionTarget {
                local_session_id: record.session.id.clone(),
                remote,
                remote_session_id: remote_session_id.to_owned(),
            }
        };
        self.fetch_remote_session_tail_target(
            &target,
            SESSION_TAIL_HYDRATION_MAX_MESSAGES,
            Some(delta_event_revision(event)),
            Some(RemoteDeltaHydrationExpectation {
                message_count,
                session_mutation_stamp,
            }),
            REMOTE_REQUEST_TIMEOUT,
            expected_remote,
            expected_connection,
            None,
        )
        .map_err(|err| {
            anyhow!(
                "failed to repair remote session `{remote_session_id}` from a bounded tail: {}",
                err.message
            )
        })?;
        Ok(true)
    }

    /// Ensures an unloaded remote proxy has one bounded tail before applying a
    /// narrow delta. It never asks either backend for an unbounded transcript.
    fn try_begin_remote_delta_hydration(
        &self,
        key: RemoteDeltaHydrationKey,
    ) -> Option<RemoteDeltaHydrationInFlightGuard> {
        let mut in_flight = self
            .remote_delta_hydrations_in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned");
        if !in_flight.insert(key.clone()) {
            return None;
        }
        Some(RemoteDeltaHydrationInFlightGuard {
            in_flight: self.remote_delta_hydrations_in_flight.clone(),
            key,
        })
    }

    fn hydrate_unloaded_remote_session_for_delta(
        &self,
        remote_id: &str,
        remote_session_id: &str,
        authority_generation: u64,
        remote_revision: u64,
        remote_message_count: u32,
        remote_session_mutation_stamp: Option<u64>,
        expected_remote: Option<&RemoteConfig>,
        expected_connection: Option<&RemoteConnection>,
        expected_state_continuity_generation: Option<u64>,
    ) -> Result<RemoteDeltaHydrationOutcome, anyhow::Error> {
        #[cfg(test)]
        self.remote_registry
            .run_test_before_remote_delta_hydration_target();
        let target = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(expected_remote) = expected_remote {
                self.ensure_remote_apply_authority_locked(
                    &inner,
                    expected_remote,
                    expected_connection,
                )
                .map_err(|err| anyhow::Error::new(RemoteAuthorityApplyError(err)))?;
            }
            if let (Some(connection), Some(generation)) = (
                expected_connection,
                expected_state_continuity_generation,
            ) {
                connection
                    .ensure_state_continuity_generation(generation)
                    .map_err(|err| anyhow::Error::new(RemoteAuthorityApplyError(err)))?;
            }
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)?;
            if inner.should_skip_remote_session_applied_delta_revision(
                remote_id,
                remote_session_id,
                remote_revision,
            ) {
                return Ok(RemoteDeltaHydrationOutcome::SkipApplied);
            }
            let Some(index) = inner.find_remote_session_index(remote_id, remote_session_id) else {
                return Ok(RemoteDeltaHydrationOutcome::Continue);
            };
            let record = &inner.sessions[index];
            if record.session.messages_loaded || !record.session.messages.is_empty() {
                return Ok(RemoteDeltaHydrationOutcome::Continue);
            }
            let remote = inner
                .find_remote(remote_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown remote `{remote_id}`"))?;
            RemoteSessionTarget {
                local_session_id: record.session.id.clone(),
                remote,
                remote_session_id: remote_session_id.to_owned(),
            }
        };

        let hydration_key = RemoteDeltaHydrationKey {
            remote_id: remote_id.to_owned(),
            remote_session_id: remote_session_id.to_owned(),
            authority_generation,
        };
        let Some(_hydration_guard) = self.try_begin_remote_delta_hydration(hydration_key) else {
            return Ok(RemoteDeltaHydrationOutcome::SkipInFlight);
        };

        let hydration_result = self.fetch_remote_session_tail_target(
            &target,
            SESSION_TAIL_HYDRATION_MAX_MESSAGES,
            Some(remote_revision),
            Some(RemoteDeltaHydrationExpectation {
                message_count: remote_message_count,
                session_mutation_stamp: remote_session_mutation_stamp,
            }),
            REMOTE_REQUEST_TIMEOUT,
            expected_remote,
            expected_connection,
            expected_state_continuity_generation,
        );
        match hydration_result {
            Ok(_) => {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let retained_tail_loaded = inner
                    .find_remote_session_index(remote_id, remote_session_id)
                    .and_then(|index| inner.sessions.get(index))
                    .is_some_and(|record| !record.session.messages.is_empty());
                if !retained_tail_loaded {
                    return Ok(RemoteDeltaHydrationOutcome::Continue);
                }
            }
            Err(err) if is_recoverable_remote_tail_miss(&err) => {
                return Ok(RemoteDeltaHydrationOutcome::Continue);
            }
            Err(err) => {
                return Err(anyhow!(
                    "failed to fetch bounded tail for remote session `{remote_session_id}`: {}",
                    err.message
                ));
            }
        }
        Ok(RemoteDeltaHydrationOutcome::SkipApplied)
    }

    fn should_skip_delta_after_remote_hydration(
        &self,
        outcome: RemoteDeltaHydrationOutcome,
        remote_delta_replay_key: &Option<RemoteDeltaReplayKey>,
    ) -> bool {
        match outcome {
            RemoteDeltaHydrationOutcome::Continue => false,
            RemoteDeltaHydrationOutcome::SkipApplied => {
                self.note_remote_applied_delta_replay(remote_delta_replay_key);
                true
            }
            RemoteDeltaHydrationOutcome::SkipInFlight => true,
        }
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
        let (remote_state, response_lease): (StateResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_lease(
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
        self.ensure_remote_request_current_locked(&inner, &response_lease)?;
        self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to retry dirty remote orchestrator state persistence: {err:#}"
                ))
            })?;
        if apply_remote_state_if_newer_locked(
            &mut inner,
            &target.remote.id,
            &remote_state,
            None,
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        ) {
            note_remote_applied_state_snapshot_revision(
                &mut inner,
                &target.remote.id,
                &remote_state,
            );
            self.commit_remote_localization_locked(&mut inner).map_err(|err| {
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
        self.apply_remote_state_snapshot_with_mode(
            remote_id,
            remote_state,
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        )
    }

    fn apply_remote_lagged_recovery_state_snapshot(
        &self,
        remote_id: &str,
        remote_state: StateResponse,
    ) -> Result<(), ApiError> {
        self.apply_remote_state_snapshot_with_mode(
            remote_id,
            remote_state,
            RemoteSnapshotApplyMode::ForceAfterLaggedEvent,
        )
    }

    fn apply_remote_state_snapshot_with_mode(
        &self,
        remote_id: &str,
        remote_state: StateResponse,
        mode: RemoteSnapshotApplyMode,
    ) -> Result<(), ApiError> {
        self.apply_remote_state_snapshot_with_authority(
            remote_id,
            None,
            None,
            None,
            remote_state,
            mode,
        )
    }

    fn apply_remote_state_snapshot_for_bridge(
        &self,
        remote: &RemoteConfig,
        connection: &RemoteConnection,
        remote_state: StateResponse,
        mode: RemoteSnapshotApplyMode,
    ) -> Result<(), ApiError> {
        self.apply_remote_state_snapshot_with_authority(
            &remote.id,
            Some(remote),
            Some(connection),
            None,
            remote_state,
            mode,
        )
    }

    fn apply_remote_state_snapshot_for_request(
        &self,
        lease: &RemoteRequestLease,
        remote_state: StateResponse,
        mode: RemoteSnapshotApplyMode,
    ) -> Result<(), ApiError> {
        self.apply_remote_state_snapshot_with_authority(
            &lease.pinned.id,
            Some(&lease.pinned),
            Some(&lease.connection),
            Some(lease.state_continuity_generation),
            remote_state,
            mode,
        )
    }

    fn apply_remote_state_snapshot_with_authority(
        &self,
        remote_id: &str,
        expected_remote: Option<&RemoteConfig>,
        expected_connection: Option<&RemoteConnection>,
        expected_state_continuity_generation: Option<u64>,
        remote_state: StateResponse,
        mode: RemoteSnapshotApplyMode,
    ) -> Result<(), ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(expected_remote) = expected_remote {
            self.ensure_remote_apply_authority_locked(
                &inner,
                expected_remote,
                expected_connection,
            )?;
        }
        if let (Some(connection), Some(generation)) = (
            expected_connection,
            expected_state_continuity_generation,
        ) {
            connection.ensure_state_continuity_generation(generation)?;
        }
        if !apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, mode) {
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to retry dirty remote state persistence: {err:#}"
                    ))
                })?;
            return Ok(());
        }
        note_remote_applied_state_snapshot_revision(&mut inner, remote_id, &remote_state);
        if let Err(err) = self.commit_remote_localization_locked(&mut inner) {
            return Err(ApiError::internal(format!(
                "failed to persist remote state: {err:#}"
            )));
        }
        Ok(())
    }
}
