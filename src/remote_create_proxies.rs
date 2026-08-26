// Remote create proxies — project / session / orchestrator creation
// against a remote backend.
//
// When a user creates a project, session, or orchestrator bound to a
// remote TermAl instance, the "real" record lives on the remote and
// this host only stores a thin local proxy that holds the
// `remote_*_id` fields needed to forward future requests. These
// three methods own the create-and-mirror flow:
//
// - `create_remote_project_proxy` — POSTs the project to the remote,
//   then persists a local `Project` carrying the returned
//   `remote_project_id`. Idempotent: if a local project already
//   points at the same remote root path, returns that one instead
//   of double-creating on the remote.
// - `create_remote_session_proxy` — POSTs a `CreateSessionRequest`
//   to the remote under a resolved `RemoteSessionTarget`, then
//   mirrors the returned `SessionResponse` into local state as a
//   proxy `SessionRecord`.
// - `create_remote_orchestrator_proxy` — analogous to session, for
//   orchestrator instances.
//
// The forward-call plumbing (`remote_get_json`, `remote_post_json`,
// `lookup_remote_config`, `ensure_remote_project_binding`) lives in
// `remote_routes.rs` and is shared with the proxy files below.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteCreateMissingProjectStatus {
    // Preserve each route's established unknown-project contract: session
    // creation reports 400 while orchestrator creation reports 404.
    BadRequest,
    NotFound,
}

impl RemoteCreateMissingProjectStatus {
    fn error(self, message: impl Into<String>) -> ApiError {
        match self {
            Self::BadRequest => ApiError::bad_request(message),
            Self::NotFound => ApiError::not_found(message),
        }
    }
}

impl AppState {
    fn remote_create_binding_rejection(
        &self,
        resolution: RemoteProjectBindingResolution,
        binding: &RemoteProjectBinding,
        response_lease: &RemoteRequestLease,
        missing_project_status: RemoteCreateMissingProjectStatus,
    ) -> ApiError {
        match resolution {
            RemoteProjectBindingResolution::Current => {
                ApiError::internal("current remote binding cannot be rejected")
            }
            RemoteProjectBindingResolution::ProjectMissing => {
                // The remote create already succeeded. Keep the bridge for the
                // still-current endpoint active so a later snapshot can expose
                // its remote-owned result without reviving the local project.
                if let Err(error) = self.start_remote_event_bridge_for_lease(response_lease) {
                    return remote_create_authority_error(error);
                }
                missing_project_status
                    .error(format!("unknown project `{}`", binding.local_project_id))
            }
            RemoteProjectBindingResolution::ProjectChanged => {
                if let Err(error) = self.start_remote_event_bridge_for_lease(response_lease) {
                    return remote_create_authority_error(error);
                }
                ApiError::conflict(REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE)
            }
            RemoteProjectBindingResolution::RemoteMissing => {
                ApiError::bad_request(format!("unknown remote `{}`", binding.remote.id))
            }
            RemoteProjectBindingResolution::RemoteChanged => {
                ApiError::conflict(REMOTE_CONNECTION_CHANGED_DURING_CREATE)
            }
        }
    }

