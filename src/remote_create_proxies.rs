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

impl AppState {

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
            return Ok(CreateProjectResponse {
                project_id: existing.id,
                state: self.snapshot(),
            });
        }

        let remote_response: CreateProjectResponse = self.remote_registry.request_json(
            &remote,
            Method::POST,
            "/api/projects",
            &[],
            Some(json!({
                "name": request.name,
                "rootPath": root_path,
                "remoteId": LOCAL_REMOTE_ID,
            })),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote.id.clone());
        let index = inner
            .projects
            .iter()
            .position(|candidate| candidate.id == project.id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        let mut changed = inner.projects.len() != existing_len;
        if inner.projects[index].remote_project_id.as_deref()
            != Some(remote_response.project_id.as_str())
        {
            inner.projects[index].remote_project_id = Some(remote_response.project_id.clone());
            changed = true;
        }
        if changed {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
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
        let Some(binding) = self.ensure_remote_project_binding(&project.id)? else {
            return Err(ApiError::bad_request("remote project binding is missing"));
        };
        let remote_response: CreateSessionResponse = self.remote_registry.request_json(
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
        )?;
        self.remote_registry
            .start_event_bridge(self.clone(), &binding.remote);
        // Reject mismatched session identity on the wire. The wire
        // contract says `session.id === session_id`; if a malformed
        // remote returns otherwise, localizing `remote_session` would
        // mirror whichever id is embedded in `session.id` while
        // downstream code refers to the other id, silently opening a
        // proxy for the wrong remote session. Fail closed instead.
        if remote_response.session.id != remote_response.session_id {
            return Err(ApiError::bad_gateway(
                "remote session id mismatch: `session.id` does not equal `sessionId`",
            ));
        }
        let remote_session = remote_response.session.clone();
        let (revision, local_session_id, local_session) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
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
                // as the most-recent-applied for this remote so a
                // later delta at the same revision is correctly
                // recognized as a duplicate and ignored.
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
            let local_session = local_record.session.clone();
            let revision = if changed {
                self.commit_session_created_locked(&mut inner, &local_record)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to persist remote session proxy: {err:#}"
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
        let Some(binding) = self.ensure_remote_project_binding(&project.id)? else {
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
        let remote_response: CreateOrchestratorInstanceResponse = match self.remote_registry.request_json(
            &binding.remote,
            Method::POST,
            "/api/orchestrators",
            &[],
            Some(request_body),
        ) {
            Ok(response) => response,
            Err(err)
                if err.status == StatusCode::NOT_FOUND
                    && !matches!(
                        self.remote_registry
                            .cached_supports_inline_orchestrator_templates(&binding.remote),
                        Some(true)
                    ) =>
            {
                return Err(ApiError::bad_gateway(format!(
                    "remote `{}` must be upgraded before it can launch local orchestrator templates",
                    binding.remote.name
                )));
            }
            Err(err) => return Err(err),
        };
        let (state, local_orchestrator) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let applied_remote_revision = apply_remote_state_if_newer_locked(
                &mut inner,
                &binding.remote.id,
                &remote_response.state,
                None,
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
                inner.note_remote_applied_revision(
                    &binding.remote.id,
                    remote_response.state.revision,
                );
            }
            if applied_remote_revision || changed {
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist remote orchestrator proxy: {err:#}"
                    ))
                })?;
            }
            (self.snapshot_from_inner(&inner), local_orchestrator)
        };
        self.remote_registry
            .start_event_bridge(self.clone(), &binding.remote);

        Ok(CreateOrchestratorInstanceResponse {
            orchestrator: local_orchestrator,
            state,
        })
    }
}
