// Top-level creation/configuration CRUD for `AppState`: sessions,
// projects, and the global `AppSettings`.
//
// This file is the "birth" half of the session lifecycle. The "death"
// half — kill/cancel/stop — lives in `session_lifecycle.rs`; the
// run-and-broadcast half lives in `session_messages.rs` and
// `turn_lifecycle.rs`. A new session starts here at `create_session`,
// is promoted from a hidden Claude spare (or freshly reserved) if one
// matches, and lands in `StateInner.sessions` with
// `SessionRuntime::None` — the real per-session runtime is spawned
// lazily on the first prompt through `dispatch_turn`
// (`turn_dispatch.rs`). The one subprocess that *is* spawned here is
// the replenishment hidden Claude spare for the next session of the
// same shape (see `claude_spares.rs`).
//
// `create_session` flow: resolve the requested workdir (explicit →
// project default → global default); pick the agent (respecting
// cross-family runtime restrictions); pre-refresh the
// `cached_agent_readiness` snapshot *before* taking the state lock so
// `commit_session_created_locked`'s broadcast carries fresh data;
// under the lock, try to claim a matching hidden Claude spare from
// `StateInner.sessions` or allocate a new hidden spare placeholder
// and promote it; for remote-backed projects, forward to the remote
// `create_remote_session_proxy` path; publish via
// `commit_session_created_locked` (emits a SessionCreated delta +
// bumps the revision). Outside the lock, if a Claude spare was
// consumed, replenish the pool by calling
// `try_start_hidden_claude_spare` for the new spare-placeholder so
// the *next* create in this shape is just as fast.
//
// `update_app_settings` updates the user's global defaults (default
// agent, model, approval policy, cursor mode, and a few bookmark /
// UI preferences). It invalidates the agent-readiness cache up front
// because the "allowed agents" set can shift and sessions following
// defaults must see the change. Settings are split into "sticky" vs
// "default": sticky values are the hard preference (applied now and
// followed going forward); default values are suggestions only
// consumed by future `create_session` calls.
//
// `create_project` + `delete_project`: Projects are named bundles of
// workdir + remote + per-project default settings. Creating a remote-
// backed project delegates to `create_remote_project_proxy`; local
// projects normalize the workdir path. Deleting a project does NOT
// cascade into its sessions — existing sessions keep their absolute
// workdirs but their `project_id` field is cleared (via
// `session_mut_by_index` so `mutation_stamp` bumps persist the
// change). Orchestrator instances that reference the project also
// have their `project_id` cleared for the same reason.