    /// Reuses any existing local project already bound to the same remote
    /// root path (so repeated project creates are idempotent), otherwise
    /// posts to the remote, persists a local proxy `Project` carrying the
    /// returned `remote_project_id`, and returns both.
    fn create_remote_project_proxy(
        &self,
        request: CreateProjectRequest,
        remote: RemoteConfig,
        root_path: String,
    ) -> Result<CreateProjectResponse, ApiError> {
        let existing = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .projects
                .iter()
                .find(|project| project.remote_id == remote.id && project.root_path == root_path)
                .cloned()
        };
        if let Some(existing) = existing {
            if existing.remote_project_id.is_none() {
                let _ = self.ensure_remote_project_binding(&existing.id)?;
            }
            #[cfg(test)]
            self.remote_registry
                .run_test_before_existing_remote_project_revalidation();
            let (project_id, state) = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let current = inner
                    .find_project(&existing.id)
                    .filter(|project| {
                        project.remote_id == remote.id && project.root_path == root_path
                    })
                    .ok_or_else(|| {
                        ApiError::conflict(REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE)
                    })?;
                let project_id = current.id.clone();
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to retry remote project persistence: {err:#}"
                        ))
                    })?;
                (project_id, self.snapshot_from_inner(&inner))
            };
            return Ok(CreateProjectResponse {
                project_id,
                state,
            });
        }

        let (remote_response, response_lease): (CreateProjectResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_lease(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": request.name,
                "rootPath": root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )
        .map_err(remote_create_authority_error)?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        self.ensure_remote_create_request_current_locked(&inner, &response_lease)?;
        match resolve_remote_authority_locked(&inner, &remote) {
            RemoteAuthorityResolution::Current => {}
            RemoteAuthorityResolution::RemoteMissing => {
                return Err(ApiError::bad_request(format!(
                    "unknown remote `{}`",
                    remote.id
                )));
            }
            RemoteAuthorityResolution::RemoteChanged => {
                return Err(ApiError::conflict(
                    REMOTE_CONNECTION_CHANGED_DURING_CREATE,
                ));
            }
        }
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote.id.clone());
        let index = inner
            .projects
            .iter()
            .position(|candidate| candidate.id == project.id)
            .ok_or_else(|| ApiError::not_found(format!("unknown project `{}`", project.id)))?;
        let mut changed = inner.projects.len() != existing_len;
        if inner.projects[index]
            .remote_project_id
            .as_deref()
            .is_some_and(|existing| existing != remote_response.project_id)
        {
            // A concurrent request already won authority for this local
            // project. Keep its binding; never overwrite it with a later POST
            // response from a duplicate remote create.
            return Err(ApiError::conflict(
                REMOTE_PROJECT_BINDING_CHANGED_DURING_CREATE,
            ));
        }
        if inner.projects[index].remote_project_id.as_deref()
            != Some(remote_response.project_id.as_str())
        {
            inner.projects[index].remote_project_id = Some(remote_response.project_id.clone());
            changed = true;
        }
        if changed {
            self.commit_remote_localization_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        } else {
            self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to retry remote project persistence: {err:#}"
                    ))
                })?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    /// Posts a session create to the remote, upserts a local proxy
    /// `SessionRecord` pointing at the returned `remote_session_id`,
    /// and starts the event bridge so inbound deltas for this session
    /// begin streaming immediately. Non-session state slices
    /// (orchestrators, projects, sibling sessions) that changed on
    /// the remote during the create round-trip arrive via the SSE
    /// delta bridge rather than this response, since Node 1 of the
    /// type-safety plan dropped the `CreateSessionResponse.state`
    /// field. Returns the local session id.
    fn create_remote_session_proxy(
        &self,
        request: CreateSessionRequest,
        project: Project,
    ) -> Result<CreateSessionResponse, ApiError> {
        let Some(binding) = self.ensure_remote_project_binding_with_missing_status(
            &project.id,
            RemoteCreateMissingProjectStatus::BadRequest,
        )? else {
            return Err(ApiError::bad_request("remote project binding is missing"));
        };
        let (remote_response, response_lease): (CreateSessionResponse, RemoteRequestLease) = self
            .remote_registry
            .request_json_with_lease(
            &binding.remote,
            Method::POST,
            "/api/sessions",
            &[],
            Some(json!({
                "agent": request.agent,
                "name": request.name,
                "workdir": request.workdir,
                "projectId": binding.remote_project_id,
                "model": request.model,
                "approvalPolicy": request.approval_policy,
                "reasoningEffort": request.reasoning_effort,
                "sandboxMode": request.sandbox_mode,
                "cursorMode": request.cursor_mode,
                "claudeApprovalMode": request.claude_approval_mode,
                "claudeEffort": request.claude_effort,
                "geminiApprovalMode": request.gemini_approval_mode,
            })),
        )
        .map_err(remote_create_authority_error)?;
        // A response whose two identity fields disagree cannot be safely
        // attributed to the requested remote session. Reject it without
        // starting a bridge solely on the strength of that malformed payload.
        // Reject mismatched session identity on the wire. The wire
        // contract says `session.id === session_id`; if a malformed
        // remote returns otherwise, localizing `remote_session` would
        // mirror whichever id is embedded in `session.id` while
        // downstream code refers to the other id, silently opening a
        // proxy for the wrong remote session. Fail closed instead.
        if remote_response.session.id != remote_response.session_id {
            return Err(self.prefer_current_remote_create_response_error(
                &response_lease,
                ApiError::bad_gateway(
                    "remote session id mismatch: `session.id` does not equal `sessionId`",
                ),
            ));
        }
        // The remote-side session now exists. Claim its exact response
        // connection before local persistence so an internal localization
        // failure cannot strand that remote-owned result, while an A response
        // can never claim the current B endpoint after settings replacement.
        self.start_remote_event_bridge_for_lease(&response_lease)
            .map_err(remote_create_authority_error)?;
        let remote_session = remote_response.session.clone();
        let (revision, local_session_id, local_session, changed, delta_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            self.ensure_remote_create_request_current_locked(&inner, &response_lease)?;
            let resolution = resolve_remote_project_binding_locked(&inner, &binding);
            if resolution != RemoteProjectBindingResolution::Current {
                drop(inner);
                return Err(self.remote_create_binding_rejection(
                    resolution,
                    &binding,
                    &response_lease,
                    RemoteCreateMissingProjectStatus::BadRequest,
                ));
            }
            // Gate `update_existing` on the remote's applied-revision
            // tracking. If the SSE bridge already applied a later
            // remote revision for this remote (normal when a fork /
            // create races against active streaming), the POST
            // response's `session` payload is older than what we have
            // mirrored — refreshing would regress the bridged state.
            // If the POST response is at-or-newer-than the applied
            // remote revision, its payload is authoritative. New
            // proxy records (no existing row) still get created
            // regardless; this flag only controls the refresh branch.
            let update_existing = !inner
                .should_skip_remote_applied_revision(
                    &binding.remote.id,
                    remote_response.revision,
                );
            let (local_session_id, changed) = ensure_remote_proxy_session_record(
                &mut inner,
                &binding.remote.id,
                &remote_session,
                Some(binding.local_project_id),
                update_existing,
            );
            if update_existing {
                // When we refreshed from the POST, record its revision
                // as the most-recent-applied for this remote. Exact
                // same-revision payload replays are deduplicated by
                // the per-remote replay cache, not by this watermark.
                inner.note_remote_applied_revision(
                    &binding.remote.id,
                    remote_response.revision,
                );
            }
            let local_record = inner
                .find_session_index(&local_session_id)
                .and_then(|index| inner.sessions.get(index))
                .cloned()
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let local_session = AppState::wire_session_from_record(&local_record);
            let revision = if changed {
                self.commit_remote_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote session proxy: {err:#}"
                        ))
                    })?
            } else {
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to retry remote session proxy persistence: {err:#}"
                        ))
                    })?;
                inner.revision
            };
            let delta_session =
                changed.then(|| AppState::wire_session_summary_from_record(&local_record));
            (revision, local_session_id, local_session, changed, delta_session)
        };
        // Skip the SSE announcement on the no-change branch. The client
        // would silently drop it anyway (`decideDeltaRevisionAction`
        // ignores deltas whose revision `<= currentRevision`), but
        // emitting a same-revision `SessionCreated` is protocol-smell:
        // it advertises a mutation that did not happen. The returned
        // `session` + `revision` already reflect the bridge-mirrored
        // state the caller needs; peer clients are already in sync via
        // the earlier SSE delta that advanced `inner.revision`. Shared
        // with `remote_codex_proxies.rs::proxy_remote_fork_codex_thread`
        // via the helper below.
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

    /// Posts an orchestrator create to the remote, localizes the returned
    /// orchestrator + sessions into local proxy records, and starts the
    /// event bridge. Reports a specific 'upgrade required' error when the
    /// remote returns 404 and is known not to support inline templates.
    fn create_remote_orchestrator_proxy(
        &self,
        template: &OrchestratorTemplate,
        project: &Project,
    ) -> Result<CreateOrchestratorInstanceResponse, ApiError> {
        let Some(binding) = self.ensure_remote_project_binding_with_missing_status(
            &project.id,
            RemoteCreateMissingProjectStatus::NotFound,
        )? else {
            return Err(ApiError::bad_request("remote project binding is missing"));
        };
        let mut remote_template = orchestrator_template_to_draft(template);
        remote_template.project_id = Some(binding.remote_project_id.clone());
        let request_body = serde_json::to_value(CreateOrchestratorInstanceRequest {
            template_id: template.id.clone(),
            project_id: Some(binding.remote_project_id.clone()),
            template: Some(remote_template),
        })
        .map_err(|err| {
            ApiError::internal(format!(
                "failed to encode remote orchestrator create request: {err}"
            ))
        })?;
        let (remote_result, response_lease) = self.remote_registry.request_json_result_with_lease(
            &binding.remote,
            Method::POST,
            "/api/orchestrators",
            &[],
            Some(request_body),
        )
        .map_err(remote_create_authority_error)?;
        let remote_response: CreateOrchestratorInstanceResponse = match remote_result {
            Ok(response) => response,
            Err(err) if err.status == StatusCode::NOT_FOUND => {
                #[cfg(test)]
                self.remote_registry
                    .run_test_before_remote_orchestrator_capability_classification();
                let capability = self
                    .remote_registry
                    .cached_supports_inline_orchestrator_templates_for_lease(&response_lease)
                    .map_err(remote_create_authority_error)?;
                if !matches!(capability, Some(true)) {
                    return Err(ApiError::bad_gateway(format!(
                        "remote `{}` must be upgraded before it can launch local orchestrator templates",
                        binding.remote.name
                    )));
                }
                return Err(err);
            }
            Err(err) => return Err(remote_create_authority_error(err)),
        };
        // The remote-side orchestrator now exists. Claim the exact response
        // connection before local persistence for the same reason as remote
        // session creation.
        self.start_remote_event_bridge_for_lease(&response_lease)
            .map_err(remote_create_authority_error)?;
        let (state, local_orchestrator) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            self.ensure_remote_create_request_current_locked(&inner, &response_lease)?;
            // The remote create is intentionally not compensated on these
            // abort paths. Its result remains manageable on the remote, while
            // refusing local localization prevents stale project revival.
            let resolution = resolve_remote_project_binding_locked(&inner, &binding);
            if resolution != RemoteProjectBindingResolution::Current {
                drop(inner);
                return Err(self.remote_create_binding_rejection(
                    resolution,
                    &binding,
                    &response_lease,
                    RemoteCreateMissingProjectStatus::NotFound,
                ));
            }
            let applied_remote_revision = apply_remote_state_if_newer_locked(
                &mut inner,
                &binding.remote.id,
                &remote_response.state,
                None,
                RemoteSnapshotApplyMode::GateBySnapshotRevision,
            );
            let remote_sessions_by_id = remote_response
                .state
                .sessions
                .iter()
                .map(|session| (session.id.as_str(), session))
                .collect::<HashMap<_, _>>();
            let (local_orchestrator, changed) = match ensure_remote_orchestrator_instance(
                &mut inner,
                &binding.remote.id,
                &remote_response.orchestrator,
                Some(&remote_sessions_by_id),
                applied_remote_revision,
            ) {
                Ok(result) => result,
                Err(err) => {
                    return Err(ApiError::bad_gateway(format!(
                        "remote orchestrator could not be localized: {err}"
                    )));
                }
            };
            if applied_remote_revision {
                note_remote_applied_state_snapshot_revision(
                    &mut inner,
                    &binding.remote.id,
                    &remote_response.state,
                );
            }
            if applied_remote_revision || changed {
                self.commit_remote_localization_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist remote orchestrator proxy: {err:#}"
                    ))
                })?;
            } else {
                self.retry_remote_delta_persist_if_dirty_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to retry remote orchestrator proxy persistence: {err:#}"
                        ))
                    })?;
            }
            (self.snapshot_from_inner(&inner), local_orchestrator)
        };
        Ok(CreateOrchestratorInstanceResponse {
            orchestrator: local_orchestrator,
            state,
        })
    }
}
