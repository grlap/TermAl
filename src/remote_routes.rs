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
    in_flight: Arc<Mutex<HashSet<(String, String)>>>,
    key: (String, String),
}

impl Drop for RemoteDeltaHydrationInFlightGuard {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .expect("remote delta hydration mutex poisoned")
            .remove(&self.key);
    }
}

const REMOTE_SESSION_RESPONSE_MISSING_FULL_TRANSCRIPT: &str =
    "remote session response did not include a full transcript";

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
                .ok_or_else(ApiError::local_session_missing)?;
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
            RemoteSnapshotApplyMode::GateBySnapshotRevision,
        ) {
            return Ok(());
        }
        inner.note_remote_applied_revision(&target.remote.id, remote_state.revision);
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist remote state: {err:#}"))
        })?;
        Ok(())
    }

    fn commit_applied_remote_state_before_rejection(
        &self,
        inner: &mut StateInner,
        remote_state_applied: bool,
        rejection_context: &str,
    ) -> Result<(), ApiError> {
        if !remote_state_applied {
            return Ok(());
        }

        self.commit_locked(inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist remote state before {rejection_context}: {err:#}"
            ))
        })?;
        Ok(())
    }

    /// Fetches the remote owner's full session transcript for a local proxy,
    /// localizes it into the proxy record, and returns the local full-session
    /// response shape. This keeps `/api/sessions/{id}` full-transcript-only
    /// even after metadata-first remote summaries create unloaded proxy records.
    /// Accepts metadata-light remotes that do not emit mutation stamps yet:
    /// when both stamps are `None`, `messageCount` is the only freshness
    /// evidence available and the caller has already confirmed broad remote
    /// state is newer than the candidate session response.
    fn remote_session_metadata_matches_record(record: &SessionRecord, session: &Session) -> bool {
        session_message_count(record) == session.message_count
            && record.session.session_mutation_stamp == session.session_mutation_stamp
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
    fn remote_delta_replay_key(
        remote_id: &str,
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
                delta,
                preview,
                session_mutation_stamp,
                ..
            } => RemoteDeltaReplayPayload::TextDelta {
                session_id: session_id.clone(),
                message_id: message_id.clone(),
                message_index: *message_index,
                message_count: *message_count,
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
            revision: delta_event_revision(event),
            payload,
        })
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

    fn hydrate_remote_session_target(
        &self,
        target: &RemoteSessionTarget,
        min_remote_revision: Option<u64>,
        delta_expectation: Option<RemoteDeltaHydrationExpectation>,
        // Applied independently to each remote round-trip below; this is not
        // a shared end-to-end budget across `/api/sessions` and `/api/state`.
        request_timeout: Duration,
    ) -> Result<SessionResponse, ApiError> {
        let remote_response: SessionResponse = self.remote_registry.request_json_with_timeout(
            &target.remote,
            Method::GET,
            &format!(
                "/api/sessions/{}",
                encode_uri_component(&target.remote_session_id)
            ),
            &[],
            None,
            request_timeout,
        )?;

        if remote_response.session.id != target.remote_session_id {
            return Err(ApiError::bad_gateway(format!(
                "remote session response id `{}` did not match requested session `{}`",
                remote_response.session.id, target.remote_session_id
            )));
        }
        if !remote_response.session.messages_loaded {
            return Err(
                ApiError::bad_gateway(REMOTE_SESSION_RESPONSE_MISSING_FULL_TRANSCRIPT)
                    .with_kind(ApiErrorKind::RemoteSessionMissingFullTranscript),
            );
        }
        let loaded_message_count =
            u32::try_from(remote_response.session.messages.len()).unwrap_or(u32::MAX);
        if loaded_message_count != remote_response.session.message_count {
            return Err(ApiError::bad_gateway(format!(
                "remote session response messageCount {} did not match loaded transcript length {}",
                remote_response.session.message_count, loaded_message_count
            )));
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
                            && remote_response.session.message_count == expectation.message_count
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

        let remote_state_for_full_hydration = if min_remote_revision.is_none() {
            let latest_remote_revision = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner
                    .remote_applied_revisions
                    .get(&target.remote.id)
                    .copied()
            };
            if latest_remote_revision
                .map(|revision| revision < remote_response.revision)
                .unwrap_or(true)
            {
                let remote_state: StateResponse = self.remote_registry.request_json_with_timeout(
                    &target.remote,
                    Method::GET,
                    "/api/state",
                    &[],
                    None,
                    request_timeout,
                )?;
                Some(remote_state)
            } else {
                None
            }
        } else {
            None
        };

        let (revision, session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut remote_state_applied = false;
            if let Some(remote_state) = remote_state_for_full_hydration.as_ref() {
                if remote_state.revision < remote_response.revision {
                    return Err(ApiError::bad_gateway(format!(
                        "remote state revision {} is older than remote session response revision {}",
                        remote_state.revision, remote_response.revision
                    )));
                }
                if apply_remote_state_if_newer_locked(
                    &mut inner,
                    &target.remote.id,
                    remote_state,
                    None,
                    RemoteSnapshotApplyMode::GateBySnapshotRevision,
                ) {
                    remote_state_applied = true;
                    note_remote_applied_state_snapshot_revision(
                        &mut inner,
                        &target.remote.id,
                        remote_state,
                    );
                }
            }

            let current_remote_revision = inner
                .remote_applied_revisions
                .get(&target.remote.id)
                .copied();
            if min_remote_revision.is_none() {
                if current_remote_revision
                    .is_none_or(|revision| revision < remote_response.revision)
                {
                    self.commit_applied_remote_state_before_rejection(
                        &mut inner,
                        remote_state_applied,
                        "stale session rejection",
                    )?;
                    let synchronized_revision = current_remote_revision
                        .map(|revision| revision.to_string())
                        .unwrap_or_else(|| "none".to_owned());
                    return Err(ApiError::bad_gateway(format!(
                        "remote session response revision {} cannot be safely applied; latest synchronized remote state revision is {synchronized_revision} and the transcript may have changed",
                        remote_response.revision,
                    ))
                    .with_kind(ApiErrorKind::RemoteSessionHydrationFreshnessRace));
                }
            }
            let Some(index) = inner
                .find_remote_session_index(&target.remote.id, &target.remote_session_id)
                .or_else(|| inner.find_session_index(&target.local_session_id))
            else {
                // Preserve the newer remote state fetched for this hydration even if the
                // requested proxy disappeared or was replaced before we localized the full
                // transcript.
                self.commit_applied_remote_state_before_rejection(
                    &mut inner,
                    remote_state_applied,
                    "missing-session rejection",
                )?;
                return Err(ApiError::not_found("session not found"));
            };
            if min_remote_revision.is_none()
                && current_remote_revision
                    .is_some_and(|revision| revision > remote_response.revision)
            {
                let record = inner
                    .sessions
                    .get(index)
                    .ok_or_else(|| ApiError::not_found("session not found"))?;
                if !Self::remote_session_metadata_matches_record(record, &remote_response.session) {
                    self.commit_applied_remote_state_before_rejection(
                        &mut inner,
                        remote_state_applied,
                        "stale session rejection",
                    )?;
                    return Err(ApiError::bad_gateway(format!(
                        "remote session response revision {} is older than synchronized remote state revision {} and does not match current session metadata",
                        remote_response.revision,
                        current_remote_revision.expect("checked as newer")
                    ))
                    .with_kind(ApiErrorKind::RemoteSessionHydrationFreshnessRace));
                }
            }
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
            if min_remote_revision.is_none() {
                inner.note_remote_session_transcript_applied_revision(
                    &target.remote.id,
                    &target.remote_session_id,
                    remote_response.revision,
                );
                inner.note_remote_applied_revision(&target.remote.id, remote_response.revision);
            } else if let Some(remote_revision) = min_remote_revision {
                // A targeted full-session repair materializes only this
                // transcript. Advance the session-specific transcript
                // watermark to the response revision so later deltas for this
                // session are skipped, but keep the broad remote watermark at
                // the triggering delta revision so unrelated sessions at
                // intermediate revisions can still apply.
                inner.note_remote_session_transcript_applied_revision(
                    &target.remote.id,
                    &target.remote_session_id,
                    remote_response.revision,
                );
                inner.note_remote_applied_revision(&target.remote.id, remote_revision);
            }
            let revision = self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist remote session hydration: {err:#}"
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

    /// Returns whether an unloaded remote proxy was repaired, is already being
    /// repaired by another delta, or still needs the caller to apply the narrow
    /// delta. `SkipInFlight` intentionally does not mark the delta replay key:
    /// the in-flight hydration has not proved this specific delta was applied.
    fn hydrate_unloaded_remote_session_for_delta(
        &self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
        remote_message_count: u32,
        remote_session_mutation_stamp: Option<u64>,
    ) -> Result<RemoteDeltaHydrationOutcome, anyhow::Error> {
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
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
            if record.session.messages_loaded {
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

        let hydration_key = (remote_id.to_owned(), remote_session_id.to_owned());
        let _hydration_guard = {
            let mut in_flight = self
                .remote_delta_hydrations_in_flight
                .lock()
                .expect("remote delta hydration mutex poisoned");
            if !in_flight.insert(hydration_key.clone()) {
                return Ok(RemoteDeltaHydrationOutcome::SkipInFlight);
            }
            RemoteDeltaHydrationInFlightGuard {
                in_flight: self.remote_delta_hydrations_in_flight.clone(),
                key: hydration_key,
            }
        };

        let hydration_result = self.hydrate_remote_session_target(
            &target,
            Some(remote_revision),
            Some(RemoteDeltaHydrationExpectation {
                message_count: remote_message_count,
                session_mutation_stamp: remote_session_mutation_stamp,
            }),
            REMOTE_REQUEST_TIMEOUT,
        );
        match hydration_result {
            Ok(_) => {}
            Err(err) if is_recoverable_remote_hydration_miss(&err) => {
                return Ok(RemoteDeltaHydrationOutcome::Continue);
            }
            Err(err) => {
                return Err(anyhow!(
                    "failed to hydrate remote session `{remote_session_id}`: {}",
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
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, mode) {
            return Ok(());
        }
        note_remote_applied_state_snapshot_revision(&mut inner, remote_id, &remote_state);
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
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                return Ok(());
            }
        }
        let remote_delta_replay_key = Self::remote_delta_replay_key(remote_id, &event);
        if self.should_skip_remote_applied_delta_replay(&remote_delta_replay_key) {
            return Ok(());
        }
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
                let Some((published_session_id, delta_session, revision)) = ({
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
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
                    let local_index =
                        inner.find_session_index(&local_session_id).ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                    if !changed {
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        None
                    } else {
                        let local_record =
                            inner.sessions.get(local_index).cloned().ok_or_else(|| {
                                anyhow!("local proxy session `{local_session_id}` not found")
                            })?;
                        let revision =
                            self.commit_session_created_locked(&mut inner, &local_record)?;
                        let local_record = inner.sessions.get(local_index).ok_or_else(|| {
                            anyhow!("local proxy session `{local_session_id}` not found")
                        })?;
                        let delta_session =
                            AppState::wire_session_summary_from_record(local_record);
                        let published_session_id = delta_session.id.clone();
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        Some((published_session_id, delta_session, revision))
                    }
                }) else {
                    self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                    return Ok(());
                };
                self.publish_delta(&DeltaEvent::SessionCreated {
                    revision,
                    session_id: published_session_id,
                    session: delta_session,
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::MessageCreated {
                message,
                message_count: remote_message_count,
                message_id,
                message_index,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                if message.id() != message_id {
                    return Err(anyhow!(
                        "remote created message payload id `{}` did not match event id `{message_id}`",
                        message.id()
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    applied_message_index,
                    revision,
                    message_count,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        applied_message_index,
                        message_count,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let applied_message_index = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let max_index_after_removal =
                                record.session.messages.len().saturating_sub(1);
                            if message_index > max_index_after_removal {
                                return Err(anyhow!(
                                    "remote MessageCreated index `{message_index}` is out of bounds for existing message `{message_id}` in session `{session_id}`"
                                ));
                            }
                            record.session.messages.remove(existing_index);
                            record
                                .session
                                .messages
                                .insert(message_index, message.clone());
                            record.message_positions =
                                build_message_positions(&record.session.messages);
                            message_index
                        } else {
                            if message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote MessageCreated index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
                            insert_message_on_record(record, message_index, message.clone())
                        };
                        record.session.preview = preview.clone();
                        record.session.status = status;
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            applied_message_index,
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        applied_message_index,
                        revision,
                        message_count,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::MessageCreated {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index: applied_message_index,
                    message_count,
                    message,
                    preview,
                    status,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::MessageUpdated {
                message,
                message_count: remote_message_count,
                message_id,
                message_index: _,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                }
                if message.id() != message_id {
                    return Err(anyhow!(
                        "remote updated message payload id `{}` did not match event id `{message_id}`",
                        message.id()
                    ));
                }
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        applied_message_index,
                        message_count,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let Some(applied_message_index) =
                            message_index_on_record(record, &message_id)
                        else {
                            return Err(anyhow!(
                                "remote MessageUpdated for unknown message `{message_id}` in session `{session_id}`"
                            ));
                        };
                        let existing_message = record
                            .session
                            .messages
                            .get_mut(applied_message_index)
                            .expect("message_index_on_record returned an out-of-bounds index");
                        *existing_message = message.clone();
                        record.session.preview = preview.clone();
                        record.session.status = status;
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            applied_message_index,
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    self.publish_delta(&DeltaEvent::MessageUpdated {
                        revision,
                        session_id: local_session_id,
                        message_id,
                        message_index: applied_message_index,
                        message_count,
                        message,
                        preview,
                        status,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::TextDelta {
                delta,
                message_count: remote_message_count,
                message_id,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    message_index,
                    message_count,
                    revision,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, message_index, message_count, session_mutation_stamp) = {
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
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            message_index,
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        message_index,
                        message_count,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::TextDelta {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message_count,
                    delta,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::TextReplace {
                message_count: remote_message_count,
                message_id,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                text,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    message_index,
                    message_count,
                    revision,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (local_session_id, message_index, message_count, session_mutation_stamp) = {
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
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            message_index,
                            session_message_count(record),
                            record.mutation_stamp,
                        )
                    };
                    let revision = self.commit_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        message_index,
                        message_count,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::TextReplace {
                    revision,
                    session_id: local_session_id,
                    message_id,
                    message_index,
                    message_count,
                    text,
                    preview,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::CommandUpdate {
                command,
                command_language,
                message_count: remote_message_count,
                message_id,
                message_index,
                output,
                output_language,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                status,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    created_message,
                    applied_message_index,
                    message_count,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        session_status,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let (created_message, applied_message_index) = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index)
                            else {
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
                                    (None, existing_index)
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a command message"
                                    ));
                                }
                            }
                        } else {
                            if message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote CommandUpdate index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
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
                            let applied_message_index =
                                insert_message_on_record(record, message_index, message.clone());
                            (Some(message), applied_message_index)
                        };
                        record.session.preview = preview.clone();
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            created_message,
                            applied_message_index,
                            session_message_count(record),
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
                        applied_message_index,
                        message_count,
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
                        message_index: applied_message_index,
                        message_count,
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
                        message_index: applied_message_index,
                        message_count,
                        command,
                        command_language,
                        output,
                        output_language,
                        status,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ParallelAgentsUpdate {
                agents,
                message_count: remote_message_count,
                message_id,
                message_index,
                preview,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                let hydration_outcome = self.hydrate_unloaded_remote_session_for_delta(
                    remote_id,
                    &session_id,
                    remote_revision,
                    remote_message_count,
                    remote_session_mutation_stamp,
                )?;
                if self.should_skip_delta_after_remote_hydration(
                    hydration_outcome,
                    &remote_delta_replay_key,
                ) {
                    return Ok(());
                }
                let (
                    local_session_id,
                    created_message,
                    applied_message_index,
                    message_count,
                    revision,
                    session_status,
                    session_mutation_stamp,
                ) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let (
                        local_session_id,
                        created_message,
                        applied_message_index,
                        message_count,
                        session_status,
                        session_mutation_stamp,
                    ) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let (created_message, applied_message_index) = if let Some(existing_index) =
                            message_index_on_record(record, &message_id)
                        {
                            let Some(message) = record.session.messages.get_mut(existing_index)
                            else {
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
                                    (None, existing_index)
                                }
                                _ => {
                                    return Err(anyhow!(
                                        "remote message `{message_id}` is not a parallel-agents message"
                                    ));
                                }
                            }
                        } else {
                            if message_index > record.session.messages.len() {
                                return Err(anyhow!(
                                    "remote ParallelAgentsUpdate index `{message_index}` leaves a gap in session `{session_id}`"
                                ));
                            }
                            let message = Message::ParallelAgents {
                                id: message_id.clone(),
                                timestamp: stamp_now(),
                                author: Author::Assistant,
                                agents: agents.clone(),
                            };
                            let applied_message_index =
                                insert_message_on_record(record, message_index, message.clone());
                            (Some(message), applied_message_index)
                        };
                        record.session.preview = preview.clone();
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (
                            record.session.id.clone(),
                            created_message,
                            applied_message_index,
                            session_message_count(record),
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
                        applied_message_index,
                        message_count,
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
                        message_index: applied_message_index,
                        message_count,
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
                        message_index: applied_message_index,
                        message_count,
                        agents,
                        preview,
                        session_mutation_stamp: Some(session_mutation_stamp),
                    });
                }
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerCreated {
                marker,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                if marker.session_id != session_id {
                    return Err(anyhow!(
                        "remote marker payload session id `{}` did not match event id `{session_id}`",
                        marker.session_id
                    ));
                }
                let (local_session_id, localized_marker, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let local_session_id = inner.sessions[index].session.id.clone();
                    let localized_marker =
                        localize_remote_conversation_marker(marker, &local_session_id).map_err(
                            |err| anyhow!("remote marker color was invalid: {}", err.message),
                        )?;
                    let session_mutation_stamp = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if let Some(existing_index) = record
                            .session
                            .markers
                            .iter()
                            .position(|entry| entry.id == localized_marker.id)
                        {
                            record.session.markers[existing_index] = localized_marker.clone();
                        } else {
                            record.session.markers.push(localized_marker.clone());
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        record.mutation_stamp
                    };
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        localized_marker,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerCreated {
                    revision,
                    session_id: local_session_id,
                    marker: localized_marker,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerUpdated {
                marker,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                if marker.session_id != session_id {
                    return Err(anyhow!(
                        "remote marker payload session id `{}` did not match event id `{session_id}`",
                        marker.session_id
                    ));
                }
                let (local_session_id, localized_marker, revision, session_mutation_stamp) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let local_session_id = inner.sessions[index].session.id.clone();
                    let localized_marker =
                        localize_remote_conversation_marker(marker, &local_session_id).map_err(
                            |err| anyhow!("remote marker color was invalid: {}", err.message),
                        )?;
                    let session_mutation_stamp = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        if let Some(existing_index) = record
                            .session
                            .markers
                            .iter()
                            .position(|entry| entry.id == localized_marker.id)
                        {
                            record.session.markers[existing_index] = localized_marker.clone();
                        } else {
                            record.session.markers.push(localized_marker.clone());
                        }
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        record.mutation_stamp
                    };
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    (
                        local_session_id,
                        localized_marker,
                        revision,
                        session_mutation_stamp,
                    )
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerUpdated {
                    revision,
                    session_id: local_session_id,
                    marker: localized_marker,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::ConversationMarkerDeleted {
                marker_id,
                session_id,
                session_mutation_stamp: remote_session_mutation_stamp,
                ..
            } => {
                let Some((local_session_id, revision, session_mutation_stamp)) = ({
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.should_skip_remote_session_applied_delta_revision(
                        remote_id,
                        &session_id,
                        remote_revision,
                    ) {
                        return Ok(());
                    }
                    let index = inner
                        .find_remote_session_index(remote_id, &session_id)
                        .ok_or_else(|| anyhow!("remote session `{session_id}` not found"))?;
                    let existing_index = inner.sessions[index]
                        .session
                        .markers
                        .iter()
                        .position(|entry| entry.id == marker_id);
                    let Some(existing_index) = existing_index else {
                        inner.note_remote_applied_revision(remote_id, remote_revision);
                        drop(inner);
                        self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                        return Ok(());
                    };
                    let (local_session_id, session_mutation_stamp) = {
                        let record = inner
                            .session_mut_by_index(index)
                            .expect("session index should be valid");
                        let local_session_id = record.session.id.clone();
                        record.session.markers.remove(existing_index);
                        if remote_session_mutation_stamp.is_some() {
                            record.session.session_mutation_stamp = remote_session_mutation_stamp;
                        }
                        (local_session_id, record.mutation_stamp)
                    };
                    inner.note_remote_applied_revision(remote_id, remote_revision);
                    let revision = self.commit_persisted_delta_locked(&mut inner)?;
                    Some((local_session_id, revision, session_mutation_stamp))
                }) else {
                    self.note_remote_applied_delta_replay(&remote_delta_replay_key);
                    return Ok(());
                };
                self.publish_delta(&DeltaEvent::ConversationMarkerDeleted {
                    revision,
                    session_id: local_session_id,
                    marker_id,
                    session_mutation_stamp: Some(session_mutation_stamp),
                });
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
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
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::CodexUpdated {
                revision: _,
                codex: _,
            } => {
                // CodexState is process-global runtime metadata, not localized
                // remote proxy state. Mark the remote revision consumed for
                // monotonicity, but intentionally do not fold the Codex payload
                // into local state; this watermark means "consumed" for this
                // informational variant, not "reflected in the proxy model".
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                    return Ok(());
                }
                inner.note_remote_applied_revision(remote_id, remote_revision);
                drop(inner);
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
            DeltaEvent::DelegationCreated { .. }
            | DeltaEvent::DelegationWaitCreated { .. }
            | DeltaEvent::DelegationWaitConsumed { .. }
            | DeltaEvent::DelegationWaitResumeDispatchFailed { .. }
            | DeltaEvent::DelegationUpdated { .. }
            | DeltaEvent::DelegationCompleted { .. }
            | DeltaEvent::DelegationFailed { .. }
            | DeltaEvent::DelegationCanceled { .. } => {
                // Delegations are local parent/child session relationships.
                // Cross-machine delegation is a non-goal for this phase, so
                // consume the remote revision without mirroring the payload.
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if inner.should_skip_remote_applied_delta_revision(remote_id, remote_revision) {
                    return Ok(());
                }
                inner.note_remote_applied_revision(remote_id, remote_revision);
                drop(inner);
                self.note_remote_applied_delta_replay(&remote_delta_replay_key);
            }
        }
        Ok(())
    }
}