impl AppState {
    /// Creates a new session (local or remote-backed) from a
    /// `CreateSessionRequest`, persists it, and broadcasts the
    /// SessionCreated delta.
    ///
    /// Workdir resolution order is: explicit `request.workdir` →
    /// the project's default workdir (if `project_id` was given) →
    /// the global `AppSettings.default_workdir`. The selected agent
    /// defaults to `Agent::Codex` but can be overridden by the
    /// request; cross-family guards reject combinations like "ACP
    /// agent with Codex-only reasoning effort" before we commit.
    ///
    /// For remote-backed projects we short-circuit to
    /// [`Self::create_remote_session_proxy`] so the real record lives
    /// on the remote and this host only stores a proxy shell. For
    /// local projects we refresh the agent-readiness cache before
    /// taking the state lock (so the broadcast carries fresh data
    /// without doing filesystem I/O under the lock), then try to
    /// claim a matching hidden Claude spare via
    /// `find_matching_hidden_claude_spare`. If one is claimed, its
    /// warmed runtime handle and `RuntimeToken` carry over to the new
    /// visible session; otherwise a new record lands in
    /// `StateInner.sessions` with `SessionRuntime::None` and the real
    /// runtime is spawned lazily on the first prompt through
    /// `turn_dispatch.rs::dispatch_turn`.
    ///
    /// Finally we replenish the hidden-spare pool (outside the lock,
    /// via [`Self::try_start_hidden_claude_spare`]) so the next
    /// create for this `(workdir, project, model, approval_mode,
    /// effort)` tuple stays instant.
    fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        let agent = request.agent.unwrap_or(Agent::Codex);
        let requested_workdir = request
            .workdir
            .as_deref()
            .map(resolve_session_workdir)
            .transpose()?;
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let requested_name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(project_id) = request.project_id.as_deref() {
                Some(inner.find_project(project_id).cloned().ok_or_else(|| {
                    ApiError::bad_request(format!("unknown project `{project_id}`"))
                })?)
            } else {
                requested_workdir
                    .as_deref()
                    .and_then(|workdir| inner.find_project_for_workdir(workdir).cloned())
            }
        };
        let workdir = requested_workdir.unwrap_or_else(|| {
            project
                .as_ref()
                .map(|entry| entry.root_path.clone())
                .unwrap_or_else(|| self.default_workdir.clone())
        });
        if let Some(project) = project.as_ref() {
            if project.remote_id != LOCAL_REMOTE_ID {
                return self.create_remote_session_proxy(request, project.clone());
            }
            if !path_contains(&project.root_path, FsPath::new(&workdir)) {
                return Err(ApiError::bad_request(format!(
                    "session workdir `{workdir}` must stay inside project `{}`",
                    project.name
                )));
            }
        }
        validate_agent_session_setup(agent, &workdir).map_err(ApiError::bad_request)?;
        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();
        match agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Codex sessions only support model, sandbox, approval policy, and reasoning effort settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model, mode, and effort settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support approval mode settings",
                    ));
                }
            }
            _ => {}
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let project_id = project.as_ref().map(|entry| entry.id.clone());
        let mut hidden_claude_spare_to_spawn = None;
        let mut record = if agent == Agent::Claude {
            let final_model = requested_model
                .clone()
                .unwrap_or_else(|| agent.default_model().to_owned());
            let final_approval_mode = request
                .claude_approval_mode
                .unwrap_or(inner.preferences.default_claude_approval_mode);
            let final_effort = request
                .claude_effort
                .unwrap_or(inner.preferences.default_claude_effort);
            if let Some(index) = inner.find_matching_hidden_claude_spare(
                &workdir,
                project_id.as_deref(),
                &final_model,
                final_approval_mode,
                final_effort,
            ) {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                // Hidden Claude spares intentionally keep their warmed runtime alive when claimed.
                // Only the visible conversation state is reset here before the session is unhidden.
                reset_hidden_claude_spare_record(record);
                record.hidden = false;
                if let Some(name) = requested_name.clone() {
                    record.session.name = name;
                }
                record.clone()
            } else {
                inner.create_session(
                    agent,
                    requested_name.clone(),
                    workdir.clone(),
                    project_id.clone(),
                    requested_model.clone(),
                )
            }
        } else {
            inner.create_session(
                agent,
                requested_name.clone(),
                workdir.clone(),
                project_id.clone(),
                requested_model.clone(),
            )
        };
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(sandbox_mode) = request.sandbox_mode {
                record.codex_sandbox_mode = sandbox_mode;
                record.session.sandbox_mode = Some(sandbox_mode);
            }
            if let Some(approval_policy) = request.approval_policy {
                record.codex_approval_policy = approval_policy;
                record.session.approval_policy = Some(approval_policy);
            }
            if let Some(reasoning_effort) = request.reasoning_effort {
                record.codex_reasoning_effort = reasoning_effort;
                record.session.reasoning_effort = Some(reasoning_effort);
            }
        } else if record.session.agent.supports_claude_approval_mode() {
            if let Some(claude_approval_mode) = request.claude_approval_mode {
                record.session.claude_approval_mode = Some(claude_approval_mode);
            }
            if let Some(claude_effort) = request.claude_effort {
                record.session.claude_effort = Some(claude_effort);
            }
        } else if record.session.agent.supports_cursor_mode() {
            if let Some(cursor_mode) = request.cursor_mode {
                record.session.cursor_mode = Some(cursor_mode);
            }
        } else if record.session.agent.supports_gemini_approval_mode() {
            if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                record.session.gemini_approval_mode = Some(gemini_approval_mode);
            }
        }
        if agent == Agent::Claude {
            hidden_claude_spare_to_spawn = inner.ensure_hidden_claude_spare(
                workdir.clone(),
                project_id.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
            );
        }
        if let Some(index) = inner.find_session_index(&record.session.id) {
            if let Some(slot) = inner.sessions.get_mut(index) {
                *slot = record.clone();
            }
            // The whole-struct replace above clobbered the stamp that
            // `push_session` assigned; re-stamp via `session_mut_by_index`
            // so `collect_persist_delta` picks up this rewrite on the
            // next persist tick. The local `record` carries
            // `mutation_stamp: 0` from construction, so skipping this
            // call would leave the row below the persist watermark.
            let _ = inner.session_mut_by_index(index);
        }
        let revision = self.commit_session_created_locked(&mut inner, &record)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        let session = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get(index))
            .map(AppState::wire_session_from_record)
            .unwrap_or_else(|| AppState::wire_session_from_record(&record));
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session.id.clone(),
            session: session.clone(),
        });
        if let Some(session_id) = hidden_claude_spare_to_spawn {
            self.try_start_hidden_claude_spare(&session_id);
        }
        Ok(CreateSessionResponse {
            session_id: session.id.clone(),
            session,
            revision,
            server_instance_id: self.server_instance_id.clone(),
        })
    }

    /// Updates app settings.
    /// Updates the user's global `AppSettings` (default agent/model,
    /// approval policy, cursor mode, bookmarks, etc.) and broadcasts.
    ///
    /// Some of these fields feed the per-session defaults used by
    /// future `create_session` calls; others flip hard behaviour
    /// immediately (for example: which agents to probe during the
    /// readiness scan). The agent-readiness cache is invalidated up
    /// front before taking the state lock so subsequent commits
    /// pick up the new scan shape. Sticky fields (values applied to
    /// existing sessions) vs default fields (values consumed only by
    /// future `create_session` calls) are distinguished by
    /// `persisted_state::AppSettings` field semantics — this method
    /// only writes the settings bag; session propagation happens
    /// lazily as individual sessions pull defaults.
    ///
    /// Settings mutations commit through [`Self::commit_locked`] so
    /// the SSE channel gets a full state snapshot — settings changes
    /// touch many UI surfaces at once and a delta event would be
    /// awkward to fan out reliably.
    fn update_app_settings(
        &self,
        request: UpdateAppSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        // Normalize remotes outside the lock — pure validation on request data.
        let normalized_remotes = request.remotes.map(normalize_remote_configs).transpose()?;

        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = false;

        if let Some(default_codex_reasoning_effort) = request.default_codex_reasoning_effort {
            if inner.preferences.default_codex_reasoning_effort != default_codex_reasoning_effort {
                inner.preferences.default_codex_reasoning_effort = default_codex_reasoning_effort;
                changed = true;
            }
        }

        if let Some(default_claude_approval_mode) = request.default_claude_approval_mode {
            if inner.preferences.default_claude_approval_mode != default_claude_approval_mode {
                inner.preferences.default_claude_approval_mode = default_claude_approval_mode;
                changed = true;
            }
        }

        if let Some(default_claude_effort) = request.default_claude_effort {
            if inner.preferences.default_claude_effort != default_claude_effort {
                inner.preferences.default_claude_effort = default_claude_effort;
                changed = true;
            }
        }

        let mut next_remotes: Option<Vec<RemoteConfig>> = None;
        if let Some(normalized_remotes) = normalized_remotes {
            let next_remote_ids: HashSet<&str> = normalized_remotes
                .iter()
                .map(|remote| remote.id.as_str())
                .collect();
            if let Some(project) = inner
                .projects
                .iter()
                .find(|project| !next_remote_ids.contains(project.remote_id.as_str()))
            {
                return Err(ApiError::bad_request(format!(
                    "cannot remove remote `{}` because project `{}` still uses it",
                    project.remote_id, project.name
                )));
            }
            if inner.preferences.remotes != normalized_remotes {
                inner.preferences.remotes = normalized_remotes.clone();
                next_remotes = Some(normalized_remotes);
                changed = true;
            }
        }

        if changed {
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist app settings: {err:#}"))
            })?;
        }

        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);
        if let Some(remotes) = next_remotes {
            let changed_ids = self.remote_registry.reconcile(&remotes);
            // Clear revision watermarks synchronously so the first response
            // from a newly pointed/restarted remote is not dropped as stale.
            for remote_id in &changed_ids {
                self.clear_remote_applied_revision(remote_id);
                self.clear_remote_sse_fallback_resync(remote_id);
            }
        }
        Ok(snapshot)
    }

    /// Creates a new Project entry (a named bundle of workdir + remote
    /// + per-project default settings).
    ///
    /// Remote-backed projects (those with a resolvable `remote_id` on
    /// the request) delegate to [`Self::create_remote_project_proxy`] so
    /// the real project record lives on the remote and this host only
    /// stores a proxy shell. Local projects normalize the workdir path
    /// through `resolve_session_workdir` and commit only if the
    /// `projects` vec actually grew — idempotent re-creates with the
    /// same id no-op rather than broadcasting redundant state.
    fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let remote_id = if request.remote_id.trim().is_empty() {
            default_local_remote_id()
        } else {
            request.remote_id.trim().to_owned()
        };
        let remote = inner
            .find_remote(&remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))?;
        let trimmed_root_path = request.root_path.trim();
        if trimmed_root_path.is_empty() {
            return Err(ApiError::bad_request("project root path cannot be empty"));
        }
        let root_path = if matches!(remote.transport, RemoteTransport::Local) {
            resolve_project_root_path(trimmed_root_path)?
        } else {
            trimmed_root_path.to_owned()
        };
        if !remote.enabled {
            return Err(ApiError::bad_request(format!(
                "remote `{}` is disabled",
                remote.name
            )));
        }
        if remote_id != LOCAL_REMOTE_ID {
            drop(inner);
            return self.create_remote_project_proxy(request, remote, root_path);
        }
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote_id);
        if inner.projects.len() != existing_len {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    /// Deletes the local project reference and keeps its sessions visible
    /// outside project scope. Remote-backed projects are intentionally removed
    /// only from this local state; TermAl does not delete remote project data
    /// from a local project-list action.
    fn delete_project(&self, project_id: &str) -> Result<StateResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(project_index) = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return Err(ApiError::not_found("project not found"));
        };

        inner.projects.remove(project_index);
        // Collect affected indices first so the mutating pass can go
        // through `session_mut_by_index` (which bumps `mutation_stamp`).
        // Iterating `&mut inner.sessions` directly would clear the
        // `project_id` in memory but skip the stamp, causing
        // `collect_persist_delta` to drop these changes — the deleted
        // project would reappear attached to those sessions on restart.
        let affected_session_indices: Vec<usize> = inner
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, record)| {
                if record.session.project_id.as_deref() == Some(project_id) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        for idx in affected_session_indices {
            if let Some(record) = inner.session_mut_by_index(idx) {
                record.session.project_id = None;
            }
        }
        for instance in &mut inner.orchestrator_instances {
            if instance.project_id == project_id {
                instance.project_id.clear();
            }
        }

        self.commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to remove project: {err:#}")))?;
        Ok(self.snapshot_from_inner(&inner))
    }
}
